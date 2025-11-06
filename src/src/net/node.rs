use crate::{
    config::{Config, PeerEntry},
    net::{ntp, proto::*},
    phc::reader::PhcReader,
    store::redis::RedisStore,
    time::{
        calc::compute_offset_delay,
        drift::linreg_slope_ppm,
        fuse::{peer_score, select_best_peers, PeerSample, TrimmedStats, WeightedEwma},
        slew,
    },
};
use anyhow::{Context, Result};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::{
    net::UdpSocket,
    sync::Mutex,
    time::{interval, Duration},
};
use tracing::{debug, error, info, warn};

const RESPONSE_TIMEOUT_MS: u64 = 400;
const MAX_PACKET: usize = 512;

#[derive(Clone)]
struct PendingRequest {
    peer_id: u32,
    t1_ns: i128,
    sent_at: Instant,
}

#[derive(Default, Clone)]
struct PeerRuntime {
    last_delay_ns: Option<f64>,
    last_offset_ns: Option<f64>,
    last_jitter_ns: Option<f64>,
}

pub async fn run_node(cfg: Config, redis: RedisStore, phc: PhcReader) -> Result<()> {
    let sock = Arc::new(
        UdpSocket::bind(&cfg.listen_addr)
            .await
            .with_context(|| format!("failed to bind UDP socket on {}", cfg.listen_addr))?,
    );
    info!("peer {} listening on {}", cfg.node_id, cfg.listen_addr);

    let cfg = Arc::new(cfg);
    let peers: Arc<Vec<PeerEntry>> = Arc::new(cfg.peers.clone());
    let peer_lookup: Arc<HashMap<u32, PeerEntry>> =
        Arc::new(peers.iter().map(|p| (p.id, p.clone())).collect());
    let ntp_ids: Arc<HashSet<u32>> = Arc::new(cfg.ntp_sources.iter().map(|s| s.id).collect());

    ntp::spawn_ntp_pollers(cfg.clone(), redis.clone(), phc.clone());

    let pending: Arc<Mutex<HashMap<u32, PendingRequest>>> = Arc::new(Mutex::new(HashMap::new()));
    let runtime: Arc<Mutex<HashMap<u32, PeerRuntime>>> = Arc::new(Mutex::new(HashMap::new()));
    let seq = Arc::new(AtomicU32::new(1));

    // Combined RX loop – handles requests and responses.
    let rx_loop = {
        let sock = sock.clone();
        let cfg = cfg.clone();
        let redis = redis.clone();
        let phc = phc.clone();
        let pending = pending.clone();
        let runtime = runtime.clone();

        tokio::spawn(async move {
            let mut buf = [0u8; MAX_PACKET];
            loop {
                let (n, addr) = match sock.recv_from(&mut buf).await {
                    Ok(pair) => pair,
                    Err(err) => {
                        error!("udp recv error: {}", err);
                        continue;
                    }
                };
                if n < 2 {
                    continue;
                }
                match buf[1] {
                    MSG_REQ => {
                        if let Err(err) =
                            handle_request(&buf[..n], addr, &sock, &cfg, &phc, &redis).await
                        {
                            debug!("handle_request failed: {err}");
                        }
                    }
                    MSG_RESP => {
                        if let Err(err) =
                            handle_response(&buf[..n], addr, &cfg, &phc, &redis, &pending, &runtime)
                                .await
                        {
                            debug!("handle_response failed: {err}");
                        }
                    }
                    _ => continue,
                }
            }
        })
    };

    // Client loop – emits probes in steady cadence.
    let tx_loop = {
        let cfg = cfg.clone();
        let sock = sock.clone();
        let peers = peers.clone();
        let redis = redis.clone();
        let phc = phc.clone();
        let pending = pending.clone();
        let seq = seq.clone();

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(cfg.sampling.interval_ms.max(1)));
            let burst = cfg.sampling.burst.unwrap_or(1).max(1);
            loop {
                ticker.tick().await;
                if cfg.phc_device.is_none() {
                    continue;
                }
                for peer in peers.iter().filter(|p| p.id != cfg.node_id) {
                    let addr: SocketAddr = match format!("{}:{}", peer.ip, peer.port).parse() {
                        Ok(addr) => addr,
                        Err(err) => {
                            warn!("invalid socket addr for peer {}: {err}", peer.id);
                            continue;
                        }
                    };
                    for _ in 0..burst {
                        let seq_id = seq.fetch_add(1, Ordering::SeqCst);
                        let t1 = match phc.now_ns() {
                            Ok(ns) => ns,
                            Err(err) => {
                                warn!("time read failed (t1): {err}");
                                continue;
                            }
                        };
                        let req = TimeRequest {
                            header: Header::new(MSG_REQ, seq_id),
                            sender_id: cfg.node_id,
                            t1_send_ns: t1,
                            reserved: [0; RESERVED_BYTES],
                            signature: [0; SIGNATURE_BYTES],
                        };
                        let payload = match bincode::serialize(&req) {
                            Ok(buf) => buf,
                            Err(err) => {
                                warn!("serialize request failed: {err}");
                                continue;
                            }
                        };
                        if let Err(err) = sock.send_to(&payload, addr).await {
                            warn!("send_to {} failed: {}", addr, err);
                            continue;
                        }
                        let mut guard = pending.lock().await;
                        guard.insert(
                            seq_id,
                            PendingRequest {
                                peer_id: peer.id,
                                t1_ns: t1 as i128,
                                sent_at: Instant::now(),
                            },
                        );
                    }
                }

                cleanup_timeouts(
                    &pending,
                    &redis,
                    cfg.sampling.window_sec,
                    Duration::from_millis(RESPONSE_TIMEOUT_MS),
                )
                .await;
            }
        })
    };

    // Fusion loop – consensus, drift estimation, optional control.
    let fusion_loop = {
        let cfg = cfg.clone();
        let redis = redis.clone();
        let phc = phc.clone();
        let peers = peers.clone();
        let peer_lookup = peer_lookup.clone();
        let ntp_ids = ntp_ids.clone();

        tokio::spawn(async move {
            let mut ewma = WeightedEwma::new(cfg.filter.lambda);
            let mut fused_ring: Vec<(u64, i64)> = Vec::with_capacity(256);
            let mut ticker = interval(Duration::from_secs(1));
            loop {
                ticker.tick().await;
                if peers.is_empty() {
                    continue;
                }

                if let Err(err) = run_fusion_iteration(
                    &cfg,
                    &redis,
                    &phc,
                    &peers,
                    &peer_lookup,
                    &ntp_ids,
                    &mut ewma,
                    &mut fused_ring,
                )
                .await
                {
                    debug!("fusion iteration failed: {err}");
                }
            }
        })
    };

    tokio::try_join!(rx_loop, tx_loop, fusion_loop)?;
    Ok(())
}

