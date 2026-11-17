//! A shared deadline that can pause during human-only interactions.
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct Clock {
    inner: Arc<Mutex<Inner>>,
}
struct Inner {
    start: Instant,
    budget: Duration,
    manual: Option<Duration>,
    paused_at: Option<Duration>,
    paused_total: Duration,
    depth: usize,
}
impl Inner {
    fn now(&self) -> Duration {
        self.manual.unwrap_or_else(|| self.start.elapsed())
    }
}
impl Clock {
    pub fn new(budget: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                start: Instant::now(),
                budget,
                manual: None,
                paused_at: None,
                paused_total: Duration::ZERO,
                depth: 0,
            })),
        }
    }
    pub fn manual(budget: Duration) -> Self {
        let s = Self::new(budget);
        s.inner.lock().unwrap().manual = Some(Duration::ZERO);
        s
    }
    pub fn advance(&self, d: Duration) {
        let mut i = self.inner.lock().unwrap();
        let now = i.now();
        i.manual = Some(now + d);
    }
    pub fn remaining(&self) -> Duration {
        let i = self.inner.lock().unwrap();
        let end = i.paused_at.unwrap_or_else(|| i.now());
        i.budget.saturating_sub(end.saturating_sub(i.paused_total))
    }
    pub fn paused(&self) -> bool {
        self.inner.lock().unwrap().depth > 0
    }
    pub fn pause(&self) -> Pause {
        let mut i = self.inner.lock().unwrap();
        if i.depth == 0 {
            i.paused_at = Some(i.now());
        }
        i.depth += 1;
        Pause(self.clone())
    }
}
pub struct Pause(Clock);
impl Drop for Pause {
    fn drop(&mut self) {
        let mut i = self.0.inner.lock().unwrap();
        i.depth -= 1;
        if i.depth == 0 {
            let elapsed = i.now().saturating_sub(i.paused_at.take().unwrap());
            i.paused_total += elapsed;
        }
    }
}
