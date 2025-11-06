use std::cmp::Ordering;

#[derive(Clone, Default)]
pub struct WeightedEwma {
    lambda: f64,
    state: f64,
    init: bool,
}

#[derive(Clone, Debug)]
pub struct PeerSample {
    pub peer_id: u32,
    pub peer_weight: f64,
    pub offset_median_ns: f64,
    pub delay_median_ns: f64,
    pub jitter_median_ns: f64,
}

#[derive(Clone, Debug)]
pub struct TrimmedStats {
    pub mean_offset_ns: f64,
    pub mean_delay_ns: f64,
    pub total_weight: f64,
    pub kept: Vec<PeerSample>,
}

impl WeightedEwma {
    pub fn new(lambda: f64) -> Self {
        Self {
            lambda: lambda.clamp(0.0, 1.0),
            ..Default::default()
        }
    }

    pub fn push(&mut self, value: f64, weight: f64) -> f64 {
        let alpha = (self.lambda * (1.0 + weight / 4.0)).clamp(0.0, 1.0);
        if !self.init {
            self.state = value;
            self.init = true;
        } else {
            self.state = alpha * value + (1.0 - alpha) * self.state;
        }
        self.state
    }
}

impl PeerSample {
    pub fn from_series(
        peer_id: u32,
        weight: f64,
        offsets: &[(u64, f64)],
        delays: &[(u64, f64)],
        jitters: &[(u64, f64)],
    ) -> Self {
        let offset_vals: Vec<f64> = offsets.iter().map(|(_, v)| *v).collect();
        let delay_vals: Vec<f64> = delays.iter().map(|(_, v)| *v).collect();
        let jitter_vals: Vec<f64> = jitters.iter().map(|(_, v)| *v).collect();

        let offset_med = robust_median(&offset_vals).unwrap_or(0.0);
        let delay_med = robust_median(&delay_vals).unwrap_or(0.0);
        let jitter_med = robust_median(&jitter_vals).unwrap_or(0.0);

        Self {
            peer_id,
            peer_weight: weight.max(0.01),
            offset_median_ns: offset_med,
            delay_median_ns: delay_med,
            jitter_median_ns: jitter_med.abs(),
        }
    }
}

impl TrimmedStats {
    pub fn from_samples(samples: Vec<PeerSample>, trim_ratio: f64) -> Self {
        let mut subset = samples;
        subset.sort_by(|a, b| {
            a.offset_median_ns
                .partial_cmp(&b.offset_median_ns)
                .unwrap_or(Ordering::Equal)
        });
        let n = subset.len();
        let trim = ((n as f64) * trim_ratio).floor() as usize;
        let start = trim.min(n);
        let end = n.saturating_sub(trim).max(start + 1);
        let kept = subset[start..end].to_vec();

        let mut total_weight = 0.0;
        let mut weighted_offset = 0.0;
        let mut weighted_delay = 0.0;
        for sample in &kept {
            let weight = sample.peer_weight / (1.0 + sample.jitter_median_ns.abs() / 1_000.0);
            total_weight += weight;
            weighted_offset += weight * sample.offset_median_ns;
            weighted_delay += weight * sample.delay_median_ns;
        }

        let mean_offset_ns = if total_weight > 0.0 {
            weighted_offset / total_weight
        } else {
            kept.iter().map(|s| s.offset_median_ns).sum::<f64>() / kept.len().max(1) as f64
        };
        let mean_delay_ns = if total_weight > 0.0 {
            weighted_delay / total_weight
        } else {
            kept.iter().map(|s| s.delay_median_ns).sum::<f64>() / kept.len().max(1) as f64
        };

        Self {
            mean_offset_ns,
            mean_delay_ns,
            total_weight: total_weight.max(f64::EPSILON),
            kept,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    pub fn weighted_mean(&self) -> &Self {
        self
    }
}

pub fn select_best_peers(samples: &[PeerSample], quantile: f64) -> Vec<PeerSample> {
    if samples.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<PeerSample> = samples.to_vec();
    sorted.sort_by(|a, b| {
        peer_score(a)
            .partial_cmp(&peer_score(b))
            .unwrap_or(Ordering::Equal)
    });
    let keep = ((sorted.len() as f64) * quantile.clamp(0.0, 1.0)).ceil() as usize;
    sorted.into_iter().take(keep.max(1)).collect()
}

pub fn peer_score(sample: &PeerSample) -> f64 {
    sample.delay_median_ns.abs() + sample.jitter_median_ns.abs()
}

fn robust_median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        Some((sorted[mid - 1] + sorted[mid]) / 2.0)
    } else {
        Some(sorted[mid])
    }
}
