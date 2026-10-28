//! What each turn of a chat used, folded from the `usage` events of the turn and the cost the
//! harness reported.

use crate::agent::TokenCounts;

/// Folds the `usage` events and the reported cost of a session's turns into what each turn used.
///
/// The harnesses report differently: Codex and the gateway give a running `total` for the
/// conversation and the `last` request; an ACP harness gives the turn's own counts in the
/// prompt's answer (`last` only), and its `total` is the context size, which is not spend. So a
/// turn's tokens are the growth of `total` over the turn, plus every `last` that came without
/// a `total`. Cost is the growth of the cumulative session cost.
#[derive(Debug, Default, Clone)]
pub struct TurnUsage {
    /// The latest cumulative total, kept across turns as the next turn's baseline.
    total: Option<TokenCounts>,
    /// What `total` was before the turn: known from an earlier turn, or derived from the first
    /// cumulative event of the turn (`total` minus `last`).
    baseline: Option<TokenCounts>,
    /// Counts a harness gave for the turn as such (`last` without `total`).
    direct: Option<TokenCounts>,
    /// The cumulative session cost last reported; `None` before one.
    cost: Option<f64>,
    /// The cost when the turn began; `None` when it is not known (a resumed session that
    /// reports its cost only at the end of the turn).
    cost_before: Option<f64>,
    /// The session began in this daemon's turns: its cost started at zero.
    fresh: bool,
}

impl TurnUsage {
    /// A session whose cost (when it reports one) starts at zero if it is `fresh`, and at an
    /// unknown amount if it continues an earlier conversation.
    pub fn new(fresh: bool) -> Self {
        Self {
            fresh,
            cost_before: fresh.then_some(0.0),
            ..Self::default()
        }
    }

    /// A turn began.
    pub fn turn_started(&mut self) {
        self.baseline = self.total;
        self.direct = None;
        self.cost_before = self.cost.or(self.fresh.then_some(0.0));
    }

    /// A `usage` event of the turn. A `total` that comes without a `last` is not spend (an ACP
    /// harness's `usage_update` says how full the context is), so it is not read.
    pub fn observe(&mut self, total: Option<TokenCounts>, last: Option<TokenCounts>) {
        match (total, last) {
            (Some(total), Some(last)) => {
                if self.baseline.is_none() {
                    self.baseline = Some(minus(&total, &last));
                }
                self.total = Some(total);
            }
            (None, Some(last)) => {
                self.direct = Some(match self.direct {
                    Some(sum) => plus(&sum, &last),
                    None => last,
                });
            }
            (Some(_), None) | (None, None) => {}
        }
    }

    /// The agent reported the cumulative cost of the session so far, in US dollars.
    pub fn cost_reported(&mut self, cost_usd: f64) {
        if cost_usd.is_finite() && cost_usd >= 0.0 {
            self.cost = Some(cost_usd);
        }
    }

    /// The cumulative session cost last reported, if any.
    pub fn cost_so_far(&self) -> Option<f64> {
        self.cost
    }

    /// The turn ended: its tokens (a counter nothing reported is `None`) and cost. The cost
    /// baseline moves to now, so a cost reported between turns is not charged to the next one.
    pub fn turn_ended(&mut self) -> (TokenCounts, Option<f64>) {
        let grown = match (self.total, self.baseline) {
            (Some(total), Some(baseline)) => Some(minus(&total, &baseline)),
            _ => None,
        };
        let tokens = match (grown, self.direct) {
            (Some(a), Some(b)) => merge(&a, &b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => TokenCounts::default(),
        };
        let cost = match (self.cost, self.cost_before) {
            (Some(now), Some(before)) => Some((now - before).max(0.0)),
            _ => None,
        };
        self.cost_before = self.cost;
        self.direct = None;
        self.baseline = self.total;
        (tokens, cost)
    }
}

fn each(
    a: &TokenCounts,
    b: &TokenCounts,
    f: impl Fn(Option<u64>, Option<u64>) -> Option<u64>,
) -> TokenCounts {
    TokenCounts {
        input_tokens: f(a.input_tokens, b.input_tokens),
        cached_input_tokens: f(a.cached_input_tokens, b.cached_input_tokens),
        cache_write_tokens: f(a.cache_write_tokens, b.cache_write_tokens),
        output_tokens: f(a.output_tokens, b.output_tokens),
        reasoning_output_tokens: f(a.reasoning_output_tokens, b.reasoning_output_tokens),
        total_tokens: f(a.total_tokens, b.total_tokens),
    }
}

/// `a - b` where both reported a counter; a counter either lacked stays unknown.
fn minus(a: &TokenCounts, b: &TokenCounts) -> TokenCounts {
    each(a, b, |a, b| Some(a?.saturating_sub(b?)))
}

/// `a + b`, counting a counter only one of them reported as it is.
fn plus(a: &TokenCounts, b: &TokenCounts) -> TokenCounts {
    each(a, b, |a, b| match (a, b) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (one, other) => one.or(other),
    })
}

