//! Process measurement and supervision (macOS): resource usage through `proc_pid_rusage`,
//! descendants through `ps`, and guards that never leave a process behind.
#![allow(unsafe_code)]

use std::os::unix::process::ExitStatusExt as _;
use std::process::{Child, Command, ExitStatus};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

/// One sample of a process's resource use.
#[derive(Debug, Clone, Copy)]
pub struct Usage {
    /// Resident set size (what `ps -o rss` shows).
    pub rss_bytes: u64,
    /// Physical footprint (what Activity Monitor's "Memory" column shows).
    pub footprint_bytes: u64,
    /// User + system CPU time since the process started.
    pub cpu: Duration,
    /// Package idle wakeups since the process started.
    pub wakeups: u64,
}

/// Mach absolute time → nanoseconds is `ticks * numer / denom` (125/3 on Apple Silicon).
// `libc` deprecates the mach calls in favor of the `mach2` crate; one call is not worth a dependency.
#[allow(deprecated)]
static TIMEBASE: LazyLock<(u64, u64)> = LazyLock::new(|| {
    let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
    // SAFETY: `info` is a valid out pointer for the duration of the call.
    let rc = unsafe { libc::mach_timebase_info(&mut info) };
    if rc == 0 && info.denom != 0 {
        (u64::from(info.numer), u64::from(info.denom))
    } else {
        (1, 1)
    }
});

fn ticks_to_duration(ticks: u64) -> Duration {
    let (numer, denom) = *TIMEBASE;
    let nanos = u128::from(ticks) * u128::from(numer) / u128::from(denom);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// Resource usage of `pid`.
pub fn usage(pid: u32) -> Result<Usage> {
    // SAFETY: `rusage_info_v4` is plain old data; all-zero is a valid value.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: `proc_pid_rusage` writes one `rusage_info_v4` for flavor `RUSAGE_INFO_V4`
    // through the pointer, which is valid for that size.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V4,
            std::ptr::from_mut(&mut info).cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        bail!(
            "proc_pid_rusage({pid}) failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(Usage {
        rss_bytes: info.ri_resident_size,
        footprint_bytes: info.ri_phys_footprint,
        cpu: ticks_to_duration(info.ri_user_time.saturating_add(info.ri_system_time)),
        wakeups: info.ri_pkg_idle_wkups,
    })
}

/// Whether a process with this pid exists (signal 0).
pub fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Sends `sig` to `pid`.
pub fn signal(pid: u32, sig: libc::c_int) {
    // SAFETY: plain `kill(2)` on a pid we spawned, found as a descendant of one, or found as a
    // leaked `hutand` of this checkout.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

/// Every transitive child of `root` (from one `ps` snapshot).
pub fn descendants(root: u32) -> Vec<u32> {
    let Ok(out) = Command::new("ps").args(["-axo", "pid=,ppid="]).output() else {
        return Vec::new();
    };
    let table: Vec<(u32, u32)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let mut found = vec![root];
    let mut next = 0;
    while let Some(&parent) = found.get(next) {
        next += 1;
        for (pid, ppid) in &table {
            if *ppid == parent && !found.contains(pid) {
                found.push(*pid);
            }
        }
    }
    found.remove(0);
    found
}

/// A spawned process that is terminated (with its descendants) when dropped.
#[derive(Debug)]
pub struct Guard {
    name: &'static str,
    child: Child,
    /// Descendants seen while it ran, killed with it.
    seen: Vec<u32>,
}

impl Guard {
    pub fn spawn(name: &'static str, command: &mut Command) -> Result<Self> {
        let child = command
            .spawn()
            .map_err(|e| anyhow::anyhow!("cannot start {name}: {e}"))?;
        Ok(Self {
            name,
            child,
            seen: Vec::new(),
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Whether the process is still running.
    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Fails when the process exited.
    pub fn ensure_running(&mut self) -> Result<()> {
        match self.child.try_wait()? {
            None => Ok(()),
            Some(status) => bail!("{} exited early ({status})", self.name),
        }
    }

    /// Remembers the current descendants so [`Self::stop`] can verify they are gone even when the
    /// process was stopped some other way first.
    pub fn note_descendants(&mut self) {
        self.seen.extend(descendants(self.pid()));
    }

    /// SIGTERM and wait up to `grace`. A process that ignores it is SIGKILLed and reported as
    /// `forced`; descendants that outlive it (PTY children) are killed too.
    pub fn stop(&mut self, grace: Duration) -> Stop {
        self.seen.extend(descendants(self.pid()));
        let mut forced = false;
        if self.running() {
            signal(self.pid(), libc::SIGTERM);
            let deadline = Instant::now() + grace;
            while self.running() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if self.running() {
                forced = true;
                let _ = self.child.kill();
            }
        }
        let exit = self.child.wait().ok();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            self.seen.retain(|pid| alive(*pid));
            if self.seen.is_empty() || Instant::now() >= deadline {
                break;
            }
            for pid in &self.seen {
                signal(*pid, libc::SIGKILL);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Stop {
            name: self.name,
            exit,
            forced,
            leaked: std::mem::take(&mut self.seen),
        }
    }
}

/// How [`Guard::stop`] ended a process.
#[derive(Debug)]
pub struct Stop {
    name: &'static str,
    exit: Option<ExitStatus>,
    forced: bool,
    leaked: Vec<u32>,
}

impl Stop {
    /// Err when the process had to be SIGKILLed, died of anything but exit 0 or the SIGTERM we
    /// sent (a crash: abort, segfault), or left processes behind.
    pub fn check(&self) -> Result<()> {
        if self.forced {
            bail!("{} ignored SIGTERM and was killed", self.name);
        }
        match self.exit {
            Some(status) if status.success() || status.signal() == Some(libc::SIGTERM) => {}
            other => bail!("{} did not stop cleanly: {other:?}", self.name),
        }
        if !self.leaked.is_empty() {
            bail!("{} left processes behind: {:?}", self.name, self.leaked);
        }
        Ok(())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Err(err) = self.stop(Duration::from_secs(3)).check() {
            eprintln!("xtask: {err}");
        }
    }
}