async fn handle_request(
    buf: &[u8],
    addr: SocketAddr,
    sock: &UdpSocket,
    cfg: &Config,
    phc: &PhcReader,
    redis: &RedisStore,
) -> Result<()> {
    let req: TimeRequest = bincode::deserialize(buf)?;
    if req.header.ver != WIRE_VERSION {
        return Ok(());
    }
    let t2 = phc.now_ns().unwrap_or_else(|_| monotonic_fallback_ns());
    let t3 = phc.now_ns().unwrap_or_else(|_| monotonic_fallback_ns());

    let mut resp = TimeResponse {
        header: Header::new(MSG_RESP, req.header.seq),
        server_id: cfg.node_id,
        t1_echo_ns: req.t1_send_ns,
        t2_recv_ns: t2,
        t3_send_ns: t3,
        server_quality: 100,
        reserved: [0; RESERVED_BYTES],
        signature: [0; SIGNATURE_BYTES],
    };

    // Future security hooks: populate signature/reserved when authentication enabled.
    resp.signature = [0; SIGNATURE_BYTES];

    let payload = bincode::serialize(&resp)?;
    sock.send_to(&payload, addr).await?;

    // Record service latency for observability.
    let now_ns = monotonic_fallback_ns();
    let window_ns = cfg.sampling.window_sec * 1_000_000_000;
    let key = format!("ts:peer:{}:service_latency", cfg.node_id);
    let latency = (t3 as i128 - t2 as i128).max(0) as f64;
    let _ = redis
        .ts_add(&key, now_ns, latency, window_ns)
        .await
        .map_err(|err| debug!("redis write failed: {err}"));

    Ok(())
}

