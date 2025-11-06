use anyhow::Result;

#[cfg(target_os = "linux")]
pub fn apply(offset_ns: f64, drift_ppm: f64) -> Result<()> {
    use anyhow::Context;

    let mut tx: libc::timex = unsafe { std::mem::zeroed() };
    tx.modes = (libc::ADJ_OFFSET | libc::ADJ_FREQUENCY) as libc::c_uint;

    let offset_us = (offset_ns / 1_000.0).clamp(-500_000.0, 500_000.0);
    tx.offset = offset_us as libc::c_long;

    let freq = (drift_ppm * (1 << 16) as f64).clamp(-512_000.0, 512_000.0);
    tx.freq = freq as libc::c_long;

    let rc = unsafe { libc::adjtimex(&mut tx) };
    if rc == -1 {
        return Err(std::io::Error::last_os_error())
            .context("adjtimex failed when applying PLL correction");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn apply(_offset_ns: f64, _drift_ppm: f64) -> Result<()> {
    tracing::debug!("PLL control not supported on this platform; skipping adjtimex");
    Ok(())
}
