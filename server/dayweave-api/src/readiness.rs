use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, Default)]
pub struct Readiness(Arc<AtomicBool>);

impl Readiness {
    pub fn set_ready(&self, ready: bool) {
        self.0.store(ready, Ordering::Release);
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
