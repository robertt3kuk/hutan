//! How a request reaches the window itself (UX-ATTN-3): a toast at the bottom right and a dot on
//! the session's tab. Pure data, no GPUI: every method takes the time it happens at, so the
//! rules (two toasts at most, the oldest replaced, five seconds, hover pauses) are tested without
//! a clock.
//!
//! The daemon decides *that* something calls (`notify.intent`: not for a session some client is
//! looking at, never for a muted one); this decides how the window says so.

use std::time::{Duration, Instant};

use hutan_core::SessionId;
use hutan_protocol::notify::IntentKind;

/// Most toasts on screen; a newer one replaces the oldest.
pub const MAX_TOASTS: usize = 2;
/// How long a toast stays when nobody hovers it.
pub const TOAST_TIME: Duration = Duration::from_secs(5);
/// Longest toast text, in characters.
const TEXT_MAX_CHARS: usize = 100;

/// Whether an intent of `kind` is a request (something waits for an answer): the only calls that
/// get a toast and a tab dot.
pub const fn is_request(kind: IntentKind) -> bool {
    matches!(
        kind,
        IntentKind::ApprovalRequired | IntentKind::QuestionRequired | IntentKind::AgentBlocked
    )
}

/// One toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub session: SessionId,
    pub text: String,
}

/// Where a toast's time stands.
#[derive(Debug, Clone, Copy)]
enum Clock {
    /// Goes at this time.
    Running(Instant),
    /// Hovered: this much is left, and it stops counting.
    Paused(Duration),
}

/// The toasts on screen, oldest first.
#[derive(Debug, Default)]
pub struct ToastStack {
    entries: Vec<(Toast, Clock)>,
    hovered: Option<u64>,
    next_id: u64,
}

impl ToastStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows `text` for `session` from `now`: a toast the session already has is replaced (its
    /// time starts over), and past [`MAX_TOASTS`] the oldest goes. Returns the new toast's id.
    pub fn push(&mut self, now: Instant, session: SessionId, text: String) -> u64 {
        self.remove_where(|toast| toast.session == session);
        self.next_id += 1;
        let id = self.next_id;
        self.entries.push((
            Toast { id, session, text },
            Clock::Running(now + TOAST_TIME),
        ));
        while self.entries.len() > MAX_TOASTS {
            let oldest = self.entries[0].0.id;
            self.remove_where(|toast| toast.id == oldest);
        }
        id
    }

    /// Takes off the toasts whose time is up at `now`; they are returned.
    pub fn expire(&mut self, now: Instant) -> Vec<Toast> {
        let mut gone = Vec::new();
        self.entries.retain(|(toast, clock)| match clock {
            Clock::Running(deadline) if *deadline <= now => {
                gone.push(toast.clone());
                false
            }
            _ => true,
        });
        gone
    }

    /// When the next toast goes (none while every one is hovered or there are none).
    pub fn next_deadline(&self) -> Option<Instant> {
        self.entries
            .iter()
            .filter_map(|(_, clock)| match clock {
                Clock::Running(deadline) => Some(*deadline),
                Clock::Paused(_) => None,
            })
            .min()
    }

    /// The pointer is over toast `over` (`None`: over none). The one it was over resumes with
    /// the time it had left; the one it is over stops counting.
    pub fn hover(&mut self, now: Instant, over: Option<u64>) {
        if self.hovered == over {
            return;
        }
        let before = std::mem::replace(&mut self.hovered, over);
        for (toast, clock) in &mut self.entries {
            if Some(toast.id) == before
                && let Clock::Paused(left) = *clock
            {
                *clock = Clock::Running(now + left);
            }
            if Some(toast.id) == over
                && let Clock::Running(deadline) = *clock
            {
                *clock = Clock::Paused(deadline.saturating_duration_since(now));
            }
        }
    }

    /// Takes toast `id` off; whether it was there.
    pub fn dismiss(&mut self, id: u64) -> bool {
        self.remove_where(|toast| toast.id == id) > 0
    }

    /// Takes every toast off; how many there were.
    pub fn dismiss_all(&mut self) -> usize {
        self.remove_where(|_| true)
    }

    /// Takes the toasts of `session` off (it was looked at, or its request was answered).
    pub fn dismiss_session(&mut self, session: SessionId) -> bool {
        self.remove_where(|toast| toast.session == session) > 0
    }

    /// The toasts, oldest first.
    pub fn toasts(&self) -> impl Iterator<Item = &Toast> {
        self.entries.iter().map(|(toast, _)| toast)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The newest toast: what *Review* opens.
    pub fn newest(&self) -> Option<&Toast> {
        self.entries.last().map(|(toast, _)| toast)
    }

    fn remove_where(&mut self, mut gone: impl FnMut(&Toast) -> bool) -> usize {
        let before = self.entries.len();
        let hovered = &mut self.hovered;
        self.entries.retain(|(toast, _)| {
            let go = gone(toast);
            if go && *hovered == Some(toast.id) {
                *hovered = None;
            }
            !go
        });
        before - self.entries.len()
    }
}

/// What a toast says: who calls and for what. `what` is the request's own words (an approval's
/// title, a question's title, a blocked agent's message); empty when there are none.
pub fn describe(who: &str, kind: IntentKind, what: &str) -> String {
    let what = what.trim();
    let text = match kind {
        IntentKind::ApprovalRequired => match what.trim_end_matches(['?', '.']).trim() {
            "" => format!("{who} asks for permission"),
            action => format!("{who} asks to {}", lower_first(action)),
        },
        IntentKind::QuestionRequired if what.is_empty() => format!("{who} has a question"),
        IntentKind::QuestionRequired => format!("{who} asks: {what}"),
        _ if what.is_empty() => format!("{who} is waiting for you"),
        _ => format!("{who} is waiting for you: {what}"),
    };
    one_line(&text)
}

/// `Edit src/lib.rs` → `edit src/lib.rs`; a word that is not plain prose (`rm`, `API`) stays.
fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first)
            if first.is_ascii_uppercase()
                && chars.clone().next().is_some_and(|c| c.is_ascii_lowercase()) =>
        {
            let mut out = String::with_capacity(text.len());
            out.push(first.to_ascii_lowercase());
            out.push_str(chars.as_str());
            out
        }
        _ => text.to_owned(),
    }
}

