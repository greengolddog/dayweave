use std::{pin::Pin, sync::Arc, time::Duration};

use futures_util::{Stream, stream};
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError, watch},
    time::{Instant, Interval, MissedTickBehavior, Sleep, interval_at, sleep},
};

use super::{ExecutionRepository, ExecutionRepositoryError};

const MAX_PROBE_INTERVAL: Duration = Duration::from_secs(5);
const MAX_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const MAX_CONNECTION_LIFETIME: Duration = Duration::from_mins(5);
const DEFAULT_MAX_CONNECTIONS: usize = 32;

/// Bounded timing and resource limits for execution invalidation streams.
///
/// Production uses [`Self::default`]. The checked constructor permits shorter
/// values for deterministic embedded and HTTP tests without permitting a
/// configuration that weakens the production convergence/lifetime bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionInvalidationConfig {
    probe_interval: Duration,
    heartbeat_interval: Duration,
    connection_lifetime: Duration,
    max_connections: usize,
}

impl ExecutionInvalidationConfig {
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
    ) -> Result<Self, ExecutionInvalidationConfigError> {
        if probe_interval.is_zero() || heartbeat_interval.is_zero() || connection_lifetime.is_zero()
        {
            return Err(ExecutionInvalidationConfigError::ZeroDuration);
        }
        if probe_interval > MAX_PROBE_INTERVAL {
            return Err(ExecutionInvalidationConfigError::ProbeIntervalTooLong);
        }
        if heartbeat_interval > MAX_HEARTBEAT_INTERVAL {
            return Err(ExecutionInvalidationConfigError::HeartbeatIntervalTooLong);
        }
        if connection_lifetime > MAX_CONNECTION_LIFETIME {
            return Err(ExecutionInvalidationConfigError::ConnectionLifetimeTooLong);
        }
        if max_connections == 0 {
            return Err(ExecutionInvalidationConfigError::ZeroCapacity);
        }
        Ok(Self {
            probe_interval,
            heartbeat_interval,
            connection_lifetime,
            max_connections,
        })
    }
}

impl Default for ExecutionInvalidationConfig {
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
pub enum ExecutionInvalidationConfigError {
    #[error("execution invalidation durations must be positive")]
    ZeroDuration,
    #[error("execution invalidation probe interval must not exceed five seconds")]
    ProbeIntervalTooLong,
    #[error("execution invalidation heartbeat interval must not exceed fifteen seconds")]
    HeartbeatIntervalTooLong,
    #[error("execution invalidation connection lifetime must not exceed five minutes")]
    ConnectionLifetimeTooLong,
    #[error("execution invalidation stream capacity must be positive")]
    ZeroCapacity,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecutionInvalidationHub {
    high_water: watch::Sender<u64>,
    permits: Arc<Semaphore>,
    config: ExecutionInvalidationConfig,
}

impl ExecutionInvalidationHub {
    pub(crate) fn new(config: ExecutionInvalidationConfig) -> Self {
        let (high_water, _) = watch::channel(0);
        Self {
            high_water,
            permits: Arc::new(Semaphore::new(config.max_connections)),
            config,
        }
    }

    /// Advances the process-local wakeup high-water monotonically.
    pub(crate) fn publish(&self, revision: u64) {
        self.high_water.send_if_modified(|high_water| {
            if revision > *high_water {
                *high_water = revision;
                true
            } else {
                false
            }
        });
    }

