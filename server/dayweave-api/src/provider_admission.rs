//! Local and durable ownership of complete Google operations during deletion.
//!
//! Closing is deliberately sticky, including when its caller is cancelled or a
//! fence commit response is lost. PostgreSQL-backed controllers also register
//! ownership before provider work, and never reap interrupted work on a timer.
//! This is not an external restore permit. Account deletion remains unavailable
//! until interruption recovery and the remaining external boundaries are ready.

use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use sqlx::PgPool;
use thiserror::Error;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{google_oauth::OAuthScope, persistence::PostgresProviderAdmissionRepository};

tokio::task_local! {
    static CURRENT_OPERATION: ProviderOperation;
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProviderAdmissionError {
    #[error("provider operation admission is closed")]
    Closed,
    #[error("provider operation scope is invalid")]
    InvalidScope,
    #[error("provider operation ownership does not match")]
    WrongOperation,
    #[error("provider operation drain belongs to another deletion")]
    ConflictingDeletion,
    #[error("an active provider operation cannot drain itself")]
    ReentrantDrain,
    #[error("durable provider admission is unavailable")]
    Unavailable,
}

#[derive(Default)]
struct State {
    deletion_id: Option<Uuid>,
    active: usize,
}

struct Inner {
    scope: OAuthScope,
    runtime_id: Uuid,
    durable: Option<PostgresProviderAdmissionRepository>,
    state: Mutex<State>,
    drained: Notify,
}

/// One controller must be shared by OAuth, sync, and the deletion repository.
/// Constructing a second controller for the same scope does not share proof.
#[derive(Clone)]
pub struct ProviderAdmission {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ProviderAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.state.lock().expect("provider admission state");
        formatter
            .debug_struct("ProviderAdmission")
            .field("closed", &state.deletion_id.is_some())
            .field("active_operations", &state.active)
            .finish_non_exhaustive()
    }
}

impl ProviderAdmission {
    #[must_use]
    pub fn new(scope: OAuthScope) -> Self {
        Self {
            inner: Arc::new(Inner {
                scope,
                runtime_id: Uuid::new_v4(),
                durable: None,
                state: Mutex::new(State::default()),
                drained: Notify::new(),
            }),
        }
    }

    /// Creates an independently registered runtime for a `PostgreSQL` scope.
    /// No provider work is polled until its operation record commits. There is
    /// no local-only fallback if the database is unavailable.
    #[must_use]
    pub fn postgres(pool: PgPool, scope: OAuthScope) -> Self {
        Self {
            inner: Arc::new(Inner {
                scope,
                runtime_id: Uuid::new_v4(),
                durable: Some(PostgresProviderAdmissionRepository::new(pool, scope)),
                state: Mutex::new(State::default()),
                drained: Notify::new(),
            }),
        }
    }

    pub(crate) fn is_durable(&self) -> bool {
        self.inner.durable.is_some()
    }

    #[must_use]
    pub fn scope(&self) -> OAuthScope {
        self.inner.scope
    }