async fn handle_response(
    buf: &[u8],
    addr: SocketAddr,
    cfg: &Config,
    phc: &PhcReader,
    redis: &RedisStore,
    pending: &Arc<Mutex<HashMap<u32, PendingRequest>>>,
    runtime: &Arc<Mutex<HashMap<u32, PeerRuntime>>>,
) -> Result<()> {
    let resp: TimeResponse = bincode::deserialize(buf)?;
    if resp.header.ver != WIRE_VERSION {
        return Ok(());
    }
    let t4 = phc.now_ns().unwrap_or_else(|_| monotonic_fallback_ns());

    let pend = {
        let mut guard = pending.lock().await;
        guard.remove(&resp.header.seq)
    };
    let Some(p) = pend else {
        debug!("orphan response seq={} from {}", resp.header.seq, addr);
        return Ok(());
    };
    if p.peer_id != resp.server_id {
        debug!(
            "server id mismatch for seq {} expected {} got {}",
            resp.header.seq, p.peer_id, resp.server_id
        );
        return Ok(());
    }

    let calc = compute_offset_delay(
        p.t1_ns,
        resp.t2_recv_ns as i128,
        resp.t3_send_ns as i128,
        t4 as i128,
    );
    let now_ns = t4;
    let window_ns = cfg.sampling.window_sec * 1_000_000_000;

    let mut state = runtime.lock().await;
    let entry = state.entry(resp.server_id).or_default();
    let delay_f = calc.delay_ns as f64;
    let jitter_f = entry
        .last_delay_ns
        .map(|prev| (delay_f - prev).abs())
        .unwrap_or(0.0);
    entry.last_delay_ns = Some(delay_f);
    entry.last_offset_ns = Some(calc.offset_ns as f64);
    entry.last_jitter_ns = Some(jitter_f);
    drop(state);

    let offset_key = format!("ts:peer:{}:offset", resp.server_id);
    let delay_key = format!("ts:peer:{}:delay", resp.server_id);
    let jitter_key = format!("ts:peer:{}:jitter", resp.server_id);
    let quality_key = format!("ts:peer:{}:quality", resp.server_id);

    let _ = redis
        .ts_add(&offset_key, now_ns, calc.offset_ns as f64, window_ns)
        .await;
    let _ = redis.ts_add(&delay_key, now_ns, delay_f, window_ns).await;
    let _ = redis.ts_add(&jitter_key, now_ns, jitter_f, window_ns).await;
    let _ = redis
        .ts_add(&quality_key, now_ns, resp.server_quality as f64, window_ns)
        .await;

    Ok(())
}

async fn cleanup_timeouts(
    pending: &Arc<Mutex<HashMap<u32, PendingRequest>>>,
    redis: &RedisStore,
    window_sec: u64,
    timeout: Duration,
) {
    let now = Instant::now();
    let mut expired: Vec<PendingRequest> = Vec::new();
    {
        let mut guard = pending.lock().await;
        guard.retain(|_, req| {
            if now.duration_since(req.sent_at) > timeout {
                expired.push(req.clone());
                false
            } else {
                true
            }
        });
    }
    if expired.is_empty() {
        return;
    }
    let window_ns = window_sec * 1_000_000_000;
    for req in expired {
        let key = format!("ts:peer:{}:timeouts", req.peer_id);
        let ts = monotonic_fallback_ns();
        let _ = redis.ts_add(&key, ts, 1.0, window_ns).await;
    }
}

