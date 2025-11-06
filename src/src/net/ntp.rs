use crate::{
    config::Config, phc::reader::PhcReader, store::redis::RedisStore,
    time::calc::compute_offset_delay,
};
use anyhow::{anyhow, Context, Result};
use std::sync::Arc;
use tokio::{
    net::{lookup_host, UdpSocket},
    process::Command,
    time::{interval, timeout, Duration},
};
use tracing::{debug, warn};

const NTP_PACKET_LEN: usize = 48;
const NTP_UNIX_TO_1900: i128 = 2_208_988_800;
const DEFAULT_POLL_MS: u64 = 2_000;
const DEFAULT_TIMEOUT_MS: u64 = 1_500;
pub const NTP_DEFAULT_WEIGHT: f64 = 0.2;

pub fn spawn_ntp_pollers(cfg: Arc<Config>, redis: RedisStore, phc: PhcReader) {
    for source in cfg.ntp_sources.clone() {
        let redis = redis.clone();
        let phc = phc.clone();
        let cfg = cfg.clone();
        tokio::spawn(async move {
            let poll_ms = source.poll_interval_ms.unwrap_or(DEFAULT_POLL_MS).max(250);
            let timeout_ms = source.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
            let port = source.port.unwrap_or(123);
            let mut ticker = interval(Duration::from_millis(poll_ms));
            let window_ns = cfg.sampling.window_sec.max(1) * 1_000_000_000;
            let mut last_delay: Option<f64> = None;
            let mut last_offset: Option<f64> = None;
            let mut last_ts: Option<u64> = None;
            loop {
                ticker.tick().await;
                let mut addrs = match lookup_host((source.host.as_str(), port)).await {
                    Ok(addrs) => addrs.collect::<Vec<_>>(),
                    Err(err) => {
                        warn!("ntp {} lookup failed: {}", source.host, err);
                        continue;
                    }
                };
                if addrs.is_empty() {
                    warn!("ntp {} lookup returned no addresses", source.host);
                    continue;
                }
                let addr = addrs.remove(0);
                match poll_once(&addr, timeout_ms, &phc).await {
                    Ok(sample) => {
                        let delay_f = sample.delay_ns as f64;
                        let jitter = last_delay.map(|prev| (delay_f - prev).abs()).unwrap_or(0.0);
                        last_delay = Some(delay_f);
                        let offset_f = sample.offset_ns as f64;
                        let drift_ppm = match last_offset.zip(last_ts) {
                            Some((prev_offset, prev_ts)) if sample.timestamp_ns > prev_ts => {
                                let delta_offset = offset_f - prev_offset;
                                let delta_time = (sample.timestamp_ns - prev_ts) as f64;
                                if delta_time > 0.0 {
                                    (delta_offset / delta_time) * 1_000_000.0
                                } else {
                                    0.0
                                }
                            }
                            _ => 0.0,
                        };
                        last_offset = Some(offset_f);
                        last_ts = Some(sample.timestamp_ns);

                        let offset_key = format!("ts:ntp:{}:offset", source.id);
                        let delay_key = format!("ts:ntp:{}:delay", source.id);
                        let jitter_key = format!("ts:ntp:{}:jitter", source.id);
                        let quality_key = format!("ts:ntp:{}:quality", source.id);
                        let drift_key = format!("ts:ntp:{}:drift_ppm", source.id);
                        let remote_key = format!("ts:ntp:{}:remote_time", source.id);
                        let system_delta_key = format!("ts:ntp:{}:system_delta", source.id);
                        let ping_key = format!("ts:ntp:{}:ping_ms", source.id);
                        let _ = redis
                            .ts_add(
                                &offset_key,
                                sample.timestamp_ns,
                                sample.offset_ns as f64,
                                window_ns,
                            )
                            .await;
                        let _ = redis
                            .ts_add(&delay_key, sample.timestamp_ns, delay_f, window_ns)
                            .await;
                        let _ = redis
                            .ts_add(&jitter_key, sample.timestamp_ns, jitter, window_ns)
                            .await;
                        let _ = redis
                            .ts_add(
                                &quality_key,
                                sample.timestamp_ns,
                                sample.stratum as f64,
                                window_ns,
                            )
                            .await;
                        let _ = redis
                            .ts_add(&drift_key, sample.timestamp_ns, drift_ppm, window_ns)
                            .await;
                        let remote_seconds = sample.remote_time_ns as f64 / 1_000_000_000.0;
                        let system_now = match phc.system_now_ns() {
                            Ok(ns) => ns as f64 / 1_000_000_000.0,
                            Err(_) => 0.0,
                        };
                        let system_delta = remote_seconds - system_now;
                        let _ = redis
                            .ts_add(&remote_key, sample.timestamp_ns, remote_seconds, window_ns)
                            .await;
                        let _ = redis
                            .ts_add(
                                &system_delta_key,
                                sample.timestamp_ns,
                                system_delta,
                                window_ns,
                            )
                            .await;
                        let score = delay_f.abs() + jitter;
                        let weight = source.weight.unwrap_or(NTP_DEFAULT_WEIGHT);
                        let summary_key = format!("hash:ntp:{}:summary", source.id);
                        let mut summary_fields: Vec<(&str, f64)> = vec![
                            ("offset_ns", offset_f),
                            ("delay_ns", delay_f),
                            ("jitter_ns", jitter),
                            ("score", score),
                            ("weight", weight),
                            ("stratum", sample.stratum as f64),
                            ("drift_ppm", drift_ppm),
                            ("remote_time_s", remote_seconds),
                            ("system_delta_s", system_delta),
                        ];
                        if let Some(diff) =
                            compute_offset_vs_mean(cfg.as_ref(), &redis, source.id, offset_f).await
                        {
                            let diff_key = format!("ts:ntp:{}:offset_vs_mean", source.id);
                            let _ = redis
                                .ts_add(&diff_key, sample.timestamp_ns, diff, window_ns)
                                .await;
                            summary_fields.push(("offset_vs_mean_ns", diff));
                        }
                        if let Some(ping_ms) =
                            ping_latency_ms(&source.host, Duration::from_millis(timeout_ms)).await
                        {
                            let _ = redis
                                .ts_add(&ping_key, sample.timestamp_ns, ping_ms, window_ns)
                                .await;
                            summary_fields.push(("ping_ms", ping_ms));
                        }
                        let _ = redis.hm_set(&summary_key, &summary_fields).await;
                    }
                    Err(err) => {
                        debug!("ntp poll {} failed: {}", source.host, err);
                    }
                }
            }
        });
    }
}

