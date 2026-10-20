//! Text providers that let a test hold the parser at a known point.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use hutan_syntax::TextProvider;

/// A latch: `wait` blocks until `open`.
#[derive(Default)]
pub struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    pub fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }
}

/// Blocks the first read of its text until `gate` opens, after announcing on `entered`.
pub struct Gated {
    pub text: Arc<str>,
    pub gate: Arc<Gate>,
    pub entered: Arc<Gate>,
    pub first: AtomicBool,
}

impl Gated {
    pub fn new(text: &str, gate: &Arc<Gate>, entered: &Arc<Gate>) -> Self {
        Self {
            text: Arc::from(text),
            gate: Arc::clone(gate),
            entered: Arc::clone(entered),
            first: AtomicBool::new(true),
        }
    }
}

impl TextProvider for Gated {
    fn len(&self) -> usize {
        self.text.len()
    }

    fn chunk(&self, offset: usize) -> &[u8] {
        if self.first.swap(false, Ordering::SeqCst) {
            self.entered.open();
            self.gate.wait();
        }
        self.text.as_bytes().get(offset..).unwrap_or_default()
    }
}

/// Opens `gate` after `after`, so a regression that blocks the caller fails instead of hanging.
pub fn watchdog(gate: &Arc<Gate>, after: Duration) {
    let gate = Arc::clone(gate);
    std::thread::spawn(move || {
        std::thread::sleep(after);
        gate.open();
    });
}
