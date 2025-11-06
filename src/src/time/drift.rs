pub fn linreg_slope_ppm(samples: &[(u64, i64)]) -> f64 {
    if samples.len() < 3 {
        return 0.0;
    }
    let n = samples.len() as f64;
    let mt = samples.iter().map(|(t, _)| *t as f64).sum::<f64>() / n;
    let mo = samples.iter().map(|(_, o)| *o as f64).sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (t, o) in samples {
        let dt = *t as f64 - mt;
        num += dt * (*o as f64 - mo);
        den += dt * dt;
    }
    let slope = if den > 0.0 { num / den } else { 0.0 }; // ns/ns
    slope * 1.0e3 // → ppm
}