    /// # Panics
    /// Panics if an earlier panic poisoned the admission state lock.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner
            .state
            .lock()
            .expect("provider admission state")
            .deletion_id
            .is_some()
    }

    /// # Panics
    /// Panics if an earlier panic poisoned the admission state lock.
    #[must_use]
    pub fn active_operations(&self) -> usize {
        self.inner
            .state
            .lock()
            .expect("provider admission state")
            .active
    }

    /// Runs a complete operation, through final durable result handling.
    /// Same-controller nested work inherits its existing ownership, allowing
    /// admitted sync requests to refresh credentials while deletion drains.
    ///
    /// # Errors
    /// Rejects new work when admission is closed, its scope is invalid, or the
    /// operation counter cannot represent another owner, or durable registration
    /// fails. An ambiguous registration is retained, not silently retired.
    /// The future is not
    /// polled on rejection; its own result remains nested in the success value.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the admission state lock.
    pub async fn run<F: Future>(&self, future: F) -> Result<F::Output, ProviderAdmissionError> {
        let operation = match self.current_operation() {
            Ok(operation) => operation,
            // Keep the database registration state machine off every nested
            // OAuth/sync caller's stack without boxing the provider body.
            Err(_) => Box::pin(self.enter()).await?,
        };
        Ok(operation.run(future).await)
    }

    pub(crate) fn current_operation(&self) -> Result<ProviderOperation, ProviderAdmissionError> {
        CURRENT_OPERATION
            .try_with(|operation| {
                if Arc::ptr_eq(&operation.owner.inner, &self.inner) {
                    Ok(operation.clone())
                } else {
                    Err(ProviderAdmissionError::WrongOperation)
                }
            })
            .unwrap_or(Err(ProviderAdmissionError::WrongOperation))
    }

    async fn enter(&self) -> Result<ProviderOperation, ProviderAdmissionError> {
        let operation = self.enter_local()?;
        if let Some(durable) = self.inner.durable.as_ref() {
            durable
                .register(self.inner.runtime_id, operation.owner.operation_id)
                .await?;
            operation.owner.registered.store(true, Ordering::Release);
        }
        Ok(operation)
    }

    fn enter_local(&self) -> Result<ProviderOperation, ProviderAdmissionError> {
        if self.inner.scope.workspace_id.is_nil() || self.inner.scope.user_id.is_nil() {
            return Err(ProviderAdmissionError::InvalidScope);
        }
        let mut state = self.inner.state.lock().expect("provider admission state");
        if state.deletion_id.is_some() {
            return Err(ProviderAdmissionError::Closed);
        }
        state.active = state
            .active
            .checked_add(1)
            .ok_or(ProviderAdmissionError::Closed)?;
        Ok(ProviderOperation {
            owner: Arc::new(OperationOwner {
                inner: self.inner.clone(),
                operation_id: Uuid::new_v4(),
                registered: AtomicBool::new(false),
                interrupted: AtomicBool::new(false),
            }),
        })
    }

    /// Closes new admission before waiting. Durable closure commits before
    /// draining, and no database transaction or lock is held while waiting.
    /// Cancellation leaves admission closed. Retrying the same deletion is
    /// allowed; another deletion cannot reuse or replace this closure.
    ///
    /// There is intentionally no automatic reopen or drop-based reopen. A
    /// future reopening protocol must prove authoritatively that no fence
    /// committed, including after an ambiguous result or process restart.
    ///
    /// # Errors
    /// Rejects nil scope/deletion IDs, a closure belonging to another deletion,
    /// a drain attempted from within any admitted provider operation, or a
    /// database failure. Interrupted/crashed operation records never expire;
    /// waiting may require cancellation and authoritative recovery.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the admission state lock.
    pub async fn close_and_drain(
        &self,
        deletion_id: Uuid,
    ) -> Result<DrainedProviderAdmission, ProviderAdmissionError> {
        if deletion_id.is_nil()
            || self.inner.scope.workspace_id.is_nil()
            || self.inner.scope.user_id.is_nil()
        {
            return Err(ProviderAdmissionError::InvalidScope);
        }
        // Any provider context may mask an outer owner of this controller.
        // For example A -> B -> drain(A) must not wait for A's own stack frame.
        // Deletion drain belongs outside provider operations altogether.
        if CURRENT_OPERATION.try_with(|_| ()).is_ok() {
            return Err(ProviderAdmissionError::ReentrantDrain);
        }
        {
            let mut state = self.inner.state.lock().expect("provider admission state");
            match state.deletion_id {
                Some(existing) if existing != deletion_id => {
                    return Err(ProviderAdmissionError::ConflictingDeletion);
                }
                Some(_) => {}
                None => state.deletion_id = Some(deletion_id),
            }
        }
        if let Some(durable) = self.inner.durable.as_ref() {
            Box::pin(durable.close(deletion_id)).await?;
        }
        loop {
            let notified = self.inner.drained.notified();
            tokio::pin!(notified);
            // Register before the state read: the last owner may finish
            // between that read and this future's first await.
            notified.as_mut().enable();
            if self
                .inner
                .state
                .lock()
                .expect("provider admission state")
                .active
                == 0
            {
                match self.inner.durable.as_ref() {
                    Some(durable) if !Box::pin(durable.is_drained(deletion_id)).await? => {}
                    _ => {
                        return Ok(DrainedProviderAdmission {
                            inner: self.inner.clone(),
                            deletion_id,
                        });
                    }
                }
            }
            if self.inner.durable.is_some() {
                // Other runtimes cannot notify this process. Polling observes
                // durable settlement, never authorizes timeout-based reaping.
                tokio::select! {
                    () = &mut notified => {}
                    () = tokio::time::sleep(Duration::from_millis(250)) => {}
                }
            } else {
                notified.await;
            }
        }
    }
}

