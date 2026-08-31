use std::{pin::Pin, sync::Arc, time::Duration};

use futures_util::{Stream, stream};
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError, watch},
    time::{Instant, Interval, MissedTickBehavior, Sleep, interval_at, sleep},
};

use super::{ItemRepository, ItemRepositoryError, service::encode_cursor};

const MAX_PROBE_INTERVAL: Duration = Duration::from_secs(5);
const MAX_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const MAX_CONNECTION_LIFETIME: Duration = Duration::from_mins(5);
const DEFAULT_MAX_CONNECTIONS: usize = 32;

/// Bounded timing and resource limits for item invalidation streams.
///
/// Production uses [`Self::default`]. The checked constructor permits shorter
/// values for deterministic tests without weakening the production convergence,
/// heartbeat, or lifetime bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItemInvalidationConfig {
    probe_interval: Duration,
    heartbeat_interval: Duration,
    connection_lifetime: Duration,
    max_connections: usize,
}

impl ItemInvalidationConfig {
    /// Builds a configuration within the stream's fixed safety bounds.
    ///
    /// # Errors
    ///
    /// Returns an error for zero durations/capacity or for durations that
    /// exceed the production convergence, heartbeat, or connection limits.
    pub fn new(
        probe_interval: Duration,
        heartbeat_interval: Duration,
        connection_lifetime: Duration,
        max_connections: usize,
    ) -> Result<Self, ItemInvalidationConfigError> {
        if probe_interval.is_zero() || heartbeat_interval.is_zero() || connection_lifetime.is_zero()
        {
            return Err(ItemInvalidationConfigError::ZeroDuration);
        }
        if probe_interval > MAX_PROBE_INTERVAL {
            return Err(ItemInvalidationConfigError::ProbeIntervalTooLong);
        }
        if heartbeat_interval > MAX_HEARTBEAT_INTERVAL {
            return Err(ItemInvalidationConfigError::HeartbeatIntervalTooLong);
        }
        if connection_lifetime > MAX_CONNECTION_LIFETIME {
            return Err(ItemInvalidationConfigError::ConnectionLifetimeTooLong);
        }
        if max_connections == 0 {
            return Err(ItemInvalidationConfigError::ZeroCapacity);
        }
        Ok(Self {
            probe_interval,
            heartbeat_interval,
            connection_lifetime,
            max_connections,
        })
    }
}

