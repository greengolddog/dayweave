use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use futures_util::{Stream, stream};
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError, watch},
    time::{Instant, Interval, MissedTickBehavior, Sleep, interval_at, sleep},
};

use super::{PostgresSchedulingRepository, SchedulingPortError};

const MAX_PROBE_INTERVAL: Duration = Duration::from_secs(5);
const MAX_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const MAX_CONNECTION_LIFETIME: Duration = Duration::from_mins(5);
const DEFAULT_MAX_CONNECTIONS: usize = 32;

/// Bounded timing and resource limits for native schedule invalidation streams.
///
/// The stream carries only a monotonically increasing published revision. A
/// reconnecting client always fetches the immutable current snapshot over the
/// ordinary JSON route before advancing its durable cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduleInvalidationConfig {
    probe_interval: Duration,
    heartbeat_interval: Duration,
    connection_lifetime: Duration,
    max_connections: usize,
}

impl ScheduleInvalidationConfig {
    /// Builds a configuration within the production convergence and resource
    /// bounds. Shorter values are available to deterministic tests.
    ///
    /// # Errors
    ///
    /// Returns an error for zero values or a duration exceeding its production
    /// maximum.
    pub fn new(
        probe_interval: Duration,
        heartbeat_interval: Duration,
        connection_lifetime: Duration,
        max_connections: usize,
    ) -> Result<Self, ScheduleInvalidationConfigError> {
        if probe_interval.is_zero() || heartbeat_interval.is_zero() || connection_lifetime.is_zero()
        {
            return Err(ScheduleInvalidationConfigError::ZeroDuration);
        }
        if probe_interval > MAX_PROBE_INTERVAL {
            return Err(ScheduleInvalidationConfigError::ProbeIntervalTooLong);
        }
        if heartbeat_interval > MAX_HEARTBEAT_INTERVAL {
            return Err(ScheduleInvalidationConfigError::HeartbeatIntervalTooLong);
        }
        if connection_lifetime > MAX_CONNECTION_LIFETIME {
            return Err(ScheduleInvalidationConfigError::ConnectionLifetimeTooLong);
        }
        if max_connections == 0 {
            return Err(ScheduleInvalidationConfigError::ZeroCapacity);
        }
        Ok(Self {
            probe_interval,
            heartbeat_interval,
            connection_lifetime,
            max_connections,
        })
    }
}

impl Default for ScheduleInvalidationConfig {
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
pub enum ScheduleInvalidationConfigError {
    #[error("schedule invalidation durations must be positive")]
    ZeroDuration,
    #[error("schedule invalidation probe interval must not exceed five seconds")]
    ProbeIntervalTooLong,
    #[error("schedule invalidation heartbeat interval must not exceed fifteen seconds")]
    HeartbeatIntervalTooLong,
    #[error("schedule invalidation connection lifetime must not exceed five minutes")]
    ConnectionLifetimeTooLong,
    #[error("schedule invalidation stream capacity must be positive")]
    ZeroCapacity,
}

#[derive(Clone, Debug)]
pub(crate) struct ScheduleInvalidationHub {
    high_water: watch::Sender<u64>,
    permits: Arc<Semaphore>,
    config: ScheduleInvalidationConfig,
}

impl ScheduleInvalidationHub {
    pub(crate) fn new(config: ScheduleInvalidationConfig) -> Self {
        let (high_water, _) = watch::channel(0);
        Self {
            high_water,
            permits: Arc::new(Semaphore::new(config.max_connections)),
            config,
        }
    }

    /// Advances the process-local wakeup high-water monotonically. Every
    /// stream still verifies the durable database head before emitting catch-up.
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

