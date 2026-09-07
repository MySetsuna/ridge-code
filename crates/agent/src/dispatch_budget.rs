use crate::communication::AgentCancellation;
use langgraph::{DispatchBudgetReason, GraphError};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Shared dispatch ceiling. The existing per-wave limit remains three; this
/// second bound spans waves and independent orchestration runs.
pub const DEFAULT_DISPATCH_CONCURRENCY: usize = 3;
pub const MAX_DISPATCH_CONCURRENCY: usize = 32;
pub const DEFAULT_DISPATCH_ATTEMPTS: usize = 32;
pub const MAX_DISPATCH_ATTEMPTS: usize = 256;
const DISPATCH_CONCURRENCY_ENV: &str = "RIDGECODE_DISPATCH_CONCURRENCY";
const DISPATCH_ATTEMPTS_ENV: &str = "RIDGECODE_DISPATCH_ATTEMPTS";

fn parse_dispatch_concurrency(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|limit| *limit > 0)
        .map(|limit| limit.min(MAX_DISPATCH_CONCURRENCY))
        .unwrap_or(DEFAULT_DISPATCH_CONCURRENCY)
}

fn parse_dispatch_attempts(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|limit| *limit > 0)
        .map(|limit| limit.min(MAX_DISPATCH_ATTEMPTS))
        .unwrap_or(DEFAULT_DISPATCH_ATTEMPTS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchBudgetConfigError;

impl std::fmt::Display for DispatchBudgetConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dispatch budget concurrency and attempt limits must be greater than zero")
    }
}