/// `a`, with what it did not report from `b`.
fn merge(a: &TokenCounts, b: &TokenCounts) -> TokenCounts {
    each(a, b, |a, b| a.or(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(input: u64, output: u64) -> TokenCounts {
        TokenCounts {
            input_tokens: Some(input),
            output_tokens: Some(output),
            total_tokens: Some(input + output),
            ..TokenCounts::default()
        }
    }

    #[test]
    fn a_cumulative_total_gives_each_turn_its_own_growth() {
        // Codex and the gateway: `total` runs on, `last` is the request just made.
        let mut turn = TurnUsage::new(true);
        turn.turn_started();
        turn.observe(Some(counts(100, 10)), Some(counts(100, 10)));
        let (tokens, _) = turn.turn_ended();
        assert_eq!(tokens.total_tokens, Some(110));
        turn.turn_started();
        // Two requests in one turn.
        turn.observe(Some(counts(250, 30)), Some(counts(150, 20)));
        turn.observe(Some(counts(400, 50)), Some(counts(150, 20)));
        let (tokens, _) = turn.turn_ended();
        assert_eq!(tokens.input_tokens, Some(300));
        assert_eq!(tokens.output_tokens, Some(40));
        assert_eq!(tokens.total_tokens, Some(340));
    }

    #[test]
    fn the_first_turn_after_a_resume_starts_from_total_minus_last() {
        // The thread already held 5 000 tokens when Hutan resumed it.
        let mut turn = TurnUsage::new(false);
        turn.turn_started();
        turn.observe(Some(counts(5_100, 60)), Some(counts(100, 10)));
        let (tokens, _) = turn.turn_ended();
        assert_eq!(tokens.input_tokens, Some(100));
        assert_eq!(tokens.total_tokens, Some(110));
    }

    #[test]
    fn an_acp_turn_counts_its_own_answer_and_not_the_context_size() {
        // `usage_update` gives the context (`total` only); the prompt's answer gives the turn.
        let mut turn = TurnUsage::new(true);
        turn.turn_started();
        turn.observe(
            Some(TokenCounts {
                total_tokens: Some(1_234),
                ..TokenCounts::default()
            }),
            None,
        );
        turn.observe(None, Some(counts(1_200, 34)));
        let (tokens, _) = turn.turn_ended();
        assert_eq!(tokens.input_tokens, Some(1_200));
        assert_eq!(tokens.total_tokens, Some(1_234));
        // The next turn does not inherit the context size as spend.
        turn.turn_started();
        turn.observe(None, Some(counts(10, 1)));
        assert_eq!(turn.turn_ended().0.total_tokens, Some(11));
    }

    #[test]
    fn a_turn_that_reported_nothing_has_nothing() {
        let mut turn = TurnUsage::new(true);
        turn.turn_started();
        assert_eq!(turn.turn_ended(), (TokenCounts::default(), None));
    }

    #[test]
    fn cache_writes_are_summed_like_the_other_counters() {
        let mut turn = TurnUsage::new(true);
        turn.turn_started();
        let with_write = |write: u64| TokenCounts {
            cache_write_tokens: Some(write),
            ..counts(1, 1)
        };
        turn.observe(None, Some(with_write(300)));
        turn.observe(None, Some(with_write(50)));
        assert_eq!(turn.turn_ended().0.cache_write_tokens, Some(350));
    }

    #[test]
    fn cost_is_the_growth_of_the_session_cost() {
        let mut turn = TurnUsage::new(true);
        turn.turn_started();
        turn.cost_reported(0.25);
        assert_eq!(turn.turn_ended().1, Some(0.25));
        turn.turn_started();
        turn.cost_reported(0.75);
        let (_, cost) = turn.turn_ended();
        assert!((cost.unwrap_or(f64::NAN) - 0.5).abs() < 1e-9);
        // A resumed session's first cost has no known start: nothing is claimed for that turn.
        let mut resumed = TurnUsage::new(false);
        resumed.turn_started();
        resumed.cost_reported(9.0);
        assert_eq!(resumed.turn_ended().1, None);
        resumed.turn_started();
        resumed.cost_reported(9.5);
        assert!((resumed.turn_ended().1.unwrap_or(f64::NAN) - 0.5).abs() < 1e-9);
    }
}