async fn run_fusion_iteration(
    cfg: &Config,
    redis: &RedisStore,
    phc: &PhcReader,
    peers: &[PeerEntry],
    peer_lookup: &HashMap<u32, PeerEntry>,
    ntp_ids: &HashSet<u32>,
    ewma: &mut WeightedEwma,
    fused_ring: &mut Vec<(u64, i64)>,
) -> Result<()> {
    let window_ns = cfg.sampling.window_sec * 1_000_000_000;
    let since = monotonic_fallback_ns().saturating_sub(window_ns);

    let mut samples = Vec::new();
    let limit = cfg.filter.min_samples_per_peer.max(1) as usize;
    for peer in peers.iter().filter(|p| p.id != cfg.node_id) {
        let offset = redis
            .ts_recent(&format!("ts:peer:{}:offset", peer.id), since, limit)
            .await?;
        if offset.len() < limit {
            continue;
        }
        let delay = redis
            .ts_recent(&format!("ts:peer:{}:delay", peer.id), since, limit)
            .await?;
        let jitter = redis
            .ts_recent(&format!("ts:peer:{}:jitter", peer.id), since, limit)
            .await?;

        let stats = PeerSample::from_series(
            peer.id,
            peer.weight.unwrap_or(1.0),
            &offset,
            &delay,
            &jitter,
        );
        samples.push(stats);
    }

    for ntp in &cfg.ntp_sources {
        let offset = redis
            .ts_recent(&format!("ts:ntp:{}:offset", ntp.id), since, limit)
            .await?;
        if offset.len() < limit {
            continue;
        }
        let delay = redis
            .ts_recent(&format!("ts:ntp:{}:delay", ntp.id), since, limit)
            .await?;
        let jitter = redis
            .ts_recent(&format!("ts:ntp:{}:jitter", ntp.id), since, limit)
            .await?;

        let stats = PeerSample::from_series(
            ntp.id,
            ntp.weight.unwrap_or(ntp::NTP_DEFAULT_WEIGHT),
            &offset,
            &delay,
            &jitter,
        );
        samples.push(stats);
    }

    if samples.is_empty() {
        return Ok(());
    }

    let best = select_best_peers(&samples, cfg.filter.best_quantile);
    if best.is_empty() {
        return Ok(());
    }

    let trimmed = TrimmedStats::from_samples(best, cfg.filter.trim_ratio);
    if trimmed.is_empty() {
        return Ok(());
    }
    let fused = trimmed.weighted_mean();
    let smoothed = ewma.push(fused.mean_offset_ns, fused.total_weight);

    let ts = monotonic_fallback_ns();
    let _ = redis
        .ts_add("ts:fused:offset", ts, fused.mean_offset_ns, window_ns)
        .await;
    let _ = redis
        .ts_add("ts:fused:delay", ts, fused.mean_delay_ns, window_ns)
        .await;
    let _ = redis.ts_add("ts:fused:ewma", ts, smoothed, window_ns).await;

    fused_ring.push((ts, smoothed as i64));
    if fused_ring.len() > 512 {
        fused_ring.drain(0..fused_ring.len() - 512);
    }

    if fused_ring.len() >= 3 {
        let drift_ppm = linreg_slope_ppm(&fused_ring);
        let _ = redis
            .ts_add("ts:fused:drift_ppm", ts, drift_ppm, window_ns)
            .await;

        if cfg.mode.control_enable && !cfg.mode.observer_only {
            if let Err(err) = slew::apply(smoothed, drift_ppm) {
                debug!("adjtimex apply failed: {err}");
            }
        }
    }

    if let Some(phc_offset) = phc.phc_system_offset()? {
        let phc_ns = phc_offset as f64;
        let diff = smoothed - phc_ns;
        let _ = redis
            .ts_add("ts:phc:system_offset", ts, phc_ns, window_ns)
            .await;
        let _ = redis
            .ts_add("ts:phc:fused_delta", ts, diff, window_ns)
            .await;
    }

    // Store peer-level aggregates for telemetry.
    for sample in samples {
        let key = if peer_lookup.contains_key(&sample.peer_id) {
            format!("hash:peer:{}:summary", sample.peer_id)
        } else if ntp_ids.contains(&sample.peer_id) {
            format!("hash:ntp:{}:summary", sample.peer_id)
        } else {
            format!("hash:peer:{}:summary", sample.peer_id)
        };
        let _ = redis
            .hm_set(
                &key,
                &[
                    ("offset_ns", sample.offset_median_ns),
                    ("delay_ns", sample.delay_median_ns),
                    ("jitter_ns", sample.jitter_median_ns),
                    ("score", peer_score(&sample)),
                ],
            )
            .await;
    }

    let _ = redis
        .hm_set(
            "hash:fused:summary",
            &[
                ("smoothed_offset_ns", smoothed),
                ("raw_offset_ns", fused.mean_offset_ns),
                ("mean_delay_ns", fused.mean_delay_ns),
                ("weight_sum", fused.total_weight),
            ],
        )
        .await;

    if let Some(my_state) = peer_lookup.get(&cfg.node_id) {
        let _ = redis
            .hm_set(
                &format!("hash:peer:{}:self", my_state.id),
                &[("last_ewma_ns", smoothed)],
            )
            .await;
    }

    Ok(())
}

fn monotonic_fallback_ns() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_secs() * 1_000_000_000) + now.subsec_nanos() as u64
}
