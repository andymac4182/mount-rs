//! Pure decisions shared by the client transports and their verification.

use crate::connection::ClientError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuicFailure {
    Deadline,
    TimedOut,
    Refused,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuicFailureDecision {
    Unavailable,
    Transport,
    Authentication,
}

pub(crate) fn classify_quic_failure(received: bool, failure: QuicFailure) -> QuicFailureDecision {
    match (received, failure) {
        (false, QuicFailure::Deadline | QuicFailure::TimedOut | QuicFailure::Refused) => {
            QuicFailureDecision::Unavailable
        }
        (true, QuicFailure::Deadline) => QuicFailureDecision::Transport,
        _ => QuicFailureDecision::Authentication,
    }
}

/// Created only after acquiring a QUIC stream or the WebSocket socket lock.
#[derive(Default)]
pub(crate) struct TransactionCompletion {
    complete: bool,
}

impl TransactionCompletion {
    /// Call only after consuming and validating the complete response envelope.
    pub(crate) fn complete_response(&mut self) {
        self.complete = true;
    }

    pub(crate) fn destroy_on_drop(&self) -> bool {
        !self.complete
    }
}

/// `None` represents a request deadline. A valid filesystem error completes the
/// exchange; local errors or invalid counts leave completion uncertain.
pub(crate) fn finish_io_completion(
    result: Option<Result<usize, ClientError>>,
    limit: usize,
) -> (Result<usize, ClientError>, bool) {
    match result {
        Some(Ok(count)) if count <= limit => (Ok(count), false),
        Some(Ok(_)) => (Err(ClientError::Protocol), true),
        Some(Err(error @ ClientError::Remote(_))) => (Err(error), false),
        Some(Err(error)) => (Err(error), true),
        None => (Err(ClientError::Transport), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_quic_failure_requires_no_contact_and_an_unavailable_outcome() {
        let failures = [
            QuicFailure::Deadline,
            QuicFailure::TimedOut,
            QuicFailure::Refused,
            QuicFailure::Other,
        ];
        for failure in failures {
            let uncontacted = classify_quic_failure(false, failure);
            assert_eq!(
                uncontacted == QuicFailureDecision::Unavailable,
                failure != QuicFailure::Other,
            );
            assert_eq!(
                classify_quic_failure(true, failure),
                if failure == QuicFailure::Deadline {
                    QuicFailureDecision::Transport
                } else {
                    QuicFailureDecision::Authentication
                },
            );
        }
    }

    #[test]
    fn acquired_transaction_requires_a_complete_response_before_reuse() {
        let mut state = TransactionCompletion::default();
        assert!(state.destroy_on_drop());
        state.complete_response();
        assert!(!state.destroy_on_drop());
    }

    #[test]
    fn io_completion_closes_invalid_counts_and_local_errors() {
        for (count, limit) in [(0, 0), (4, 4), (usize::MAX, usize::MAX)] {
            assert_eq!(
                finish_io_completion(Some(Ok(count)), limit),
                (Ok(count), false)
            );
        }
        for (count, limit) in [(1, 0), (5, 4), (usize::MAX, usize::MAX - 1)] {
            assert_eq!(
                finish_io_completion(Some(Ok(count)), limit),
                (Err(ClientError::Protocol), true),
            );
        }
        for error in [
            ClientError::Transport,
            ClientError::Protocol,
            ClientError::Authentication,
            ClientError::Credential,
        ] {
            assert_eq!(
                finish_io_completion(Some(Err(error.clone())), 0),
                (Err(error), true),
            );
        }
        assert_eq!(
            finish_io_completion(Some(Err(ClientError::Remote("EACCES".into()))), 0),
            (Err(ClientError::Remote("EACCES".into())), false),
        );
        assert_eq!(
            finish_io_completion(None, 0),
            (Err(ClientError::Transport), true)
        );
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn initial_auto_fallback_requires_uncontacted_unavailability() {
        let received: bool = kani::any();
        let kind: u8 = kani::any();
        kani::assume(kind < 4);
        let failure = match kind {
            0 => QuicFailure::Deadline,
            1 => QuicFailure::TimedOut,
            2 => QuicFailure::Refused,
            _ => QuicFailure::Other,
        };
        let decision = classify_quic_failure(received, failure);
        assert_eq!(
            decision == QuicFailureDecision::Unavailable,
            !received && kind < 3
        );
        if received {
            assert_ne!(decision, QuicFailureDecision::Unavailable);
            assert_eq!(
                decision,
                if kind == 0 {
                    QuicFailureDecision::Transport
                } else {
                    QuicFailureDecision::Authentication
                },
            );
        }
        if kind == 3 {
            assert_eq!(decision, QuicFailureDecision::Authentication);
        }
        kani::cover!(!received && kind == 0 && decision == QuicFailureDecision::Unavailable);
        kani::cover!(!received && kind == 1 && decision == QuicFailureDecision::Unavailable);
        kani::cover!(!received && kind == 2 && decision == QuicFailureDecision::Unavailable);
        kani::cover!(received && kind == 0 && decision == QuicFailureDecision::Transport);
        kani::cover!(received && kind == 1 && decision == QuicFailureDecision::Authentication);
        kani::cover!(received && kind == 2 && decision == QuicFailureDecision::Authentication);
        kani::cover!(!received && kind == 3 && decision == QuicFailureDecision::Authentication);
    }

    #[kani::proof]
    fn acquired_transaction_cannot_reuse_an_incomplete_response() {
        let completed: bool = kani::any();
        let mut state = TransactionCompletion::default();
        assert!(state.destroy_on_drop());
        if completed {
            state.complete_response();
        }
        assert_eq!(state.destroy_on_drop(), !completed);
        kani::cover!(completed && !state.destroy_on_drop());
        kani::cover!(!completed && state.destroy_on_drop());
    }

    #[kani::proof]
    fn remote_io_completion_bounds_counts_and_closes_uncertainty() {
        let count: usize = kani::any();
        let limit: usize = kani::any();
        let (result, close) = finish_io_completion(Some(Ok(count)), limit);
        assert_eq!(close, count > limit);
        if count <= limit {
            assert_eq!(result, Ok(count));
        } else {
            assert_eq!(result, Err(ClientError::Protocol));
        }
        kani::cover!(count == 0 && limit == 0 && !close);
        kani::cover!(count == usize::MAX && limit == usize::MAX && !close);
        kani::cover!(count > limit && close);

        let kind: u8 = kani::any();
        kani::assume(kind < 6);
        let input = match kind {
            0 => Some(Err(ClientError::Remote(String::new()))),
            1 => Some(Err(ClientError::Transport)),
            2 => Some(Err(ClientError::Protocol)),
            3 => Some(Err(ClientError::Authentication)),
            4 => Some(Err(ClientError::Credential)),
            _ => None,
        };
        let (result, close) = finish_io_completion(input, limit);
        assert_eq!(close, kind != 0);
        let expected = match kind {
            0 => ClientError::Remote(String::new()),
            1 | 5 => ClientError::Transport,
            2 => ClientError::Protocol,
            3 => ClientError::Authentication,
            _ => ClientError::Credential,
        };
        assert_eq!(result, Err(expected));
        kani::cover!(kind == 0 && !close);
        kani::cover!(kind == 5 && close && result == Err(ClientError::Transport));
    }
}
