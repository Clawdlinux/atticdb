//! The outcome rule: how one call merges the results of its attempts.
//!
//! Only `committed` replaces `unknown`. A later `not_committed` cannot clear an
//! earlier unclear attempt, because the engine can reject a retry of a key whose
//! first attempt is already durable.

/// What a write call reports to its caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The write is durable.
    Committed,
    /// Nothing was written.
    NotCommitted,
    /// The write may or may not be durable.
    Unknown,
}

impl Outcome {
    /// Parses the wire form used in response bodies.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "committed" => Some(Self::Committed),
            "not_committed" => Some(Self::NotCommitted),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Returns the wire form used in response bodies.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::NotCommitted => "not_committed",
            Self::Unknown => "unknown",
        }
    }
}

/// The result of one attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attempt {
    /// The server answered with an outcome.
    Answered(Outcome),
    /// Connect, reset, timeout, or a short or malformed response.
    TransportError,
}

/// What the caller does after recording an attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    /// Stop and report this outcome.
    Return(Outcome),
    /// Retry with the same key until the deadline.
    Retry,
}

/// The merged state of one call across its attempts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Merge {
    state: Option<Outcome>,
}

impl Merge {
    /// Folds in one attempt and says whether to return or retry.
    pub fn record(&mut self, attempt: Attempt) -> Step {
        let next = match (self.state, attempt) {
            (_, Attempt::Answered(Outcome::Committed)) => Outcome::Committed,
            (Some(Outcome::Unknown), Attempt::Answered(Outcome::NotCommitted)) => Outcome::Unknown,
            (_, Attempt::Answered(Outcome::NotCommitted)) => Outcome::NotCommitted,
            (_, Attempt::Answered(Outcome::Unknown) | Attempt::TransportError) => Outcome::Unknown,
        };
        self.state = Some(next);
        match next {
            Outcome::Unknown => Step::Retry,
            done => Step::Return(done),
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

    struct Case {
        name: &'static str,
        previous: &'static [Attempt],
        this: Attempt,
        step: Step,
        finish: Outcome,
    }

    const CASES: &[Case] = &[
        Case {
            name: "first committed",
            previous: &[],
            this: C,
            step: Step::Return(Outcome::Committed),
            finish: Outcome::Committed,
        },
        Case {
            name: "committed after unknown",
            previous: &[U],
            this: C,
            step: Step::Return(Outcome::Committed),
            finish: Outcome::Committed,
        },
        Case {
            name: "committed after transport",
            previous: &[T, U],
            this: C,
            step: Step::Return(Outcome::Committed),
            finish: Outcome::Committed,
        },
        Case {
            name: "first not_committed",
            previous: &[],
            this: N,
            step: Step::Return(Outcome::NotCommitted),
            finish: Outcome::NotCommitted,
        },
        Case {
            name: "not_committed after transport",
            previous: &[T],
            this: N,
            step: Step::Retry,
            finish: Outcome::Unknown,
        },
        Case {
            name: "not_committed after unknown",
            previous: &[U],
            this: N,
            step: Step::Retry,
            finish: Outcome::Unknown,
        },
        Case {
            name: "not_committed twice after unknown",
            previous: &[U, N],
            this: N,
            step: Step::Retry,
            finish: Outcome::Unknown,
        },
        Case {
            name: "first unknown",
            previous: &[],
            this: U,
            step: Step::Retry,
            finish: Outcome::Unknown,
        },
        Case {
            name: "first transport",
            previous: &[],
            this: T,
            step: Step::Retry,
            finish: Outcome::Unknown,
        },
        Case {
            name: "transport after unknown",
            previous: &[U],
            this: T,
            step: Step::Retry,
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
            assert_eq!(Outcome::parse(outcome.as_str()), Some(outcome), "{outcome:?} wire form");
        }
    }
}
