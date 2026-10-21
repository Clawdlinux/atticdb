//! The outcome rule: how one call merges the results of its attempts.
//!
//! Only `committed` replaces `unknown`. A later `not_committed` cannot clear an
//! earlier unclear attempt, because the engine can reject a retry of a key whose
//! first attempt is already durable. An attempt whose connect failed sent nothing: it
//! counts as `not_committed`, but the call still retries until the deadline.

pub use zerodb_tape::Outcome;

/// Parses the wire form used in response bodies.
pub fn parse(text: &str) -> Option<Outcome> {
    [Outcome::Committed, Outcome::NotCommitted, Outcome::Unknown]
        .into_iter()
        .find(|outcome| name(*outcome) == text)
}

/// Returns the wire form used in response bodies.
pub fn name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Committed => "committed",
        Outcome::NotCommitted => "not_committed",
        Outcome::Unknown => "unknown",
    }
}

/// The result of one attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attempt {
    /// The server answered with an outcome.
    Answered(Outcome),
    /// Reset, timeout, or a short or malformed response, after the request was sent.
    TransportError,
    /// Connect failed, so nothing was sent.
    NotSent,
}

/// The merged state of one call across its attempts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Merge {
    state: Option<Outcome>,
}

impl Merge {
    /// Folds in one attempt. Returns the outcome to report now, or `None` to retry
    /// with the same key until the deadline.
    pub fn record(&mut self, attempt: Attempt) -> Option<Outcome> {
        let next = match (self.state, attempt) {
            (_, Attempt::Answered(Outcome::Committed)) => Outcome::Committed,
            (
                Some(Outcome::Unknown),
                Attempt::Answered(Outcome::NotCommitted) | Attempt::NotSent,
            ) => Outcome::Unknown,
            (_, Attempt::Answered(Outcome::NotCommitted) | Attempt::NotSent) => {
                Outcome::NotCommitted
            }
            (_, Attempt::Answered(Outcome::Unknown) | Attempt::TransportError) => Outcome::Unknown,
        };
        self.state = Some(next);
        match (next, attempt) {
            (Outcome::Unknown, _) | (_, Attempt::NotSent) => None,
            (done, _) => Some(done),
        }
    }

    /// The outcome to report when the deadline passes. No attempt at all is `Unknown`.
    pub fn finish(&self) -> Outcome {
        self.state.unwrap_or(Outcome::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: Attempt = Attempt::Answered(Outcome::Committed);
    const N: Attempt = Attempt::Answered(Outcome::NotCommitted);
    const U: Attempt = Attempt::Answered(Outcome::Unknown);
    const T: Attempt = Attempt::TransportError;
    const S: Attempt = Attempt::NotSent;

    struct Case {
        name: &'static str,
        previous: &'static [Attempt],
        this: Attempt,
        step: Option<Outcome>,
        finish: Outcome,
    }

    const CASES: &[Case] = &[
        Case {
            name: "first committed",
            previous: &[],
            this: C,
            step: Some(Outcome::Committed),
            finish: Outcome::Committed,
        },
        Case {
            name: "committed after unknown",
            previous: &[U],
            this: C,
            step: Some(Outcome::Committed),
            finish: Outcome::Committed,
        },
        Case {
            name: "committed after transport",
            previous: &[T, U],
            this: C,
            step: Some(Outcome::Committed),
            finish: Outcome::Committed,
        },
        Case {
            name: "first not_committed",
            previous: &[],
            this: N,
            step: Some(Outcome::NotCommitted),
            finish: Outcome::NotCommitted,
        },
        Case {
            name: "not_committed after transport",
            previous: &[T],
            this: N,
            step: None,
            finish: Outcome::Unknown,
        },
        Case {
            name: "not_committed after unknown",
            previous: &[U],
            this: N,
            step: None,
            finish: Outcome::Unknown,
        },
        Case {
            name: "not_committed twice after unknown",
            previous: &[U, N],
            this: N,
            step: None,
            finish: Outcome::Unknown,
        },
        Case {
            name: "first unknown",
            previous: &[],
            this: U,
            step: None,
            finish: Outcome::Unknown,
        },
        Case {
            name: "first transport",
            previous: &[],
            this: T,
            step: None,
            finish: Outcome::Unknown,
        },
        Case {
            name: "only connect failures",
            previous: &[S],
            this: S,
            step: None,
            finish: Outcome::NotCommitted,
        },
        Case {
            name: "connect failure after unknown",
            previous: &[U],
            this: S,
            step: None,
            finish: Outcome::Unknown,
        },
        Case {
            name: "transport after connect failure",
            previous: &[S],
            this: T,
            step: None,
            finish: Outcome::Unknown,
        },
        Case {
            name: "transport after unknown",
            previous: &[U],
            this: T,
            step: None,
            finish: Outcome::Unknown,
        },
    ];

    #[test]
    fn merge_follows_the_outcome_rule() {
        for case in CASES {
            let mut merge = Merge::default();
            for attempt in case.previous {
                merge.record(*attempt);
            }
            assert_eq!(merge.record(case.this), case.step, "{}: step", case.name);
            assert_eq!(merge.finish(), case.finish, "{}: deadline result", case.name);
        }
        assert_eq!(Merge::default().finish(), Outcome::Unknown, "no attempt before the deadline");
        for outcome in [Outcome::Committed, Outcome::NotCommitted, Outcome::Unknown] {
            assert_eq!(parse(name(outcome)), Some(outcome), "{outcome:?} wire form");
        }
    }
}
