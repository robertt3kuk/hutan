// Compiles the git commit of the checkout into `HUTAN_GIT_SHA` (12 hex digits; empty outside
// git), the build id clients and the daemon compare (`docs/architecture.md` § Daemon
// lifecycle). Also the daemon's build script (`crates/daemon/build.rs` includes this file), so
// both sides of one build carry the same id. A set `HUTAN_GIT_SHA` wins (a build from a source
// archive).

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (output.status.success() && !text.is_empty()).then(|| text.to_owned())
}

/// Builds again when the commit changes: `HEAD`, the branch it names, `packed-refs`.
fn watch(name: &str) {
    if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", name])
        && Path::new(&path).exists()
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=HUTAN_GIT_SHA");
    let sha = std::env::var("HUTAN_GIT_SHA")
        .ok()
        .filter(|sha| !sha.is_empty())
        .or_else(|| git(&["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_default();
    watch("HEAD");
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watch(&branch);
    }
    watch("packed-refs");
    // The worktree's own reflog moves on every commit, rebase or checkout made in it, also
    // when the branch ref itself is packed or lives in the common dir of a linked worktree.
    watch("logs/HEAD");
    println!("cargo:rustc-env=HUTAN_GIT_SHA={sha}");
}
