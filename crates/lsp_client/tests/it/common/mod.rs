//! Starting the fake server and waiting on what it does. Every wait is on an event or on the
//! process table with an upper bound that only turns a hang into a failure.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_lsp_client::ls_types::Uri;
use hutan_lsp_client::{
    Launch, RestartPolicy, Server, ServerConfig, ServerEvent, Subscription, Timeouts,
};
use serde_json::Value;
use tempfile::TempDir;

/// Upper bound of any wait in these tests.
pub const PATIENCE: Duration = Duration::from_secs(20);

pub struct Fake {
    pub server: Server,
    pub events: Subscription,
    pub dir: TempDir,
}

impl Fake {
    pub fn uri(&self, name: &str) -> Uri {
        uri(self.dir.path(), name)
    }
}

pub fn uri(dir: &Path, name: &str) -> Uri {
    Uri::from_file_path(dir.join(name)).expect("absolute path")
}

/// A config that runs the fake server with `script` in a fresh directory.
pub fn config(script: &Value) -> (ServerConfig, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path: PathBuf = dir.path().join("script.json");
    std::fs::write(&path, script.to_string()).expect("write script");
    let launch = Launch {
        program: env!("CARGO_BIN_EXE_hutan-fake-lsp").into(),
        args: vec!["--script".to_owned(), path.display().to_string()],
        cwd: Some(dir.path().to_owned()),
        env: Vec::new(),
    };
    let mut config = ServerConfig::new("fake", launch, dir.path());
    config.timeouts = Timeouts {
        initialize: Duration::from_secs(10),
        request: Duration::from_secs(10),
        interactive: Duration::from_secs(10),
        shutdown: Duration::from_secs(2),
        exit: Duration::from_secs(2),
    };
    config.restart = RestartPolicy {
        max_failures: 3,
        window: Duration::from_secs(60),
        initial_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_secs(1),
    };
    (config, dir)
}

pub fn start_with(config: ServerConfig, dir: TempDir) -> Fake {
    let server = Server::start(config);
    // On the test's current-thread runtime nothing ran yet: no event is missed.
    let events = server.subscribe().expect("subscribe");
    Fake {
        server,
        events,
        dir,
    }
}

pub fn start(script: &Value) -> Fake {
    let (config, dir) = config(script);
    start_with(config, dir)
}

/// The next event matching `want`, skipping others.
pub async fn event(
    events: &mut Subscription,
    mut want: impl FnMut(&ServerEvent) -> bool,
) -> ServerEvent {
    tokio::time::timeout(PATIENCE, async {
        loop {
            match events.recv().await {
                Some(event) if want(&event) => return event,
                Some(_) => {}
                None => panic!("the server's events ended"),
            }
        }
    })
    .await
    .expect("event within the patience")
}

/// Waits for a log line containing `needle` and returns it.
pub async fn log_line(fake: &mut Fake, needle: &str) -> String {
    let found = |fake: &Fake| {
        fake.server
            .log()
            .into_iter()
            .find(|entry| entry.message.contains(needle))
            .map(|entry| entry.message)
    };
    loop {
        if let Some(line) = found(fake) {
            return line;
        }
        let event = event(&mut fake.events, |event| match event {
            ServerEvent::Log(entry) => entry.message.contains(needle),
            // Missed events: the log ring has them.
            ServerEvent::Lagged(_) => true,
            _ => false,
        })
        .await;
        if let ServerEvent::Log(entry) = event {
            return entry.message;
        }
    }
}

pub async fn ready(fake: &Fake) {
    let status = tokio::time::timeout(
        PATIENCE,
        fake.server.wait_for(|s| {
            matches!(
                s.state,
                hutan_lsp_client::ServerState::Ready | hutan_lsp_client::ServerState::Indexing
            ) || s.state.is_final()
        }),
    )
    .await
    .expect("server ready within the patience");
    assert!(
        !status.state.is_final(),
        "server ended instead of starting: {status:?}"
    );
}

/// Whether `pid` is gone (a zombie counts as gone: it only waits for its parent's reaper).
pub fn gone(pid: u32) -> bool {
    hutan_terminal::procs::inspect(pid)
        .is_none_or(|p| p.state == hutan_terminal::procs::ProcState::Zombie)
}

/// Waits until `pid` left the process table (polling it: the kernel offers no event).
pub async fn wait_gone(pid: u32) {
    tokio::time::timeout(PATIENCE, async {
        while !gone(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("process {pid} still running"));
}
