use crate::{
    config::{Config, NtpSource},
    store::redis::RedisStore,
};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::{cmp::Ordering, collections::HashMap, net::SocketAddr, sync::Arc, time::SystemTime};

const PROM_QUANTILES: &[(f64, &str)] = &[(0.5, "0.50"), (0.95, "0.95")];
const METRIC_LOOKBACK_LIMIT: usize = 256;

#[derive(Clone)]
struct AppState {
    cfg: Arc<Config>,
    redis: RedisStore,
}

pub async fn serve(cfg: Config, redis: RedisStore) -> anyhow::Result<()> {
    let addr: SocketAddr = cfg.telemetry_addr.parse()?;
    let state = AppState {
        cfg: Arc::new(cfg),
        redis,
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let now_ns = system_time_ns();
    match state.redis.ts_last("ts:fused:ewma").await {
        Ok(Some((ts, _))) if now_ns.saturating_sub(ts) < 5_000_000_000 => (StatusCode::OK, "ok\n"),
        Ok(_) => (StatusCode::SERVICE_UNAVAILABLE, "stale\n"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "booting\n"),
    }
}

async fn metrics(State(state): State<AppState>) -> Response {
    let mut body = String::new();
    body.push_str("# zeitpeer metrics\n");
    body.push_str(&format!(
        "zeitpeer_node_info{{node_id=\"{}\"}} 1\n",
        state.cfg.node_id
    ));

    let now_ns = system_time_ns();
    let window_ns = state.cfg.sampling.window_sec.max(1) * 1_000_000_000;
    let since_ns = now_ns.saturating_sub(window_ns);

    write_primary_metrics(&state, since_ns, &mut body).await;
    write_peer_metrics(&state, &mut body).await;

    Response::builder()
        .status(StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )
        .body(body.into())
        .unwrap()
}

async fn write_primary_metrics(state: &AppState, since_ns: u64, body: &mut String) {
    if let Ok(samples) = state
        .redis
        .ts_recent("ts:fused:ewma", since_ns, METRIC_LOOKBACK_LIMIT)
        .await
    {
        if let Some((_, val)) = samples.last() {
            body.push_str("# TYPE zeitpeer_p2p_fused_offset_ns gauge\n");
            body.push_str(&format!("zeitpeer_p2p_fused_offset_ns {}\n", val));
        }
        add_quantile_series(body, "zeitpeer_p2p_fused_offset_ns", &samples);
    }

    if let Ok(samples) = state
        .redis
        .ts_recent("ts:fused:drift_ppm", since_ns, METRIC_LOOKBACK_LIMIT)
        .await
    {
        if let Some((_, val)) = samples.last() {
            body.push_str("# TYPE zeitpeer_drift_ppm gauge\n");
            body.push_str(&format!("zeitpeer_drift_ppm {}\n", val));
        }
        add_quantile_series(body, "zeitpeer_drift_ppm", &samples);
    }

    if let Ok(samples) = state
        .redis
        .ts_recent("ts:phc:system_offset", since_ns, METRIC_LOOKBACK_LIMIT)
        .await
    {
        if let Some((_, val)) = samples.last() {
            body.push_str("# TYPE zeitpeer_phc_vs_sys_offset_ns gauge\n");
            body.push_str(&format!("zeitpeer_phc_vs_sys_offset_ns {}\n", val));
        }
        add_quantile_series(body, "zeitpeer_phc_vs_sys_offset_ns", &samples);
    }

    if let Ok(samples) = state
        .redis
        .ts_recent("ts:phc:fused_delta", since_ns, METRIC_LOOKBACK_LIMIT)
        .await
    {
        if let Some((_, val)) = samples.last() {
            body.push_str("# TYPE zeitpeer_p2p_vs_phc_offset_ns gauge\n");
            body.push_str(&format!("zeitpeer_p2p_vs_phc_offset_ns {}\n", val));
        }
        add_quantile_series(body, "zeitpeer_p2p_vs_phc_offset_ns", &samples);
    }
}

async fn write_peer_metrics(state: &AppState, body: &mut String) {
    for peer in &state.cfg.peers {
        let summary_key = format!("hash:peer:{}:summary", peer.id);
        if let Ok(map) = state.redis.hm_get_all(&summary_key).await {
            append_peer_metrics(body, peer.id, &map);
        }
        let timeout_key = format!("ts:peer:{}:timeouts", peer.id);
        if let Ok(Some((_, val))) = state.redis.ts_last(&timeout_key).await {
            body.push_str(&format!(
                "zeitpeer_peer_timeouts_total{{peer_id=\"{}\"}} {}\n",
                peer.id, val
            ));
        }
    }

    for source in &state.cfg.ntp_sources {
        let summary_key = format!("hash:ntp:{}:summary", source.id);
        if let Ok(map) = state.redis.hm_get_all(&summary_key).await {
            append_ntp_metrics(body, source, &map);
        }
    }
}

fn add_quantile_series(body: &mut String, metric: &str, samples: &[(u64, f64)]) {
    if samples.is_empty() {
        return;
    }
    let mut values: Vec<f64> = samples.iter().map(|(_, v)| *v).collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    for &(q, label) in PROM_QUANTILES {
        if let Some(val) = percentile(&values, q) {
            body.push_str(&format!(
                "{metric}{{quantile=\"{label}\"}} {val}\n",
                metric = metric,
                label = label,
                val = val
            ));
        }
    }
}

fn percentile(sorted_values: &[f64], q: f64) -> Option<f64> {
    if sorted_values.is_empty() {
        return None;
    }
    let clamped = q.clamp(0.0, 1.0);
    let pos = clamped * ((sorted_values.len() - 1) as f64);
    let idx = pos.floor() as usize;
    let frac = pos - (idx as f64);
    if idx + 1 < sorted_values.len() {
        Some(sorted_values[idx] + (sorted_values[idx + 1] - sorted_values[idx]) * frac)
    } else {
        sorted_values.last().copied()
    }
}

fn append_peer_metrics(body: &mut String, peer_id: u32, map: &HashMap<String, String>) {
    if let Some(offset) = parse_float(map, "offset_ns") {
        body.push_str(&format!(
            "zeitpeer_peer_offset_ns{{peer_id=\"{}\"}} {}\n",
            peer_id, offset
        ));
    }
    if let Some(delay) = parse_float(map, "delay_ns") {
        body.push_str(&format!(
            "zeitpeer_peer_delay_ns{{peer_id=\"{}\"}} {}\n",
            peer_id, delay
        ));
    }
    if let Some(jitter) = parse_float(map, "jitter_ns") {
        body.push_str(&format!(
            "zeitpeer_peer_jitter_ns{{peer_id=\"{}\"}} {}\n",
            peer_id, jitter
        ));
    }
    if let Some(score) = parse_float(map, "score") {
        body.push_str(&format!(
            "zeitpeer_peer_score{{peer_id=\"{}\"}} {}\n",
            peer_id, score
        ));
    }
}

fn append_ntp_metrics(body: &mut String, source: &NtpSource, map: &HashMap<String, String>) {
    let alias = source
        .alias
        .as_deref()
        .unwrap_or_else(|| source.host.as_str());
    if let Some(offset) = parse_float(map, "offset_ns") {
        body.push_str(&format!(
            "zeitpeer_ntp_offset_ns{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, offset
        ));
    }
    if let Some(delay) = parse_float(map, "delay_ns") {
        body.push_str(&format!(
            "zeitpeer_ntp_delay_ns{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, delay
        ));
    }
    if let Some(jitter) = parse_float(map, "jitter_ns") {
        body.push_str(&format!(
            "zeitpeer_ntp_jitter_ns{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, jitter
        ));
    }
    if let Some(score) = parse_float(map, "score") {
        body.push_str(&format!(
            "zeitpeer_ntp_score{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, score
        ));
    }
    if let Some(stratum) = parse_float(map, "stratum") {
        body.push_str(&format!(
            "zeitpeer_ntp_stratum{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, stratum
        ));
    }
    if let Some(weight) = parse_float(map, "weight") {
        body.push_str(&format!(
            "zeitpeer_ntp_weight{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, weight
        ));
    }
    if let Some(drift) = parse_float(map, "drift_ppm") {
        body.push_str(&format!(
            "zeitpeer_ntp_drift_ppm{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, drift
        ));
    }
    if let Some(remote) = parse_float(map, "remote_time_s") {
        body.push_str(&format!(
            "zeitpeer_ntp_remote_time_seconds{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, remote
        ));
    }
    if let Some(delta) = parse_float(map, "system_delta_s") {
        body.push_str(&format!(
            "zeitpeer_ntp_remote_delta_seconds{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, delta
        ));
    }
    if let Some(ping_ms) = parse_float(map, "ping_ms") {
        body.push_str(&format!(
            "zeitpeer_ntp_ping_ms{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, ping_ms
        ));
    }
    if let Some(diff) = parse_float(map, "offset_vs_mean_ns") {
        body.push_str(&format!(
            "zeitpeer_ntp_offset_vs_mean_ns{{ntp_id=\"{}\",alias=\"{}\"}} {}\n",
            source.id, alias, diff
        ));
    }
}

fn parse_float(map: &HashMap<String, String>, key: &str) -> Option<f64> {
    map.get(key).and_then(|v| v.parse::<f64>().ok())
}

fn system_time_ns() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() * 1_000_000_000 + d.subsec_nanos() as u64)
        .unwrap_or_default()
}