    pub(crate) async fn open(
        &self,
        repository: PostgresSchedulingRepository,
        cursor: u64,
    ) -> Result<ScheduleInvalidationStream, ScheduleInvalidationOpenError> {
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(ScheduleInvalidationOpenError::from)?;
        let receiver = self.high_water.subscribe();
        let head = repository.published_revision_head().await?;
        if cursor > head {
            return Err(ScheduleInvalidationOpenError::CursorAhead { cursor, head });
        }
        self.publish(head);
        Ok(ScheduleInvalidationStream::new(
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
pub(crate) enum ScheduleInvalidationOpenError {
    #[error("the authenticated principal does not own this schedule scope")]
    AccessDenied,
    #[error("schedule invalidation stream capacity is exhausted")]
    Capacity,
    #[error("schedule invalidation cursor {cursor} is ahead of authoritative head {head}")]
    CursorAhead { cursor: u64, head: u64 },
    #[error(transparent)]
    Repository(#[from] SchedulingPortError),
}

impl From<TryAcquireError> for ScheduleInvalidationOpenError {
    fn from(_: TryAcquireError) -> Self {
        Self::Capacity
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScheduleInvalidationSignal {
    Revision(u64),
    Heartbeat,
}

pub(crate) struct ScheduleInvalidationStream {
    repository: PostgresSchedulingRepository,
    hub: ScheduleInvalidationHub,
    receiver: watch::Receiver<u64>,
    probe: Interval,
    heartbeat: Interval,
    expiration: Pin<Box<Sleep>>,
    _permit: OwnedSemaphorePermit,
    last_sent_revision: u64,
    pending_revision: Option<u64>,
}

enum ScheduleProbeWake<T> {
    Expired,
    Heartbeat,
    Complete(T),
}

async fn await_schedule_probe<F, T>(
    probe: F,
    heartbeat: &mut Interval,
    expiration: &mut Pin<Box<Sleep>>,
) -> ScheduleProbeWake<T>
where
    F: Future<Output = T>,
{
    tokio::select! {
        biased;
        () = expiration.as_mut() => ScheduleProbeWake::Expired,
        result = probe => ScheduleProbeWake::Complete(result),
        _ = heartbeat.tick() => ScheduleProbeWake::Heartbeat,
    }
}

impl ScheduleInvalidationStream {
    fn new(
        repository: PostgresSchedulingRepository,
        hub: ScheduleInvalidationHub,
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
    ) -> Pin<Box<dyn Stream<Item = ScheduleInvalidationSignal> + Send>> {
        Box::pin(stream::unfold(self, |mut state| async move {
            if let Some(revision) = state.pending_revision.take() {
                state.last_sent_revision = revision;
                return Some((ScheduleInvalidationSignal::Revision(revision), state));
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
                            return Some((ScheduleInvalidationSignal::Revision(revision), state));
                        }
                    }
                    Wakeup::Probe => {
                        let head = match await_schedule_probe(
                            state.repository.published_revision_head(),
                            &mut state.heartbeat,
                            &mut state.expiration,
                        )
                        .await
                        {
                            ScheduleProbeWake::Expired => return None,
                            ScheduleProbeWake::Heartbeat => {
                                return Some((ScheduleInvalidationSignal::Heartbeat, state));
                            }
                            ScheduleProbeWake::Complete(result) => result,
                        };
                        let Ok(head) = head else {
                            return None;
                        };
                        state.hub.publish(head);
                        if head > state.last_sent_revision {
                            state.last_sent_revision = head;
                            return Some((ScheduleInvalidationSignal::Revision(head), state));
                        }
                    }
                    Wakeup::Heartbeat => {
                        return Some((ScheduleInvalidationSignal::Heartbeat, state));
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
            ScheduleInvalidationConfig::new(
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(1),
                1,
            ),
            Err(ScheduleInvalidationConfigError::ZeroDuration)
        );
        assert_eq!(
            ScheduleInvalidationConfig::new(
                Duration::from_secs(6),
                Duration::from_secs(1),
                Duration::from_secs(1),
                1,
            ),
            Err(ScheduleInvalidationConfigError::ProbeIntervalTooLong)
        );
        assert_eq!(
            ScheduleInvalidationConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(16),
                Duration::from_secs(1),
                1,
            ),
            Err(ScheduleInvalidationConfigError::HeartbeatIntervalTooLong)
        );
        assert_eq!(
            ScheduleInvalidationConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(301),
                1,
            ),
            Err(ScheduleInvalidationConfigError::ConnectionLifetimeTooLong)
        );
        assert_eq!(
            ScheduleInvalidationConfig::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
                0,
            ),
            Err(ScheduleInvalidationConfigError::ZeroCapacity)
        );
    }

    #[tokio::test]
    async fn stalled_durable_probe_does_not_suppress_heartbeat() {
        let now = Instant::now();
        let mut heartbeat = interval_at(now + Duration::from_millis(10), Duration::from_millis(10));
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut expiration = Box::pin(sleep(Duration::from_millis(200)));

        for _ in 0..2 {
            let wake = tokio::time::timeout(
                Duration::from_millis(100),
                await_schedule_probe(
                    std::future::pending::<()>(),
                    &mut heartbeat,
                    &mut expiration,
                ),
            )
            .await
            .expect("every heartbeat must win a stalled durable probe");
            assert!(matches!(wake, ScheduleProbeWake::Heartbeat));
        }
    }
}
