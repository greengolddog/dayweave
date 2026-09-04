use std::{pin::Pin, sync::Arc, time::Duration};

use futures_util::{Stream, stream};
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError, watch},
    time::{Instant, Interval, MissedTickBehavior, Sleep, interval_at, sleep},
};

use super::{HabitRepository, HabitRepositoryError, service::encode_delta_cursor};

const PROBE_INTERVAL: Duration = Duration::from_secs(5);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const CONNECTION_LIFETIME: Duration = Duration::from_mins(5);
const MAX_CONNECTIONS: usize = 32;

#[derive(Clone, Debug)]
pub(super) struct HabitInvalidationHub {
    wake: watch::Sender<u64>,
    permits: Arc<Semaphore>,
}

impl HabitInvalidationHub {
    pub(super) fn new() -> Self {
        let (wake, _) = watch::channel(0);
        Self {
            wake,
            permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }
    }

    pub(super) fn poke(&self) {
        self.wake
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    pub(super) async fn open(
        &self,
        repository: Arc<dyn HabitRepository>,
        cursor: u64,
    ) -> Result<HabitInvalidationStream, HabitInvalidationOpenError> {
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(HabitInvalidationOpenError::from)?;
        let receiver = self.wake.subscribe();
        let head = repository.delta_head().await?;
        if cursor > head {
            return Err(HabitInvalidationOpenError::CursorAhead);
        }
        Ok(HabitInvalidationStream::new(
            repository,
            receiver,
            permit,
            cursor,
            (head > cursor).then_some(head),
        ))
    }
}

#[derive(Debug, Error)]
pub(super) enum HabitInvalidationOpenError {
    #[error("habit invalidation capacity is exhausted")]
    Capacity,
    #[error("habit cursor is ahead of the durable head")]
    CursorAhead,
    #[error(transparent)]
    Repository(#[from] HabitRepositoryError),
}

impl From<TryAcquireError> for HabitInvalidationOpenError {
    fn from(_: TryAcquireError) -> Self {
        Self::Capacity
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum HabitInvalidationSignal {
    Cursor(String),
    Heartbeat,
}

pub(super) struct HabitInvalidationStream {
    repository: Arc<dyn HabitRepository>,
    receiver: watch::Receiver<u64>,
    probe: Interval,
    heartbeat: Interval,
    expiration: Pin<Box<Sleep>>,
    _permit: OwnedSemaphorePermit,
    last_sent: u64,
    pending: Option<u64>,
}

impl HabitInvalidationStream {
    fn new(
        repository: Arc<dyn HabitRepository>,
        receiver: watch::Receiver<u64>,
        permit: OwnedSemaphorePermit,
        cursor: u64,
        pending: Option<u64>,
    ) -> Self {
        let now = Instant::now();
        let mut probe = interval_at(now + PROBE_INTERVAL, PROBE_INTERVAL);
        probe.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut heartbeat = interval_at(now + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        Self {
            repository,
            receiver,
            probe,
            heartbeat,
            expiration: Box::pin(sleep(CONNECTION_LIFETIME)),
            _permit: permit,
            last_sent: cursor,
            pending,
        }
    }

    pub(super) fn into_stream(self) -> Pin<Box<dyn Stream<Item = HabitInvalidationSignal> + Send>> {
        Box::pin(stream::unfold(self, |mut state| async move {
            if let Some(head) = state.pending.take() {
                state.last_sent = head;
                return Some((
                    HabitInvalidationSignal::Cursor(encode_delta_cursor(
                        head,
                        state.repository.cursor_scope(),
                    )),
                    state,
                ));
            }
            loop {
                enum Wake {
                    Expired,
                    Local(bool),
                    Probe,
                    Heartbeat,
                }
                let wake = tokio::select! {
                    biased;
                    () = &mut state.expiration => Wake::Expired,
                    result = state.receiver.changed() => Wake::Local(result.is_ok()),
                    _ = state.probe.tick() => Wake::Probe,
                    _ = state.heartbeat.tick() => Wake::Heartbeat,
                };
                match wake {
                    Wake::Expired | Wake::Local(false) => return None,
                    Wake::Heartbeat => {
                        return Some((HabitInvalidationSignal::Heartbeat, state));
                    }
                    Wake::Local(true) => {
                        state.receiver.borrow_and_update();
                    }
                    Wake::Probe => {}
                }
                let head = tokio::select! {
                    biased;
                    () = &mut state.expiration => return None,
                    result = state.repository.delta_head() => result,
                };
                let Ok(head) = head else {
                    return None;
                };
                if head > state.last_sent {
                    state.last_sent = head;
                    return Some((
                        HabitInvalidationSignal::Cursor(encode_delta_cursor(
                            head,
                            state.repository.cursor_scope(),
                        )),
                        state,
                    ));
                }
            }
        }))
    }
}