struct NtpSample {
    timestamp_ns: u64,
    offset_ns: i128,
    delay_ns: i128,
    stratum: u8,
    remote_time_ns: i128,
}

async fn poll_once(
    addr: &std::net::SocketAddr,
    timeout_ms: u64,
    phc: &PhcReader,
) -> Result<NtpSample> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(addr).await?;

    let mut packet = [0u8; NTP_PACKET_LEN];
    packet[0] = 0b00_100_011; // LI=0, Version=4, Mode=3 (client)

    let t1 = phc.now_ns()?;
    encode_ntp_timestamp(&mut packet[40..48], t1);

    socket.send(&packet).await?;

    let mut resp = [0u8; NTP_PACKET_LEN];
    let (len, _) = timeout(
        Duration::from_millis(timeout_ms),
        socket.recv_from(&mut resp),
    )
    .await??;
    if len < NTP_PACKET_LEN {
        return Err(anyhow!("short NTP response: {} bytes", len));
    }
    let t4 = phc.now_ns()?;

    let stratum = resp[1];
    let t1_remote = decode_ntp_timestamp(&resp[24..32]).context("originate timestamp missing")?;
    let t2_remote = decode_ntp_timestamp(&resp[32..40]).context("receive timestamp missing")?;
    let t3_remote = decode_ntp_timestamp(&resp[40..48]).context("transmit timestamp missing")?;

    // Some servers zero the originate field; fall back to local t1 in that case.
    let t1_used = if t1_remote == 0 {
        t1 as i128
    } else {
        t1_remote
    };

    let calc = compute_offset_delay(t1_used, t2_remote, t3_remote, t4 as i128);

    Ok(NtpSample {
        timestamp_ns: t4,
        offset_ns: calc.offset_ns,
        delay_ns: calc.delay_ns,
        stratum,
        remote_time_ns: t3_remote,
    })
}

fn encode_ntp_timestamp(buf: &mut [u8], unix_ns: u64) {
    let unix_sec = unix_ns / 1_000_000_000;
    let unix_frac_ns = unix_ns % 1_000_000_000;
    let ntp_sec = unix_sec + NTP_UNIX_TO_1900 as u64;
    let frac = ((unix_frac_ns as u128) << 32) / 1_000_000_000u128;
    buf[..4].copy_from_slice(&u32::try_from(ntp_sec).unwrap_or(u32::MAX).to_be_bytes());
    buf[4..8].copy_from_slice(&(frac as u32).to_be_bytes());
}

fn decode_ntp_timestamp(buf: &[u8]) -> Result<i128> {
    if buf.len() < 8 {
        return Err(anyhow!("invalid ntp timestamp length"));
    }
    let secs = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as i128;
    let frac = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as u128;
    if secs == 0 && frac == 0 {
        return Ok(0);
    }
    let unix_sec = secs - NTP_UNIX_TO_1900;
    let frac_ns = ((frac as u128) * 1_000_000_000u128 >> 32) as i128;
    Ok(unix_sec * 1_000_000_000 + frac_ns)
}

async fn compute_offset_vs_mean(
    cfg: &Config,
    redis: &RedisStore,
    current_id: u32,
    current_offset_ns: f64,
) -> Option<f64> {
    let ids: Vec<u32> = cfg.ntp_sources.iter().map(|s| s.id).collect();
    let mut total = 0.0;
    let mut count = 0usize;
    for id in ids.into_iter() {
        if id == current_id {
            continue;
        }
        let key = format!("hash:ntp:{}:summary", id);
        if let Ok(map) = redis.hm_get_all(&key).await {
            if let Some(val) = map.get("offset_ns").and_then(|v| v.parse::<f64>().ok()) {
                total += val;
                count += 1;
            }
        }
    }
    if count > 0 {
        let mean = total / count as f64;
        Some(current_offset_ns - mean)
    } else {
        None
    }
}

async fn ping_latency_ms(host: &str, timeout_budget: Duration) -> Option<f64> {
    let mut cmd = Command::new("ping");
    cmd.arg("-c").arg("1");
    #[cfg(target_os = "linux")]
    {
        let secs = timeout_budget.as_secs().max(1);
        cmd.arg("-W").arg(secs.to_string());
    }
    cmd.arg(host);
    let output = match timeout(timeout_budget, cmd.output()).await.ok()? {
        Ok(out) => out,
        Err(_) => return None,
    };
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(idx) = line.find("time=") {
            let after = &line[idx + 5..];
            let token = after
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches("ms");
            if let Ok(val) = token.parse::<f64>() {
                return Some(val);
            }
        }
    }
    None
}
