use serde::Deserialize;
use std::{collections::HashMap, fs, path::Path};

#[derive(Clone, Debug, Deserialize)]
pub struct PeerEntry {
    pub id: u32,
    pub ip: String,
    pub port: u16,
    #[serde(default)]
    pub weight: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SamplingCfg {
    /// Interval between active probes (milliseconds).
    pub interval_ms: u64,
    /// Sliding window size for statistics (seconds).
    pub window_sec: u64,
    /// Number of packets in a burst per peer (optional).
    #[serde(default)]
    pub burst: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FilterCfg {
    /// EWMA smoothing factor.
    pub lambda: f64,
    /// Fraction to trim from each side of the sorted offsets.
    pub trim_ratio: f64,
    /// Keep the best fraction of peers (by delay/jitter score).
    pub best_quantile: f64,
    /// Minimum samples required per peer in the window.
    pub min_samples_per_peer: u32,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct ModeCfg {
    #[serde(default)]
    pub observer_only: bool,
    #[serde(default)]
    pub control_enable: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NtpSource {
    pub id: u32,
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub weight: Option<f64>,
    #[serde(default)]
    pub poll_interval_ms: Option<u64>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub alias: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub node_id: u32,
    pub listen_addr: String,
    pub redis_url: String,
    pub phc_device: Option<String>,
    pub telemetry_addr: String,
    pub sampling: SamplingCfg,
    pub filter: FilterCfg,
    #[serde(default)]
    pub peers: Vec<PeerEntry>,
    #[serde(default)]
    pub mode: ModeCfg,
    /// Optional metadata to tag time-series data (stored alongside Redis keys).
    #[serde(default)]
    #[allow(dead_code)]
    pub tags: HashMap<String, String>,
    #[serde(default)]
    pub ntp_sources: Vec<NtpSource>,
}

pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Config> {
    let buf = fs::read_to_string(path)?;
    Ok(toml::from_str::<Config>(&buf)?)
}

pub fn ensure_defaults(cfg: &mut Config) {
    if cfg.phc_device.is_none() && cfg.ntp_sources.is_empty() {
        tracing::info!(
            "No PHC device provided and no NTP sources configured; falling back to default public NTP set"
        );
        cfg.ntp_sources = default_ntp_sources();
    }
}

pub fn default_ntp_sources() -> Vec<NtpSource> {
    vec![
        NtpSource {
            id: 9101,
            host: "time.google.com".into(),
            port: None,
            weight: Some(0.15),
            poll_interval_ms: Some(2_000),
            timeout_ms: Some(1_200),
            alias: Some("google".into()),
        },
        NtpSource {
            id: 9102,
            host: "time.cloudflare.com".into(),
            port: None,
            weight: Some(0.1),
            poll_interval_ms: Some(2_500),
            timeout_ms: Some(1_500),
            alias: Some("cloudflare".into()),
        },
        NtpSource {
            id: 9103,
            host: "pool.ntp.org".into(),
            port: None,
            weight: Some(0.1),
            poll_interval_ms: Some(3_000),
            timeout_ms: Some(1_500),
            alias: Some("pool".into()),
        },
        NtpSource {
            id: 9104,
            host: "0.pool.ntp.org".into(),
            port: None,
            weight: Some(0.1),
            poll_interval_ms: Some(3_000),
            timeout_ms: Some(1_500),
            alias: Some("pool-0".into()),
        },
        NtpSource {
            id: 9105,
            host: "1.pool.ntp.org".into(),
            port: None,
            weight: Some(0.1),
            poll_interval_ms: Some(3_000),
            timeout_ms: Some(1_500),
            alias: Some("pool-1".into()),
        },
        NtpSource {
            id: 9106,
            host: "time.nist.gov".into(),
            port: None,
            weight: Some(0.08),
            poll_interval_ms: Some(4_000),
            timeout_ms: Some(2_000),
            alias: Some("nist".into()),
        },
        NtpSource {
            id: 9107,
            host: "time.windows.com".into(),
            port: None,
            weight: Some(0.08),
            poll_interval_ms: Some(4_000),
            timeout_ms: Some(2_000),
            alias: Some("windows".into()),
        },
        NtpSource {
            id: 9108,
            host: "ntp.ubuntu.com".into(),
            port: None,
            weight: Some(0.08),
            poll_interval_ms: Some(3_500),
            timeout_ms: Some(2_000),
            alias: Some("ubuntu".into()),
        },
        NtpSource {
            id: 9109,
            host: "time.apple.com".into(),
            port: None,
            weight: Some(0.08),
            poll_interval_ms: Some(3_500),
            timeout_ms: Some(2_000),
            alias: Some("apple".into()),
        },
        NtpSource {
            id: 9110,
            host: "time1.google.com".into(),
            port: None,
            weight: Some(0.12),
            poll_interval_ms: Some(2_200),
            timeout_ms: Some(1_200),
            alias: Some("google-1".into()),
        },
    ]
}