    /// Reserves a bounded stream, subscribes before reading the authoritative
    /// repository head, and prepares an immediate coalesced catch-up when the
    /// caller's durable cursor is behind.
    pub(crate) async fn open(
        &self,
        repository: Arc<dyn ExecutionRepository>,
        cursor: u64,
    ) -> Result<ExecutionInvalidationStream, ExecutionInvalidationOpenError> {
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(ExecutionInvalidationOpenError::from)?;

        // This ordering closes the in-process race: a successful command that
        // commits while the authoritative head is loading either appears in
        // that head or remains pending in this receiver.
        let receiver = self.high_water.subscribe();
        let head = repository.snapshot().await?.revision;
        if cursor > head {
            return Err(ExecutionInvalidationOpenError::CursorAhead { cursor, head });
        }
        self.publish(head);

        Ok(ExecutionInvalidationStream::new(
            repository,
            self.clone(),
            receiver,
            permit,
            cursor,
            (cursor < head).then_some(head),
        ))
    }
}

#[derive(Debug, Error)]
pub(crate) enum ExecutionInvalidationOpenError {
    #[error("execution invalidation stream capacity is exhausted")]
    Capacity,
    #[error("execution invalidation cursor {cursor} is ahead of authoritative head {head}")]
    CursorAhead { cursor: u64, head: u64 },
    #[error(transparent)]
    Repository(#[from] ExecutionRepositoryError),
}

impl From<TryAcquireError> for ExecutionInvalidationOpenError {
    fn from(_: TryAcquireError) -> Self {
        Self::Capacity
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionInvalidationSignal {
    Revision(u64),
    Heartbeat,
}

pub(crate) struct ExecutionInvalidationStream {
    repository: Arc<dyn ExecutionRepository>,
    hub: ExecutionInvalidationHub,
    receiver: watch::Receiver<u64>,
    probe: Interval,
    heartbeat: Interval,
    expiration: Pin<Box<Sleep>>,
    _permit: OwnedSemaphorePermit,
    last_sent_revision: u64,
    pending_revision: Option<u64>,
}

impl ExecutionInvalidationStream {
    fn new(
        repository: Arc<dyn ExecutionRepository>,
        hub: ExecutionInvalidationHub,
        receiver: watch::Receiver<u64>,
        permit: OwnedSemaphorePermit,
        cursor: u64,
        pending_revision: Option<u64>,
    ) -> Self {
        let now = Instant::now();
        let mut probe = interval_at(now + hub.config.probe_interval, hub.config.probe_interval);
        probe.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut heartbeat = interval_at(
            now + hub.config.heartbeat_interval,
            hub.config.heartbeat_interval,
        );
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let expiration = Box::pin(sleep(hub.config.connection_lifetime));
        Self {
            repository,
            hub,
            receiver,
            probe,
            heartbeat,
            expiration,
            _permit: permit,
            last_sent_revision: cursor,
            pending_revision,
        }
    }

    pub(crate) fn into_stream(
        self,
    ) -> Pin<Box<dyn Stream<Item = ExecutionInvalidationSignal> + Send>> {
        Box::pin(stream::unfold(self, |mut state| async move {
            if let Some(revision) = state.pending_revision.take() {
                state.last_sent_revision = revision;
                return Some((ExecutionInvalidationSignal::Revision(revision), state));
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
                        let revision = *state.receiver.borrow_and_update();
                        if revision > state.last_sent_revision {
                            state.last_sent_revision = revision;
                            return Some((ExecutionInvalidationSignal::Revision(revision), state));
                        }
                    }
                    Wakeup::Probe => {
                        // Keep the lifetime bound effective even if an
                        // authoritative repository probe becomes slow.
                        let snapshot = tokio::select! {
                            biased;
                            () = &mut state.expiration => return None,
                            result = state.repository.snapshot() => result,
                        };
                        let Ok(snapshot) = snapshot else {
                            // Once HTTP 200 has begun, ending the content-free
                            // stream is safer than serializing repository detail.
                            return None;
                        };
                        state.hub.publish(snapshot.revision);
                        if snapshot.revision > state.last_sent_revision {
                            state.last_sent_revision = snapshot.revision;
                            return Some((
                                ExecutionInvalidationSignal::Revision(snapshot.revision),
                                state,
                            ));
                        }
                    }
                    Wakeup::Heartbeat => {
                        return Some((ExecutionInvalidationSignal::Heartbeat, state));
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
            ExecutionInvalidationConfig::new(
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(1),
                1,
            ),
            Err(ExecutionInvalidationConfigError::ZeroDuration)
        );
        assert_eq!(
            ExecutionInvalidationConfig::new(
                Duration::from_secs(6),
                Duration::from_secs(1),
                Duration::from_secs(1),
                1,
            ),
            Err(ExecutionInvalidationConfigError::ProbeIntervalTooLong)
        );
        assert_eq!(
            ExecutionInvalidationConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(16),
                Duration::from_secs(1),
                1,
            ),
            Err(ExecutionInvalidationConfigError::HeartbeatIntervalTooLong)
        );
        assert_eq!(
            ExecutionInvalidationConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(301),
                1,
            ),
            Err(ExecutionInvalidationConfigError::ConnectionLifetimeTooLong)
        );
        assert_eq!(
            ExecutionInvalidationConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
                0,
            ),
            Err(ExecutionInvalidationConfigError::ZeroCapacity)
        );
    }
}
