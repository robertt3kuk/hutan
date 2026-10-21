//! `xtask watch-daemons -- <command>`: the gate's guard against leaked test daemons. Runs the
//! command (the test suite), then fails when a `hutand` of this checkout's target directory that
//! was not running before the command is still running after it, and stops those (SIGTERM, then
//! SIGKILL). A test daemon that outlives its test is a bug in that test's harness: every one is
//! started on a temporary `HUTAN_HOME` and must be stopped by the test, also when it fails.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

use crate::proc;

/// How long daemons that tests stopped get to finish exiting before they count as leaked.
const SETTLE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(100);
/// How long a leaked daemon gets after SIGTERM before SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(5);

/// A `hutand` process: its pid and command line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Daemon {
    pid: u32,
    command: String,
}

/// Runs `command`; returns the exit code for xtask: the command's, or 1 when daemons leaked.
pub fn run(command: &[String]) -> Result<i32> {
    let Some((program, args)) = command.split_first() else {
        bail!("watch-daemons needs a command to run: `xtask watch-daemons -- cargo nextest run`");
    };
    let target = crate::bundle::target_dir(&crate::bundle::workspace_root());
    let target = target.canonicalize().unwrap_or(target);
    let before: BTreeSet<u32> = daemons(&target)?.into_iter().map(|d| d.pid).collect();
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {program}"))?;

    let deadline = Instant::now() + SETTLE;
    let leaked = loop {
        let left: Vec<Daemon> = daemons(&target)?
            .into_iter()
            .filter(|d| !before.contains(&d.pid))
            .collect();
        if left.is_empty() || Instant::now() >= deadline {
            break left;
        }
        std::thread::sleep(POLL);
    };
    if leaked.is_empty() {
        return Ok(status.code().unwrap_or(1));
    }
    eprintln!(
        "xtask: {} test daemon(s) outlived the run (a test started them and did not stop them):",
        leaked.len()
    );
    for daemon in &leaked {
        eprintln!("  pid {}: {}", daemon.pid, daemon.command);
        for open in open_files(daemon.pid) {
            eprintln!("    {open}");
        }
        stop(daemon.pid);
    }
    Ok(1)
}

/// Every running (not zombie) process whose executable is a `hutand` under `target`.
fn daemons(target: &Path) -> Result<Vec<Daemon>> {
    let out = Command::new("ps")
        .args(["-axww", "-o", "pid=,stat=,command="])
        .output()
        .context("running ps")?;
    if !out.status.success() {
        bail!("ps failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(parse(&String::from_utf8_lossy(&out.stdout), target))
}

fn parse(table: &str, target: &Path) -> Vec<Daemon> {
    let prefix = format!("{}/", target.display());
    table
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, rest) = line.split_once(char::is_whitespace)?;
            let (stat, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let command = command.trim();
            let executable = command.strip_prefix(&prefix)?.split(' ').next()?;
            let ours = Path::new(executable).file_name()? == "hutand" && !stat.starts_with('Z');
            let pid = pid.parse().ok()?;
            ours.then(|| Daemon {
                pid,
                command: command.to_owned(),
            })
        })
        .collect()
}

/// The files a leaked daemon has open under its home (its log and database name the test's
/// temporary `HUTAN_HOME`), for the report.
fn open_files(pid: u32) -> Vec<String> {
    let Ok(out) = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd,3-20", "-Fn"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .filter(|name| name.starts_with('/') && !name.starts_with("/dev/"))
        .map(str::to_owned)
        .collect()
}

fn stop(pid: u32) {
    for signal in [libc::SIGTERM, libc::SIGKILL] {
        if !proc::alive(pid) {
            return;
        }
        proc::signal(pid, signal);
        let deadline = Instant::now() + TERM_GRACE;
        while proc::alive(pid) && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_running_hutand_processes_of_the_target_count() {
        let target = Path::new("/w/target");
        let table = "\
  101 Ss   /w/target/debug/hutand
  102 S    /w/target/debug/hutan-desktop
  103 Z    /w/target/debug/hutand
  104 S    /other/target/debug/hutand
  105 S    /w/target/bundle/Hutan.app/Contents/MacOS/hutand --version
  106 S    /bin/sh /w/target/debug/hutand-wrapper.sh
  107 S    /w/target/debug/hutandx
";
        assert_eq!(
            parse(table, target),
            [
                Daemon {
                    pid: 101,
                    command: "/w/target/debug/hutand".into()
                },
                Daemon {
                    pid: 105,
                    command: "/w/target/bundle/Hutan.app/Contents/MacOS/hutand --version".into()
                },
            ]
        );
    }
}