/// One line, cut to [`TEXT_MAX_CHARS`] with an ellipsis.
fn one_line(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= TEXT_MAX_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(TEXT_MAX_CHARS - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionId {
        SessionId::new()
    }

    #[test]
    fn only_requests_call_the_window() {
        for kind in IntentKind::ALL {
            let expected = matches!(
                kind,
                IntentKind::ApprovalRequired
                    | IntentKind::QuestionRequired
                    | IntentKind::AgentBlocked
            );
            assert_eq!(is_request(kind), expected, "{kind:?}");
        }
    }

    #[test]
    fn at_most_two_toasts_and_the_oldest_is_replaced() {
        let now = Instant::now();
        let mut stack = ToastStack::new();
        let (a, b, c) = (session(), session(), session());
        stack.push(now, a, "a".into());
        stack.push(now, b, "b".into());
        stack.push(now, c, "c".into());
        let shown: Vec<_> = stack.toasts().map(|t| t.session).collect();
        assert_eq!(shown, [b, c]);
        assert_eq!(stack.newest().unwrap().session, c);
    }

    #[test]
    fn a_second_request_of_a_session_replaces_its_toast_and_restarts_the_time() {
        let now = Instant::now();
        let mut stack = ToastStack::new();
        let a = session();
        stack.push(now, a, "first".into());
        stack.push(now + Duration::from_secs(4), a, "second".into());
        assert_eq!(stack.len(), 1);
        assert_eq!(stack.newest().unwrap().text, "second");
        // The first one's five seconds are over, the second's are not.
        assert!(stack.expire(now + TOAST_TIME).is_empty());
        assert_eq!(stack.expire(now + Duration::from_secs(9)).len(), 1);
    }

    #[test]
    fn a_toast_goes_after_five_seconds_each_on_its_own_time() {
        let now = Instant::now();
        let mut stack = ToastStack::new();
        let (a, b) = (session(), session());
        stack.push(now, a, "a".into());
        stack.push(now + Duration::from_secs(2), b, "b".into());
        assert_eq!(stack.next_deadline(), Some(now + TOAST_TIME));
        assert!(
            stack
                .expire(now + TOAST_TIME - Duration::from_millis(1))
                .is_empty()
        );
        let gone = stack.expire(now + TOAST_TIME);
        assert_eq!(gone.iter().map(|t| t.session).collect::<Vec<_>>(), [a]);
        assert_eq!(
            stack.next_deadline(),
            Some(now + Duration::from_secs(2) + TOAST_TIME)
        );
        assert_eq!(stack.expire(now + Duration::from_secs(7)).len(), 1);
        assert!(stack.is_empty());
        assert_eq!(stack.next_deadline(), None);
    }

    #[test]
    fn hovering_pauses_a_toast_and_leaving_gives_back_the_time_it_had_left() {
        let now = Instant::now();
        let mut stack = ToastStack::new();
        let a = session();
        let id = stack.push(now, a, "a".into());
        // Two seconds in, the pointer comes over it: three seconds are left.
        stack.hover(now + Duration::from_secs(2), Some(id));
        assert_eq!(
            stack.next_deadline(),
            None,
            "a hovered toast has no deadline"
        );
        assert!(stack.expire(now + Duration::from_secs(60)).is_empty());
        // It leaves a minute later: the three seconds start again from then.
        let left = now + Duration::from_secs(62);
        stack.hover(left, None);
        assert_eq!(stack.next_deadline(), Some(left + Duration::from_secs(3)));
        assert!(stack.expire(left + Duration::from_millis(2999)).is_empty());
        assert_eq!(stack.expire(left + Duration::from_secs(3)).len(), 1);
    }

    #[test]
    fn moving_from_one_toast_to_the_other_swaps_which_one_waits() {
        let now = Instant::now();
        let mut stack = ToastStack::new();
        let (a, b) = (session(), session());
        let first = stack.push(now, a, "a".into());
        let second = stack.push(now, b, "b".into());
        stack.hover(now + Duration::from_secs(1), Some(first));
        stack.hover(now + Duration::from_secs(2), Some(second));
        // `first` runs again with its four seconds, `second` waits with its three.
        assert_eq!(
            stack.next_deadline(),
            Some(now + Duration::from_secs(2) + Duration::from_secs(4))
        );
    }

    #[test]
    fn dismissing_takes_toasts_off_and_forgets_the_hover() {
        let now = Instant::now();
        let mut stack = ToastStack::new();
        let (a, b) = (session(), session());
        let first = stack.push(now, a, "a".into());
        stack.push(now, b, "b".into());
        stack.hover(now, Some(first));
        assert!(stack.dismiss(first));
        assert!(!stack.dismiss(first));
        // The hover went with its toast: the other one's deadline still counts.
        assert_eq!(stack.next_deadline(), Some(now + TOAST_TIME));
        assert!(stack.dismiss_session(b));
        assert!(!stack.dismiss_session(b));
        stack.push(now, a, "again".into());
        assert_eq!(stack.dismiss_all(), 1);
        assert!(stack.is_empty());
    }

    #[test]
    fn a_toast_says_who_asks_for_what() {
        use IntentKind::*;
        assert_eq!(
            describe("Claude Code", ApprovalRequired, "Edit src/lib.rs?"),
            "Claude Code asks to edit src/lib.rs"
        );
        // Not prose: kept as it is.
        assert_eq!(
            describe("Codex", ApprovalRequired, "rm -rf build"),
            "Codex asks to rm -rf build"
        );
        assert_eq!(
            describe("Codex", ApprovalRequired, "  "),
            "Codex asks for permission"
        );
        assert_eq!(
            describe("OMP", QuestionRequired, "Which branch?"),
            "OMP asks: Which branch?"
        );
        assert_eq!(describe("OMP", QuestionRequired, ""), "OMP has a question");
        assert_eq!(
            describe("api", AgentBlocked, "wants to run npm install"),
            "api is waiting for you: wants to run npm install"
        );
        assert_eq!(describe("api", AgentBlocked, ""), "api is waiting for you");
    }

    #[test]
    fn a_long_text_is_one_line_cut_with_an_ellipsis() {
        let long = format!("Run `{}`\nand more", "x".repeat(300));
        let text = describe("Codex", IntentKind::ApprovalRequired, &long);
        assert_eq!(text.chars().count(), TEXT_MAX_CHARS);
        assert!(text.ends_with('…') && !text.contains('\n'));
    }
}
