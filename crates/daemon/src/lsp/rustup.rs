//! rustup's proxies. The `rust-analyzer` found on `PATH` is often rustup's proxy (`~/.cargo/bin`,
//! Homebrew's rustup): it runs the tool of the folder's toolchain, and only when that toolchain
//! has the component. Without it the proxy fails ("Unknown binary 'rust-analyzer' in official
//! toolchain …", or "infinite recursion detected" when the toolchain's own `rust-analyzer` is a
//! proxy again), so started as a server it would end at once and crash-loop to `failed`.
//!
//! So a program that is a proxy is asked about before it is used: `rustup which <name>` in the
//! folder (rustup reads `rust-toolchain.toml` from there). When rustup has no such tool, or names
//! a proxy again, the server is `not_installed` and says to run `rustup component add <name>`.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::convert::clip;

/// How long `rustup which` may take (it reads the toolchain settings; no network).
const WHICH_TIMEOUT: Duration = Duration::from_secs(10);

/// The `rustup` that `program` is a proxy of: a `rustup` beside it that is the same file
/// (rustup installs its proxies as hard or symbolic links to itself).
fn proxy_of(program: &Path) -> Option<PathBuf> {
    let rustup = program.parent()?.join("rustup");
    (rustup != program && same_file(program, &rustup)).then_some(rustup)
}

/// Whether `a` and `b` are one file (following links).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Why `program` cannot serve when it is a rustup proxy whose toolchain (for `dir`) lacks the
/// tool; `None` when it is no proxy, when rustup names the tool, or when rustup could not be
/// asked (the server then starts and fails, if it does, as any other).
pub fn missing_component(
    program: &Path,
    env: &BTreeMap<String, String>,
    dir: &Path,
) -> Option<String> {
    let rustup = proxy_of(program)?;
    let name = program.file_name()?.to_str()?;
    let (ok, stdout, stderr) = match which(&rustup, name, env, dir) {
        Ok(answer) => answer,
        Err(error) => {
            tracing::debug!(%error, rustup = %rustup.display(), "cannot ask rustup which");
            return None;
        }
    };
    let add = format!("rustup component add {name}");
    if !ok {
        let said = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| clip(line, 300))
            .unwrap_or_else(|| "rustup which failed".to_owned());
        return Some(format!(
            "`{}` is rustup's proxy and the toolchain has no {name} ({said}); install it with `{add}`",
            program.display()
        ));
    }
    let target = PathBuf::from(stdout.trim());
    if !target.is_file() || same_file(&target, &rustup) {
        return Some(format!(
            "`{}` is rustup's proxy and the toolchain's {name} is not the tool itself ({}); install it with `{add}`",
            program.display(),
            target.display()
        ));
    }
    None
}

/// `rustup which <name>` in `dir`: whether it succeeded, its output and its errors.
fn which(
    rustup: &Path,
    name: &str,
    env: &BTreeMap<String, String>,
    dir: &Path,
) -> std::io::Result<(bool, String, String)> {
    let mut child = Command::new(rustup)
        .arg("which")
        .arg(name)
        .envs(env)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + WHICH_TIMEOUT;
    let status = loop {
        // The answer is a line or two: it fits the pipes, so waiting before reading is safe.
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "rustup which did not answer",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut stdout)?;
    }
    if let Some(mut err) = child.stderr.take() {
        err.read_to_string(&mut stderr)?;
    }
    Ok((status.success(), stdout, stderr))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    /// A folder with a fake `rustup` (answering `which` with `which_answer`, a shell snippet)
    /// and `rust-analyzer` linked to it, as rustup installs its proxies.
    fn proxy(which_answer: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let rustup = dir.path().join("rustup");
        std::fs::write(
            &rustup,
            format!(
                "#!/bin/sh\n\
                 case \"$(basename \"$0\")\" in rustup) ;; *) touch \"$(dirname \"$0\")/ran\"; exit 1;; esac\n\
                 [ \"$1\" = which ] || exit 2\n\
                 {which_answer}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755)).unwrap();
        let proxy = dir.path().join("rust-analyzer");
        std::os::unix::fs::symlink(&rustup, &proxy).unwrap();
        (dir, proxy)
    }

    #[test]
    fn a_proxy_without_the_component_is_missing_and_never_runs() {
        let (dir, program) = proxy(
            "echo \"error: Unknown binary '$2' in official toolchain 'stable-aarch64-apple-darwin'.\" >&2; exit 1",
        );
        let reason = missing_component(&program, &BTreeMap::new(), dir.path()).unwrap();
        assert!(reason.contains("rustup component add rust-analyzer"), "{reason}");
        assert!(reason.contains("Unknown binary 'rust-analyzer'"), "{reason}");
        assert!(!dir.path().join("ran").exists(), "the proxy itself never ran");
    }

    #[test]
    fn a_proxy_that_names_itself_would_recurse() {
        let (dir, program) = proxy("echo \"$(dirname \"$0\")/rustup\"");
        let reason = missing_component(&program, &BTreeMap::new(), dir.path()).unwrap();
        assert!(reason.contains("rustup component add rust-analyzer"), "{reason}");
    }

    #[test]
    fn a_proxy_with_the_component_and_a_plain_program_serve() {
        let (dir, program) = proxy("echo /bin/sh");
        assert_eq!(missing_component(&program, &BTreeMap::new(), dir.path()), None);
        // Not a proxy: nothing is asked.
        let plain = dir.path().join("plain");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        assert_eq!(missing_component(&plain, &BTreeMap::new(), dir.path()), None);
        assert_eq!(proxy_of(&dir.path().join("rustup")), None);
    }
}