struct OperationOwner {
    inner: Arc<Inner>,
    operation_id: Uuid,
    registered: AtomicBool,
    interrupted: AtomicBool,
}

impl Drop for OperationOwner {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().expect("provider admission state");
        state.active = state.active.checked_sub(1).expect("one operation owner");
        if state.active == 0 {
            self.inner.drained.notify_waiters();
        }
        drop(state);
        // Drop alone is never settlement. Every polled or handed-off context
        // must have returned normally, and registration must be confirmed.
        if self.registered.load(Ordering::Acquire)
            && !self.interrupted.load(Ordering::Acquire)
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let owner = Arc::downgrade(&self.inner);
            let operation_id = self.operation_id;
            let durable = self
                .inner
                .durable
                .clone()
                .expect("registered durable operation");
            let runtime_id = self.inner.runtime_id;
            runtime.spawn(async move {
                let mut delay = Duration::from_millis(100);
                loop {
                    match durable.settle(runtime_id, operation_id).await {
                        Ok(()) => {
                            if let Some(inner) = owner.upgrade() {
                                inner.drained.notify_waiters();
                            }
                            return;
                        }
                        Err(ProviderAdmissionError::Unavailable) => {}
                        Err(_) => return,
                    }
                    if owner.upgrade().is_none() {
                        return;
                    }
                    // Retry a known normal completion only while its runtime
                    // exists. Loss of this memory leaves its durable row intact.
                    tokio::time::sleep(delay).await;
                    delay = delay.saturating_mul(2).min(Duration::from_secs(5));
                }
            });
        }
    }
}

struct OperationCompletion {
    owner: Arc<OperationOwner>,
    completed: bool,
}

impl OperationCompletion {
    fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for OperationCompletion {
    fn drop(&mut self) {
        if !self.completed {
            self.owner.interrupted.store(true, Ordering::Release);
        }
    }
}

/// A child cleanup task inherits this owner before the parent can return or
/// be cancelled. The last clone, not the parent request, determines draining.
#[derive(Clone)]
pub(crate) struct ProviderOperation {
    owner: Arc<OperationOwner>,
}

impl ProviderOperation {
    pub(crate) fn run<F: Future>(self, future: F) -> impl Future<Output = F::Output> {
        // Construct this guard synchronously, before a detached future can be
        // dropped without even its first poll (it may already own a token).
        let completion = OperationCompletion {
            owner: self.owner.clone(),
            completed: false,
        };
        async move {
            let output = CURRENT_OPERATION.scope(self, future).await;
            // Consume the whole guard, not only its flag: disjoint async
            // capture must not drop the owning Arc before the future runs.
            completion.complete();
            output
        }
    }

    pub(crate) fn spawn<F>(self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tokio::spawn(self.run(future))
    }
}

/// Opaque proof of a particular controller's sticky, fully drained closure.
/// With a durable controller this also observes its persisted closure and no
/// unresolved registrations. Fencing must independently recheck that database
/// state under its exclusive mutation barrier. It is not a restore permit.
pub struct DrainedProviderAdmission {
    inner: Arc<Inner>,
    deletion_id: Uuid,
}

impl std::fmt::Debug for DrainedProviderAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DrainedProviderAdmission")
            .finish_non_exhaustive()
    }
}

