use std::{
    process::ExitStatus,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};

use tokio::{process::Child, time};

use crate::{Error, Result, error::transport_error};

#[cfg(all(test, unix))]
use nix::unistd::getpgid;
#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::{
        signal::{Signal, kill, killpg},
        wait::waitpid,
    },
    unistd::Pid,
};

/// A direct child placed in its own process group.
///
/// On Unix the stored process-group id is used only after `getpgid` verifies
/// that the direct child is the group leader. This prevents a broad or
/// caller-owned process group from ever becoming a kill target.
pub(crate) struct ManagedChild {
    child: Option<Child>,
    #[cfg(unix)]
    verified_process_group: Option<Pid>,
    #[cfg(unix)]
    direct_pid: Pid,
    teardown_state: Arc<AtomicU8>,
}

const TEARDOWN_ACTIVE: u8 = 1;
const TEARDOWN_REAPED_OR_DETACHED: u8 = 2;

impl ManagedChild {
    #[cfg(test)]
    pub(crate) fn from_spawned(mut child: Child) -> Result<Self> {
        #[cfg(unix)]
        let verified_process_group = {
            let raw_pid =
                child
                    .id()
                    .and_then(|pid| i32::try_from(pid).ok())
                    .ok_or(Error::Spawn {
                        kind: std::io::ErrorKind::InvalidData,
                    })?;
            let pid = Pid::from_raw(raw_pid);
            match getpgid(Some(pid)) {
                Ok(process_group) if process_group == pid => Some(pid),
                Err(_) if child.try_wait().ok().flatten().is_some() => None,
                _ => {
                    let _ = killpg(pid, Signal::SIGKILL);
                    let _ = child.start_kill();
                    detach_tree_reaper(pid, Some(pid), Duration::ZERO);
                    return Err(Error::Spawn {
                        kind: std::io::ErrorKind::PermissionDenied,
                    });
                }
            }
        };

        Ok(Self {
            child: Some(child),
            #[cfg(unix)]
            verified_process_group,
            #[cfg(unix)]
            direct_pid: verified_process_group.ok_or(Error::Spawn {
                kind: std::io::ErrorKind::PermissionDenied,
            })?,
            teardown_state: Arc::new(AtomicU8::new(0)),
        })
    }

    pub(crate) fn child_mut(&mut self) -> Result<&mut Child> {
        self.child.as_mut().ok_or(Error::ConnectionUnusable)
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        let status = self
            .child_mut()?
            .try_wait()
            .map_err(|error| transport_error(&error))?;
        Ok(status)
    }

