//! [`FakeStatusItem`] and [`FakeLauncher`]: in-memory stand-ins for the menu bar and for "open
//! Hutan.app" that record every call and let a test choose a menu row. Behind the `test-support`
//! feature (this crate's integration tests) and `cfg(test)`.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};

use super::{
    AppLauncher, CHOICE_QUEUE, MenuBar, MenuChoice, MenuModel, StatusItemHost, StopAnswer,
};

/// What the fake menu bar did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Show(MenuModel),
    Hide,
    ConfirmStop(u32),
}

#[derive(Debug)]
pub struct FakeStatusItem {
    calls: Mutex<Vec<Call>>,
    /// The content on the menu bar now; `None` while the item is not there.
    shown: Mutex<Option<MenuModel>>,
    /// What the user answers to a stop confirmation; `None` leaves it asked (the dialog is up).
    answer: Mutex<Option<StopAnswer>>,
    held: Mutex<Vec<oneshot::Sender<StopAnswer>>>,
    clicks: mpsc::Sender<MenuChoice>,
    choices: Mutex<Option<mpsc::Receiver<MenuChoice>>>,
}

impl FakeStatusItem {
    pub fn new() -> Arc<Self> {
        let (clicks, choices) = mpsc::channel(CHOICE_QUEUE);
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            shown: Mutex::new(None),
            answer: Mutex::new(Some(StopAnswer::Cancel)),
            held: Mutex::new(Vec::new()),
            clicks,
            choices: Mutex::new(Some(choices)),
        })
    }

    /// Everything the fake was asked, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().clone()
    }

    /// The item as it is on the menu bar now; `None` while it is not there.
    pub fn shown(&self) -> Option<MenuModel> {
        self.shown.lock().clone()
    }

    /// How many times content was put on the menu bar.
    pub fn shows(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| matches!(call, Call::Show(_)))
            .count()
    }

    /// The user's answer to the next stop confirmations; `None` keeps the dialog up.
    pub fn answer_with(&self, answer: Option<StopAnswer>) {
        *self.answer.lock() = answer;
    }

    /// Answers the confirmations that were left up.
    pub fn answer_held(&self, answer: StopAnswer) {
        for held in self.held.lock().drain(..) {
            let _ = held.send(answer);
        }
    }

    /// The user chose a row of the menu.
    pub fn click(&self, choice: MenuChoice) {
        let _ = self.clicks.try_send(choice);
    }

    /// Polls until `done` holds for the calls so far; panics after ten seconds.
    pub async fn wait_for(&self, what: &str, done: impl Fn(&[Call]) -> bool) -> Vec<Call> {
        for _ in 0..1000 {
            let calls = self.calls();
            if done(&calls) {
                return calls;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}; calls: {:?}", self.calls());
    }

    /// Polls until the item is on the menu bar with content `done` accepts.
    pub async fn wait_shown(&self, what: &str, done: impl Fn(&MenuModel) -> bool) -> MenuModel {
        for _ in 0..1000 {
            if let Some(model) = self.shown().filter(&done) {
                return model;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "timed out waiting for {what}; shown: {:?}, calls: {:?}",
            self.shown(),
            self.calls()
        );
    }

    /// Polls until the item is off the menu bar.
    pub async fn wait_hidden(&self, what: &str) {
        for _ in 0..1000 {
            if self.shown().is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}; calls: {:?}", self.calls());
    }
}

impl StatusItemHost for FakeStatusItem {
    fn show(&self, model: &MenuModel) {
        self.calls.lock().push(Call::Show(model.clone()));
        *self.shown.lock() = Some(model.clone());
    }

    fn hide(&self) {
        self.calls.lock().push(Call::Hide);
        *self.shown.lock() = None;
    }

    fn confirm_stop(&self, live_sessions: u32) -> oneshot::Receiver<StopAnswer> {
        self.calls.lock().push(Call::ConfirmStop(live_sessions));
        let (tx, rx) = oneshot::channel();
        match *self.answer.lock() {
            Some(answer) => {
                let _ = tx.send(answer);
            }
            None => self.held.lock().push(tx),
        }
        rx
    }

    fn choices(&self) -> Option<mpsc::Receiver<MenuChoice>> {
        self.choices.lock().take()
    }
}

/// Counts how many times Hutan was asked to open.
#[derive(Debug, Default)]
pub struct FakeLauncher {
    opened: Mutex<usize>,
}

impl FakeLauncher {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    pub fn opened(&self) -> usize {
        *self.opened.lock()
    }
}

impl AppLauncher for FakeLauncher {
    fn open_hutan(&self) {
        *self.opened.lock() += 1;
    }
}

/// A [`MenuBar`] on fakes, with the show delay and the hand-off patience tests need.
pub fn menu_bar(
    item: &Arc<FakeStatusItem>,
    launcher: &Arc<FakeLauncher>,
    show_delay: Duration,
    open_patience: Duration,
) -> MenuBar {
    MenuBar {
        host: item.clone(),
        launcher: launcher.clone(),
        show_delay,
        open_patience,
        desktop_poll: show_delay,
    }
}