impl std::error::Error for DispatchBudgetConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchBudgetRejection {
    Cancelled,
    Closed,
    AttemptsExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchBudgetError {
    pub operation: &'static str,
    pub limit: usize,
    pub attempt_limit: usize,
    pub attempts: usize,
    pub reason: DispatchBudgetRejection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchBudgetStats {
    pub limit: usize,
    pub active: usize,
    pub peak: usize,
    pub attempt_limit: usize,
    pub attempts: usize,
}

/// A clonable, semaphore-backed dispatch budget shared by waves and runs.
///
/// Permit acquisition is cancellation-aware. A permit is held only for the
/// provider/A2A operation and is released by RAII on success, cancellation,
/// or error, so a failed fallback cannot leak capacity.
pub struct DispatchBudget {
    shared: Arc<DispatchBudgetShared>,
    attempt_limit: usize,
    attempts: Arc<AtomicUsize>,
}

struct DispatchBudgetShared {
    semaphore: Arc<Semaphore>,
    limit: usize,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl DispatchBudget {
    pub fn new(limit: usize) -> Result<Self, DispatchBudgetConfigError> {
        Self::new_with_attempt_limit(limit, DEFAULT_DISPATCH_ATTEMPTS)
    }

    pub fn new_with_attempt_limit(
        limit: usize,
        attempt_limit: usize,
    ) -> Result<Self, DispatchBudgetConfigError> {
        if limit == 0 || attempt_limit == 0 {
            return Err(DispatchBudgetConfigError);
        }
        Ok(Self {
            shared: Arc::new(DispatchBudgetShared {
                semaphore: Arc::new(Semaphore::new(limit)),
                limit,
                active: Arc::new(AtomicUsize::new(0)),
                peak: Arc::new(AtomicUsize::new(0)),
            }),
            attempt_limit: attempt_limit.min(MAX_DISPATCH_ATTEMPTS),
            attempts: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Start a fresh run/session scope while retaining the process-wide
    /// concurrency ceiling. Attempt/retry/fallback accounting is intentionally
    /// scoped here, not kept in the process-wide default forever.
    pub fn scope(&self) -> Self {
        self.scope_with_attempt_limit(self.attempt_limit)
    }

    /// Start a fresh scope with an explicit remaining attempt ceiling while
    /// retaining the shared process-wide concurrency ceiling. A zero ceiling
    /// is useful to represent an already exhausted run; acquisition then
    /// returns a structured `AttemptsExhausted` rejection without waiting for
    /// or entering the provider.
    pub fn scope_with_attempt_limit(&self, attempt_limit: usize) -> Self {
        Self {
            shared: self.shared.clone(),
            attempt_limit: attempt_limit.min(MAX_DISPATCH_ATTEMPTS),
            attempts: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn attempt_limit(&self) -> usize {
        self.attempt_limit
    }

    fn error(
        &self,
        operation: &'static str,
        reason: DispatchBudgetRejection,
    ) -> DispatchBudgetError {
        DispatchBudgetError {
            operation,
            limit: self.shared.limit,
            attempt_limit: self.attempt_limit,
            attempts: self.attempts.load(Ordering::SeqCst),
            reason,
        }
    }

    fn consume_attempt(&self, operation: &'static str) -> Result<(), DispatchBudgetError> {
        let mut attempts = self.attempts.load(Ordering::SeqCst);
        loop {
            if attempts >= self.attempt_limit {
                return Err(self.error(operation, DispatchBudgetRejection::AttemptsExhausted));
            }
            match self.attempts.compare_exchange_weak(
                attempts,
                attempts + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(()),
                Err(current) => attempts = current,
            }
        }
    }

    fn finish_permit(&self, permit: OwnedSemaphorePermit) -> DispatchPermit {
        let active = self.shared.active.fetch_add(1, Ordering::SeqCst) + 1;
        let mut observed = self.shared.peak.load(Ordering::SeqCst);
        while observed < active {
            match self.shared.peak.compare_exchange_weak(
                observed,
                active,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(current) => observed = current,
            }
        }
        DispatchPermit {
            _permit: permit,
            active: self.shared.active.clone(),
        }
    }

    pub fn stats(&self) -> DispatchBudgetStats {
        DispatchBudgetStats {
            limit: self.shared.limit,
            active: self.shared.active.load(Ordering::SeqCst),
            peak: self.shared.peak.load(Ordering::SeqCst),
            attempt_limit: self.attempt_limit,
            attempts: self.attempts.load(Ordering::SeqCst),
        }
    }

    /// Close the budget during runtime shutdown; current and future waiters
    /// receive a structured `Closed` rejection and never enter a dispatch.
    pub fn close(&self) {
        self.shared.semaphore.close();
    }

    pub async fn acquire(
        &self,
        cancellation: Option<&AgentCancellation>,
        operation: &'static str,
    ) -> Result<DispatchPermit, DispatchBudgetError> {
        if cancellation.is_some_and(AgentCancellation::is_cancelled) {
            return Err(self.error(operation, DispatchBudgetRejection::Cancelled));
        }
        if self.attempts.load(Ordering::SeqCst) >= self.attempt_limit {
            return Err(self.error(operation, DispatchBudgetRejection::AttemptsExhausted));
        }

        let permit = match cancellation {
            Some(cancellation) => {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        return Err(self.error(operation, DispatchBudgetRejection::Cancelled));
                    }
                    result = self.shared.semaphore.clone().acquire_owned() => {
                        result.map_err(|_| self.error(operation, DispatchBudgetRejection::Closed))?
                    }
                }
            }
            None => self
                .shared
                .semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| self.error(operation, DispatchBudgetRejection::Closed))?,
        };

        // Cancellation may race with semaphore wake-up. Never hand a permit
        // to an operation after cancellation has become observable.
        if cancellation.is_some_and(AgentCancellation::is_cancelled) {
            drop(permit);
            return Err(self.error(operation, DispatchBudgetRejection::Cancelled));
        }

        if let Err(error) = self.consume_attempt(operation) {
            drop(permit);
            return Err(error);
        }
        Ok(self.finish_permit(permit))
    }
}

#[derive(Debug)]
pub struct DispatchPermit {
    _permit: OwnedSemaphorePermit,
    active: Arc<AtomicUsize>,
}

impl Drop for DispatchPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

static DEFAULT_DISPATCH_BUDGET: OnceLock<Arc<DispatchBudget>> = OnceLock::new();

/// Process-wide default shared budget. Invalid or zero environment values
/// fall back to the bounded safe default rather than disabling the guard.
pub fn default_dispatch_budget() -> Arc<DispatchBudget> {
    DEFAULT_DISPATCH_BUDGET
        .get_or_init(|| {
            let limit =
                parse_dispatch_concurrency(std::env::var(DISPATCH_CONCURRENCY_ENV).ok().as_deref());
            let attempts =
                parse_dispatch_attempts(std::env::var(DISPATCH_ATTEMPTS_ENV).ok().as_deref());
            Arc::new(
                DispatchBudget::new_with_attempt_limit(limit, attempts)
                    .expect("validated dispatch budget limits"),
            )
        })
        .clone()
}

pub(crate) fn dispatch_budget_error(error: DispatchBudgetError) -> GraphError {
    GraphError::DispatchBudget {
        operation: error.operation.to_string(),
        limit: error.limit,
        reason: match error.reason {
            DispatchBudgetRejection::Cancelled => DispatchBudgetReason::Cancelled,
            DispatchBudgetRejection::Closed => DispatchBudgetReason::Closed,
            DispatchBudgetRejection::AttemptsExhausted => DispatchBudgetReason::AttemptsExhausted,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_dispatch_attempts, parse_dispatch_concurrency, DispatchBudget,
        DispatchBudgetRejection, DEFAULT_DISPATCH_ATTEMPTS, DEFAULT_DISPATCH_CONCURRENCY,
        MAX_DISPATCH_ATTEMPTS, MAX_DISPATCH_CONCURRENCY,
    };

    #[test]
    fn dispatch_concurrency_parser_is_bounded_without_environment_mutation() {
        assert_eq!(
            parse_dispatch_concurrency(None),
            DEFAULT_DISPATCH_CONCURRENCY
        );
        assert_eq!(
            parse_dispatch_concurrency(Some("bad")),
            DEFAULT_DISPATCH_CONCURRENCY
        );
        assert_eq!(
            parse_dispatch_concurrency(Some("0")),
            DEFAULT_DISPATCH_CONCURRENCY
        );
        assert_eq!(parse_dispatch_concurrency(Some("2")), 2);
        assert_eq!(
            parse_dispatch_concurrency(Some("999999")),
            MAX_DISPATCH_CONCURRENCY
        );
        assert_eq!(parse_dispatch_attempts(None), DEFAULT_DISPATCH_ATTEMPTS);
        assert_eq!(
            parse_dispatch_attempts(Some("0")),
            DEFAULT_DISPATCH_ATTEMPTS
        );
        assert_eq!(parse_dispatch_attempts(Some("2")), 2);
        assert_eq!(
            parse_dispatch_attempts(Some("999999")),
            MAX_DISPATCH_ATTEMPTS
        );
    }

    #[tokio::test]
    async fn attempt_budget_counts_retries_but_scope_resets() {
        let budget = DispatchBudget::new_with_attempt_limit(2, 2).unwrap();
        drop(budget.acquire(None, "first").await.unwrap());
        drop(budget.acquire(None, "fallback").await.unwrap());
        let rejected = budget.acquire(None, "retry").await.unwrap_err();
        assert_eq!(rejected.reason, DispatchBudgetRejection::AttemptsExhausted);
        assert_eq!(rejected.attempts, 2);
        assert_eq!(budget.stats().attempts, 2);
        let scoped = budget.scope();
        let permit = scoped.acquire(None, "new-run").await.unwrap();
        assert_eq!(scoped.stats().attempts, 1);
        drop(permit);
    }
}