impl DrainedProviderAdmission {
    pub(crate) fn matches(&self, admission: &ProviderAdmission, deletion_id: Uuid) -> bool {
        if !Arc::ptr_eq(&self.inner, &admission.inner) || self.deletion_id != deletion_id {
            return false;
        }
        let state = self.inner.state.lock().expect("provider admission state");
        state.deletion_id == Some(deletion_id) && state.active == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::oneshot;

    fn admission() -> ProviderAdmission {
        ProviderAdmission::new(OAuthScope {
            workspace_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
        })
    }

    #[tokio::test]
    async fn normal_completion_keeps_ownership_until_the_future_finishes() {
        let gate = admission();
        let operation = gate.enter_local().unwrap();
        let owner = operation.owner.clone();
        let future = operation.run(async { 7 });
        assert!(!owner.interrupted.load(Ordering::Acquire));
        assert_eq!(future.await, 7);
        assert!(!owner.interrupted.load(Ordering::Acquire));
        assert_eq!(gate.active_operations(), 1);
        drop(owner);
        assert_eq!(gate.active_operations(), 0);
    }

    #[tokio::test]
    async fn dropping_an_unpolled_child_marks_its_entire_operation_interrupted() {
        let gate = admission();
        let operation = gate.enter_local().unwrap();
        let owner = operation.owner.clone();
        let future = operation.run(async {});
        assert!(!owner.interrupted.load(Ordering::Acquire));
        drop(future);
        assert!(owner.interrupted.load(Ordering::Acquire));
        drop(owner);
        assert_eq!(gate.active_operations(), 0);

        let operation = gate.enter_local().unwrap();
        let owner = operation.owner.clone();
        // The current-thread test runtime cannot poll the new task until this
        // test yields, so abort-before-first-poll is deterministic.
        let child = operation.spawn(std::future::pending::<()>());
        child.abort();
        assert!(child.await.unwrap_err().is_cancelled());
        assert!(owner.interrupted.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn closure_is_sticky_exact_and_controller_bound() {
        let gate = admission();
        let deletion_id = Uuid::new_v4();
        let permit = gate.close_and_drain(deletion_id).await.unwrap();
        assert!(permit.matches(&gate.clone(), deletion_id));
        assert!(!permit.matches(&ProviderAdmission::new(gate.scope()), deletion_id));
        assert!(!permit.matches(&gate, Uuid::new_v4()));
        assert!(matches!(
            gate.close_and_drain(Uuid::new_v4()).await,
            Err(ProviderAdmissionError::ConflictingDeletion)
        ));
        drop(permit);
        let polled = AtomicBool::new(false);
        assert_eq!(
            gate.run(async {
                polled.store(true, Ordering::SeqCst);
            })
            .await,
            Err(ProviderAdmissionError::Closed)
        );
        assert!(!polled.load(Ordering::SeqCst));
        assert!(
            gate.close_and_drain(deletion_id)
                .await
                .unwrap()
                .matches(&gate, deletion_id)
        );
    }

    #[tokio::test]
    #[allow(clippy::async_yields_async)] // The parent returns its detached cleanup handle without awaiting it.
    async fn inherited_cleanup_outlives_parent_and_can_finish_nested_work_after_close() {
        let gate = admission();
        let (release, released) = oneshot::channel();
        let child_gate = gate.clone();
        let child = gate
            .run(async {
                let operation = gate.current_operation().unwrap();
                tokio::spawn(operation.run(async move {
                    released.await.unwrap();
                    child_gate.run(async { 42 }).await.unwrap()
                }))
            })
            .await
            .unwrap();
        assert_eq!(gate.active_operations(), 1);
        let deletion_id = Uuid::new_v4();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), gate.close_and_drain(deletion_id))
                .await
                .is_err()
        );
        assert!(gate.is_closed());
        assert_eq!(
            gate.run(async {}).await,
            Err(ProviderAdmissionError::Closed)
        );
        release.send(()).unwrap();
        assert_eq!(child.await.unwrap(), 42);
        assert!(
            gate.close_and_drain(deletion_id)
                .await
                .unwrap()
                .matches(&gate, deletion_id)
        );
        assert_eq!(gate.active_operations(), 0);
    }

