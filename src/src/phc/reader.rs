#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::Result;
use nix::{
    sys::time::TimeSpec,
    time::{clock_gettime, ClockId},
};
use once_cell::sync::OnceCell;
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
#[cfg(target_os = "linux")]
use std::{fs::OpenOptions, os::unix::io::AsRawFd};

#[derive(Clone)]
pub struct PhcReader {
    inner: Arc<Inner>,
}

struct Inner {
    device: Option<String>,
    #[cfg(target_os = "linux")]
    fd: Mutex<Option<std::fs::File>>,
    #[cfg(target_os = "linux")]
    open_warned: AtomicBool,
}

impl Inner {
    fn new(device: Option<String>) -> Self {
        Inner {
            device,
            #[cfg(target_os = "linux")]
            fd: Mutex::new(None),
            #[cfg(target_os = "linux")]
            open_warned: AtomicBool::new(false),
        }
    }
}

impl PhcReader {
    pub fn new(device: Option<String>) -> Self {
        PhcReader {
            inner: Arc::new(Inner::new(device)),
        }
    }

    /// Returns the best time source available (PHC if configured, otherwise TAI/REALTIME).
    pub fn now_ns(&self) -> Result<u64> {
        if let Some(ns) = self.phc_now_ns()? {
            return Ok(ns);
        }
        let fallback = clock_gettime(fallback_clock_id())
            .or_else(|_| clock_gettime(ClockId::CLOCK_REALTIME))?;
        Ok(timespec_to_ns(&fallback))
    }

    pub fn system_now_ns(&self) -> Result<u64> {
        let ts = clock_gettime(ClockId::CLOCK_REALTIME)?;
        Ok(timespec_to_ns(&ts))
    }

    /// Reads the PHC (if available) and returns the time in nanoseconds.
    pub fn phc_now_ns(&self) -> Result<Option<u64>> {
        match self.inner.phc_clock_id() {
            Ok(Some(clock_id)) => {
                let ts = clock_gettime(clock_id)?;
                Ok(Some(timespec_to_ns(&ts)))
            }
            Ok(None) => Ok(None),
            Err(err) => {
                static WARN_ONCE: OnceCell<()> = OnceCell::new();
                WARN_ONCE.get_or_init(|| {
                    tracing::warn!("PHC access failed; falling back to system clock: {err}");
                });
                Ok(None)
            }
        }
    }

    /// Computes PHC - System offset in nanoseconds (if PHC available).
    pub fn phc_system_offset(&self) -> Result<Option<i128>> {
        let Some(phc) = self.phc_now_ns()? else {
            return Ok(None);
        };
        let system = self.system_now_ns()? as i128;
        Ok(Some(phc as i128 - system))
    }
}

impl Inner {
    #[cfg(target_os = "linux")]
    fn phc_clock_id(&self) -> Result<Option<ClockId>> {
        let Some(path) = &self.device else {
            return Ok(None);
        };
        let mut guard = self.fd.lock().unwrap();
        if guard.is_none() {
            match OpenOptions::new().read(true).open(path) {
                Ok(file) => {
                    *guard = Some(file);
                }
                Err(err) => {
                    if !self.open_warned.swap(true, Ordering::SeqCst) {
                        tracing::warn!(
                            "unable to open PHC device {}: {}; falling back to system clock",
                            path,
                            err
                        );
                    }
                    return Ok(None);
                }
            }
        }
        let fd = guard
            .as_ref()
            .map(|f| f.as_raw_fd())
            .with_context(|| "PHC device unexpectedly missing FD")?;
        let clock_id = clockid_from_fd(fd);
        Ok(Some(ClockId::from_raw(clock_id)))
    }

    #[cfg(not(target_os = "linux"))]
    fn phc_clock_id(&self) -> Result<Option<ClockId>> {
        if let Some(dev) = &self.device {
            tracing::debug!(
                "PHC device {} is configured but not supported on this platform; skipping",
                dev
            );
        }
        Ok(None)
    }
}

fn timespec_to_ns(ts: &TimeSpec) -> u64 {
    (ts.tv_sec() as u64) * 1_000_000_000 + ts.tv_nsec() as u64
}

#[cfg(target_os = "linux")]
fn clockid_from_fd(fd: std::os::unix::io::RawFd) -> libc::clockid_t {
    const CLOCKFD: libc::c_int = 3;
    let fd = fd as libc::c_int;
    (((!fd) << 3) | CLOCKFD) as libc::clockid_t
}

#[cfg(target_os = "linux")]
fn fallback_clock_id() -> ClockId {
    ClockId::CLOCK_TAI
}

#[cfg(not(target_os = "linux"))]
fn fallback_clock_id() -> ClockId {
    ClockId::CLOCK_REALTIME
}
