//! Developer tasks. `cargo run -p xtask --release -- bench` measures the performance budgets;
//! `cargo run -p xtask -- bundle` assembles and ad-hoc signs `Hutan.app`, `-- install` puts a
//! release build into /Applications, `-- icon` renders its icon and `-- menubar-icon` the menu
//! bar glyph (`docs/packaging.md`); `-- watch-daemons -- <command>` runs the test suite and fails
//! when a test daemon outlives it (`just test`).

mod bench;
mod bench_ui;
mod bundle;
mod daemons;
mod fake_codex;
mod icon;
mod install;
mod menubar;
mod proc;
mod release;
mod seed;
mod stats;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Hutan developer tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build release binaries and measure the budgets B1–B6 of docs/vision.md and B3p, B7–B10 of docs/perf.md.
    Bench {
        /// Budgets to measure, comma separated (e.g. `B1,B4`); default all.
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Print the full report as JSON instead of the table.
        #[arg(long)]
        json: bool,
        /// Runs per scenario; the table shows the median.
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// Profile the desktop and daemon with `sample(1)` during the latency scenarios
        /// (output in target/bench/).
        #[arg(long)]
        sample: bool,
        /// Milliseconds between keystrokes of the typing and flood scenarios (B4: 100, B5: 50).
        #[arg(long)]
        key_interval_ms: Option<u64>,
    },
    /// Assemble Hutan.app (desktop, hutand, hutan, LaunchAgent plist, shell integration
    /// scripts) and sign it ad hoc. Nothing is launched or registered.
    Bundle {
        /// Build (and bundle) the release binaries instead of the debug ones.
        #[arg(long)]
        release: bool,
        /// Folder that gets Hutan.app (default: target/bundle).
        #[arg(long, value_name = "DIR")]
        out: Option<std::path::PathBuf>,
        /// Bundle the binaries already built in DIR instead of running cargo build.
        #[arg(long, value_name = "DIR")]
        bin_dir: Option<std::path::PathBuf>,
    },
    /// Build the release bundle and source archives, print SHA-256 and update a local tap.
    /// Does not tag, upload, install, launch or publish anything.
    Release {
        /// Existing Homebrew tap checkout to receive Casks/hutan.rb and Formula/hutan.rb.
        #[arg(long, value_name = "DIR")]
        tap: std::path::PathBuf,
    },
    /// A stand-in for `codex app-server` that the chat benchmarks point the daemon at (reads
    /// JSON-RPC on stdin, never touches the network or a login).
    #[command(hide = true)]
    FakeCodex {
        /// The daemon appends `-c key=value` overrides (its MCP wiring); a stand-in ignores them.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _overrides: Vec<String>,
    },
    /// Render assets/macos/Hutan.icns from the brand icon SVG.
    Icon,
    /// Render the menu bar glyph (assets/macos/MenuBarIcon.png and @2x) from the brand favicon.
    MenubarIcon,
    /// Build a release Hutan.app and install it into /Applications (quits a running Hutan
    /// from there first; never stops sessions).
    Install {
        /// Install the debug build instead of release.
        #[arg(long)]
        debug: bool,
        /// Folder that gets Hutan.app (default: /Applications).
        #[arg(long, value_name = "DIR")]
        dest: Option<std::path::PathBuf>,
    },
    /// Run COMMAND (the test suite), then fail when a `hutand` of this target directory that
    /// started during it is still running, and stop it (SIGTERM, then SIGKILL).
    WatchDaemons {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Bench {
            only,
            json,
            runs,
            sample,
            key_interval_ms,
        } => {
            bench::run(bench::Options {
                only,
                json,
                runs: runs.max(1),
                sample,
                key_interval_ms,
            })
            .await
        }
        Command::Bundle {
            release,
            out,
            bin_dir,
        } => bundle::run(bundle::Options {
            release,
            out,
            bin_dir,
        }),
        Command::FakeCodex { .. } => fake_codex::run(),
        Command::Release { tap } => release::run(tap),
        Command::Icon => icon::run(&bundle::workspace_root()),
        Command::MenubarIcon => menubar::run(&bundle::workspace_root()),
        Command::Install { debug, dest } => install::run(install::Options { debug, dest }),
        Command::WatchDaemons { command } => std::process::exit(daemons::run(&command)?),
    }
}