    #[tokio::test]
    async fn cancelling_operation_releases_ownership_but_cancelling_drain_does_not_reopen() {
        let gate = admission();
        let (started, ready) = oneshot::channel();
        let running_gate = gate.clone();
        let operation = tokio::spawn(async move {
            running_gate
                .run(async {
                    started.send(()).unwrap();
                    std::future::pending::<()>().await;
                })
                .await
        });
        ready.await.unwrap();
        let deletion_id = Uuid::new_v4();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), gate.close_and_drain(deletion_id))
                .await
                .is_err()
        );
        operation.abort();
        assert!(operation.await.unwrap_err().is_cancelled());
        assert_eq!(gate.active_operations(), 0);
        let _permit = gate.close_and_drain(deletion_id).await.unwrap();
        assert_eq!(
            gate.run(async {}).await,
            Err(ProviderAdmissionError::Closed)
        );
    }

    #[tokio::test]
    async fn nested_work_does_not_multiply_owners_or_authorize_another_controller() {
        let gate = admission();
        let other = ProviderAdmission::new(gate.scope());
        let _closed = other.close_and_drain(Uuid::new_v4()).await.unwrap();
        gate.run(async {
            assert_eq!(gate.active_operations(), 1);
            gate.run(async {
                assert_eq!(gate.active_operations(), 1);
            })
            .await
            .unwrap();
            assert_eq!(
                other.run(async {}).await,
                Err(ProviderAdmissionError::Closed)
            );
            assert!(matches!(
                gate.close_and_drain(Uuid::new_v4()).await,
                Err(ProviderAdmissionError::ReentrantDrain)
            ));
            assert!(!gate.is_closed());
        })
        .await
        .unwrap();
        assert_eq!(gate.active_operations(), 0);
    }

    #[tokio::test]
    async fn invalid_scope_or_deletion_never_closes_or_polls_work() {
        let gate = admission();
        assert!(matches!(
            gate.close_and_drain(Uuid::nil()).await,
            Err(ProviderAdmissionError::InvalidScope)
        ));
        assert!(!gate.is_closed());
        let invalid = ProviderAdmission::new(OAuthScope {
            workspace_id: Uuid::nil(),
            user_id: Uuid::new_v4(),
        });
        assert_eq!(
            invalid.run(async {}).await,
            Err(ProviderAdmissionError::InvalidScope)
        );
    }

    #[tokio::test]
    async fn another_nested_controller_cannot_hide_a_reentrant_drain() {
        let outer = admission();
        let inner = admission();
        outer
            .run(async {
                inner
                    .run(async {
                        assert!(matches!(
                            outer.close_and_drain(Uuid::new_v4()).await,
                            Err(ProviderAdmissionError::ReentrantDrain)
                        ));
                        assert!(!outer.is_closed());
                        assert!(!inner.is_closed());
                    })
                    .await
                    .unwrap();
            })
            .await
            .unwrap();
        assert_eq!(outer.active_operations(), 0);
        assert_eq!(inner.active_operations(), 0);
    }

    #[tokio::test]
    async fn all_concurrent_drain_waiters_observe_the_last_owner() {
        let gate = admission();
        let (release, released) = oneshot::channel();
        let (started, ready) = oneshot::channel();
        let running_gate = gate.clone();
        let operation = tokio::spawn(async move {
            running_gate
                .run(async {
                    started.send(()).unwrap();
                    released.await.unwrap();
                })
                .await
                .unwrap();
        });
        ready.await.unwrap();
        let deletion_id = Uuid::new_v4();
        let mut waiters = Vec::new();
        for _ in 0..8 {
            let waiting_gate = gate.clone();
            waiters.push(tokio::spawn(async move {
                waiting_gate.close_and_drain(deletion_id).await.unwrap()
            }));
        }
        release.send(()).unwrap();
        operation.await.unwrap();
        for waiter in waiters {
            assert!(
                tokio::time::timeout(Duration::from_secs(1), waiter)
                    .await
                    .unwrap()
                    .unwrap()
                    .matches(&gate, deletion_id)
            );
        }
    }
}
