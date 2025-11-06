use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

static START_INSTANT: OnceLock<Instant> = OnceLock::new();

/// Simple monotonic clock helper returning nanoseconds since the helper was first used.
#[allow(dead_code)]
pub fn monotonic_ns() -> u128 {
    let now = Instant::now();
    let start = *START_INSTANT.get_or_init(|| now);
    duration_to_ns(now.duration_since(start))
}

#[allow(dead_code)]
pub fn duration_to_ns(d: Duration) -> u128 {
    d.as_secs() as u128 * 1_000_000_000 + d.subsec_nanos() as u128
}

#[allow(dead_code)]
pub fn ns_to_secs_f64(ns: u64) -> f64 {
    ns as f64 / 1e9
}