impl Default for ItemInvalidationConfig {
    fn default() -> Self {
        Self {
            probe_interval: MAX_PROBE_INTERVAL,
            heartbeat_interval: MAX_HEARTBEAT_INTERVAL,
            connection_lifetime: MAX_CONNECTION_LIFETIME,
            max_connections: DEFAULT_MAX_CONNECTIONS,
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ItemInvalidationConfigError {
    #[error("item invalidation durations must be positive")]
    ZeroDuration,
    #[error("item invalidation probe interval must not exceed five seconds")]
    ProbeIntervalTooLong,
    #[error("item invalidation heartbeat interval must not exceed fifteen seconds")]
    HeartbeatIntervalTooLong,
    #[error("item invalidation connection lifetime must not exceed five minutes")]
    ConnectionLifetimeTooLong,
    #[error("item invalidation stream capacity must be positive")]
    ZeroCapacity,
}

#[derive(Clone, Debug)]
pub(super) struct ItemInvalidationHub {
    wake_generation: watch::Sender<u64>,
    permits: Arc<Semaphore>,
    config: ItemInvalidationConfig,
}

impl ItemInvalidationHub {
    pub(super) fn new(config: ItemInvalidationConfig) -> Self {
        let (wake_generation, _) = watch::channel(0);
        Self {
            wake_generation,
            permits: Arc::new(Semaphore::new(config.max_connections)),
            config,
        }
    }

    /// Coalesces a successful process-local item command into a wakeup. The
    /// stream still re-reads the durable head before emitting anything, so a
    /// replay or a command that produced no new delta row remains harmless.
    pub(super) fn poke(&self) {
        self.wake_generation.send_modify(|generation| {
            *generation = generation.wrapping_add(1);
        });
    }

    /// Reserves a bounded stream, subscribes before reading the authoritative
    /// repository head, and prepares an immediate coalesced catch-up when the
    /// caller's durable cursor is behind.
    pub(super) async fn open(
        &self,
        repository: Arc<dyn ItemRepository>,
        cursor: u64,
    ) -> Result<ItemInvalidationStream, ItemInvalidationOpenError> {
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(ItemInvalidationOpenError::from)?;

        // This ordering closes the in-process race: a successful command that
        // commits while the authoritative head is loading either appears in
        // that head or remains pending in this receiver.
        let receiver = self.wake_generation.subscribe();
        let head = repository.delta_head().await?;
        if cursor > head {
            return Err(ItemInvalidationOpenError::CursorAhead);
        }
        Ok(ItemInvalidationStream::new(
            repository,
            receiver,
            permit,
            self.config,
            cursor,
            (cursor < head).then_some(head),
        ))
    }
}

#[derive(Debug, Error)]
pub(super) enum ItemInvalidationOpenError {
    #[error("item invalidation cursor is invalid")]
    InvalidCursor,
    #[error("item invalidation stream capacity is exhausted")]
    Capacity,
    #[error("item invalidation cursor is ahead of the authoritative head")]
    CursorAhead,
    #[error(transparent)]
    Repository(#[from] ItemRepositoryError),
}

impl From<TryAcquireError> for ItemInvalidationOpenError {
    fn from(_: TryAcquireError) -> Self {
        Self::Capacity
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ItemInvalidationSignal {
    Cursor(String),
    Heartbeat,
}

pub(super) struct ItemInvalidationStream {
    repository: Arc<dyn ItemRepository>,
    receiver: watch::Receiver<u64>,
    probe: Interval,
    heartbeat: Interval,
    expiration: Pin<Box<Sleep>>,
    _permit: OwnedSemaphorePermit,
    last_sent_sequence: u64,
    pending_sequence: Option<u64>,
}

impl ItemInvalidationStream {
    fn new(
        repository: Arc<dyn ItemRepository>,
        receiver: watch::Receiver<u64>,
        permit: OwnedSemaphorePermit,
        config: ItemInvalidationConfig,
        cursor: u64,
        pending_sequence: Option<u64>,
    ) -> Self {
        let now = Instant::now();
        let mut probe = interval_at(now + config.probe_interval, config.probe_interval);
        probe.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut heartbeat = interval_at(now + config.heartbeat_interval, config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let expiration = Box::pin(sleep(config.connection_lifetime));
        Self {
            repository,
            receiver,
            probe,
            heartbeat,
            expiration,
            _permit: permit,
            last_sent_sequence: cursor,
            pending_sequence,
        }
    }

    pub(super) fn into_stream(self) -> Pin<Box<dyn Stream<Item = ItemInvalidationSignal> + Send>> {
        Box::pin(stream::unfold(self, |mut state| async move {
            if let Some(sequence) = state.pending_sequence.take() {
                state.last_sent_sequence = sequence;
                let cursor = encode_cursor(sequence, state.repository.cursor_scope());
                return Some((ItemInvalidationSignal::Cursor(cursor), state));
            }

            loop {
                enum Wakeup {
                    Expired,
                    Published(bool),
                    Probe,
                    Heartbeat,
                }

                let wakeup = tokio::select! {
                    biased;
                    () = &mut state.expiration => Wakeup::Expired,
                    _ = state.probe.tick() => Wakeup::Probe,
                    result = state.receiver.changed() => Wakeup::Published(result.is_ok()),
                    _ = state.heartbeat.tick() => Wakeup::Heartbeat,
                };

                match wakeup {
                    Wakeup::Expired => return None,
                    Wakeup::Published(open) => {
                        if !open {
                            return None;
                        }
                        state.receiver.borrow_and_update();
                        let head = tokio::select! {
                            biased;
                            () = &mut state.expiration => return None,
                            result = state.repository.delta_head() => result,
                        };
                        let Ok(head) = head else {
                            return None;
                        };
                        if head > state.last_sent_sequence {
                            state.last_sent_sequence = head;
                            let cursor = encode_cursor(head, state.repository.cursor_scope());
                            return Some((ItemInvalidationSignal::Cursor(cursor), state));
                        }
                    }
                    Wakeup::Probe => {
                        // Keep the lifetime bound effective even if an
                        // authoritative repository probe becomes slow.
                        let head = tokio::select! {
                            biased;
                            () = &mut state.expiration => return None,
                            result = state.repository.delta_head() => result,
                        };
                        let Ok(head) = head else {
                            // Once HTTP 200 has begun, ending the content-free
                            // stream is safer than serializing repository detail.
                            return None;
                        };
                        if head > state.last_sent_sequence {
                            state.last_sent_sequence = head;
                            let cursor = encode_cursor(head, state.repository.cursor_scope());
                            return Some((ItemInvalidationSignal::Cursor(cursor), state));
                        }
                    }
                    Wakeup::Heartbeat => {
                        return Some((ItemInvalidationSignal::Heartbeat, state));
                    }
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_weaker_or_degenerate_bounds() {
        assert_eq!(
            ItemInvalidationConfig::new(
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(1),
                1,
            ),
            Err(ItemInvalidationConfigError::ZeroDuration)
        );
        assert_eq!(
            ItemInvalidationConfig::new(
                Duration::from_secs(6),
                Duration::from_secs(1),
                Duration::from_secs(1),
                1,
            ),
            Err(ItemInvalidationConfigError::ProbeIntervalTooLong)
        );
        assert_eq!(
            ItemInvalidationConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(16),
                Duration::from_secs(1),
                1,
            ),
            Err(ItemInvalidationConfigError::HeartbeatIntervalTooLong)
        );
        assert_eq!(
            ItemInvalidationConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(301),
                1,
            ),
            Err(ItemInvalidationConfigError::ConnectionLifetimeTooLong)
        );
        assert_eq!(
            ItemInvalidationConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
                0,
            ),
            Err(ItemInvalidationConfigError::ZeroCapacity)
        );
    }
}