    pub(crate) async fn wait_for(&mut self, timeout: Duration) -> Result<Option<ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        match time::timeout(timeout, child.wait()).await {
            Ok(result) => Ok(Some(result.map_err(|error| transport_error(&error))?)),
            Err(_) => Ok(None),
        }
    }

    pub(crate) fn request_termination(&mut self) {
        #[cfg(unix)]
        if self.signal_verified_group(Signal::SIGTERM) {
            return;
        }
        if self.try_wait().ok().flatten().is_some() {
            return;
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }

    pub(crate) fn force_kill(&mut self) {
        #[cfg(unix)]
        if self.signal_verified_group(Signal::SIGKILL) {
            if let Some(child) = self.child.as_mut() {
                let _ = child.start_kill();
            }
            return;
        }
        if self.try_wait().ok().flatten().is_some() {
            return;
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }

    #[cfg(unix)]
    fn signal_verified_group(&mut self, signal: Signal) -> bool {
        let Some(process_group) = self.verified_process_group else {
            return false;
        };
        killpg(process_group, signal).is_ok()
    }

    /// Sends TERM, then KILL after one bounded grace period, and always waits
    /// for the direct child so it cannot remain a zombie.
    pub(crate) async fn terminate_and_reap(&mut self, timeout: Duration) -> Result<ExitStatus> {
        self.teardown_state
            .store(TEARDOWN_ACTIVE, Ordering::Release);
        self.request_termination();
        let grace = timeout.min(Duration::from_millis(100));
        time::sleep(grace).await;
        self.force_kill();
        let status = if let Some(status) = self.try_wait()? {
            status
        } else if let Some(status) = self.wait_for(timeout).await? {
            status
        } else {
            #[cfg(unix)]
            detach_tree_reaper(self.direct_pid, self.verified_process_group, Duration::ZERO);
            self.child.take();
            self.verified_process_group = None;
            self.teardown_state
                .store(TEARDOWN_REAPED_OR_DETACHED, Ordering::Release);
            return Err(Error::Timeout);
        };
        self.verified_process_group = None;
        self.teardown_state
            .store(TEARDOWN_REAPED_OR_DETACHED, Ordering::Release);
        Ok(status)
    }

    pub(crate) fn cancellation_guard(
        &self,
        poisoned: Arc<AtomicBool>,
        timeout: Duration,
    ) -> CancellationGuard {
        CancellationGuard {
            armed: true,
            poisoned,
            teardown_state: Arc::clone(&self.teardown_state),
            #[cfg(unix)]
            process_group: self.verified_process_group,
            #[cfg(unix)]
            direct_pid: self.direct_pid,
            grace: timeout.min(Duration::from_millis(100)),
        }
    }

    pub(crate) fn teardown_arranged_or_complete(&self) -> bool {
        self.teardown_state.load(Ordering::Acquire) == TEARDOWN_REAPED_OR_DETACHED
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.child.is_none() {
            return;
        }
        if self
            .teardown_state
            .swap(TEARDOWN_REAPED_OR_DETACHED, Ordering::AcqRel)
            != TEARDOWN_REAPED_OR_DETACHED
        {
            self.request_termination();
            #[cfg(unix)]
            detach_tree_reaper(
                self.direct_pid,
                self.verified_process_group,
                Duration::from_millis(100),
            );
        }
        self.child.take();
    }
}

#[cfg(unix)]
fn detach_tree_reaper(direct_pid: Pid, process_group: Option<Pid>, grace: Duration) {
    let spawned = std::thread::Builder::new()
        .name("dayweave-codex-reaper".to_owned())
        .spawn(move || reap_process_tree(direct_pid, process_group, grace));
    if spawned.is_err() {
        // Thread creation can fail under resource pressure. Drop must still
        // complete the bounded escalation and reap synchronously.
        reap_process_tree(direct_pid, process_group, grace);
    }
}

#[cfg(unix)]
fn reap_process_tree(direct_pid: Pid, process_group: Option<Pid>, grace: Duration) {
    std::thread::sleep(grace);
    if let Some(process_group) = process_group {
        let _ = killpg(process_group, Signal::SIGKILL);
    }
    let _ = kill(direct_pid, Signal::SIGKILL);
    while let Err(Errno::EINTR) = waitpid(direct_pid, None) {}
}

pub(crate) struct CancellationGuard {
    armed: bool,
    poisoned: Arc<AtomicBool>,
    teardown_state: Arc<AtomicU8>,
    #[cfg(unix)]
    process_group: Option<Pid>,
    #[cfg(unix)]
    direct_pid: Pid,
    grace: Duration,
}

impl CancellationGuard {
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.poisoned.store(true, Ordering::Release);
        if self
            .teardown_state
            .swap(TEARDOWN_REAPED_OR_DETACHED, Ordering::AcqRel)
            == TEARDOWN_REAPED_OR_DETACHED
        {
            return;
        }
        #[cfg(unix)]
        {
            if let Some(process_group) = self.process_group {
                let _ = killpg(process_group, Signal::SIGTERM);
            } else {
                let _ = kill(self.direct_pid, Signal::SIGTERM);
            }
            detach_tree_reaper(self.direct_pid, self.process_group, self.grace);
        }
    }
}
