//! Guest time. By default it is the machine's. When `sf.initialize` gives a
//! `clock`, it is the host's: it moves only with `sf.clock.advance`, and
//! spinfoam names its earliest timer to the host with `sf.clock.next`, so a
//! host that runs on a test clock decides when every guest timer fires.
use crate::outbox::Outbox;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    future::Future,
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

struct HostTime {
    monotonic_ms: Cell<u64>,
    unix_ms: Cell<u64>,
    timers: RefCell<BTreeMap<(u64, u64), oneshot::Sender<()>>>,
    next_seq: Cell<u64>,
    announced: Cell<Option<u64>>,
    // Timers fired whose sleepers have not run yet.
    woken: RefCell<BTreeSet<(u64, u64)>>,
    out: Outbox,
}

impl HostTime {
    // Names the earliest timer to the host when it changed.
    fn announce(&self) {
        let next = self.timers.borrow().keys().next().map(|(due, _)| *due);
        if self.announced.replace(next) != next {
            self.out
                .try_control(json!({"jsonrpc":"2.0","method":"sf.clock.next",
                "params":{"due_monotonic_ms":next}}));
        }
    }
}

// Removes a host timer when its sleep ends, fired or not.
struct TimerGuard {
    host: Rc<HostTime>,
    key: (u64, u64),
}
impl Drop for TimerGuard {
    fn drop(&mut self) {
        self.host.woken.borrow_mut().remove(&self.key);
        if self.host.timers.borrow_mut().remove(&self.key).is_some() {
            self.host.announce();
        }
    }
}

#[derive(Clone)]
pub struct Clock {
    started: Instant,
    host: Rc<RefCell<Option<Rc<HostTime>>>>,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            host: Default::default(),
        }
    }
}

impl Clock {
    /// Puts guest time on the host's clock, at the given times.
    pub fn use_host(&self, out: Outbox, monotonic_ms: u64, unix_ms: u64) {
        *self.host.borrow_mut() = Some(Rc::new(HostTime {
            monotonic_ms: Cell::new(monotonic_ms),
            unix_ms: Cell::new(unix_ms),
            timers: Default::default(),
            next_seq: Cell::new(0),
            announced: Cell::new(None),
            woken: Default::default(),
            out,
        }));
    }

    pub fn is_host(&self) -> bool {
        self.host.borrow().is_some()
    }

    fn host_time(&self) -> Option<Rc<HostTime>> {
        self.host.borrow().clone()
    }

    pub fn monotonic_ms(&self) -> u64 {
        match self.host_time() {
            Some(host) => host.monotonic_ms.get(),
            None => self.started.elapsed().as_millis() as u64,
        }
    }

    pub fn unix_ms(&self) -> u64 {
        match self.host_time() {
            Some(host) => host.unix_ms.get(),
            None => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }

    pub async fn sleep(&self, ms: u64) {
        let Some(host) = self.host_time() else {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            return;
        };
        let due = host.monotonic_ms.get().saturating_add(ms);
        let seq = host.next_seq.get();
        host.next_seq.set(seq + 1);
        let (tx, rx) = oneshot::channel();
        host.timers.borrow_mut().insert((due, seq), tx);
        host.announce();
        let _guard = TimerGuard {
            host,
            key: (due, seq),
        };
        let _ = rx.await;
    }

    /// `future`'s output, or `Err(())` when `ms` pass first.
    pub async fn timeout<F: Future>(&self, ms: u64, future: F) -> Result<F::Output, ()> {
        tokio::select! {
            biased;
            output = future => Ok(output),
            _ = self.sleep(ms) => Err(()),
        }
    }

    /// Moves the host clock and fires the timers now due. Time never goes back.
    pub fn advance(&self, monotonic_ms: u64, unix_ms: u64) -> Result<(), &'static str> {
        let host = self.host_time().ok_or("the clock is not the host's")?;
        if monotonic_ms < host.monotonic_ms.get() {
            return Err("monotonic time cannot go back");
        }
        host.monotonic_ms.set(monotonic_ms);
        host.unix_ms.set(unix_ms);
        loop {
            let first = host.timers.borrow().keys().next().copied();
            match first {
                Some(key) if key.0 <= monotonic_ms => {
                    let fired = host.timers.borrow_mut().remove(&key);
                    if let Some(tx) = fired
                        && tx.send(()).is_ok()
                    {
                        host.woken.borrow_mut().insert(key);
                    }
                }
                _ => break,
            }
        }
        host.announce();
        Ok(())
    }

    /// Whether a fired timer's sleeper has yet to run.
    pub fn waking(&self) -> bool {
        self.host_time()
            .is_some_and(|host| !host.woken.borrow().is_empty())
    }

    /// The earliest timer's due time on the host clock.
    pub fn next_due(&self) -> Option<u64> {
        self.host_time()
            .and_then(|host| host.timers.borrow().keys().next().map(|(due, _)| *due))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn host_timers_fire_only_when_the_host_moves_the_clock() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let out = Outbox::default();
                let clock = Clock::default();
                clock.use_host(out.clone(), 1_000, 5_000);
                let fired = Rc::new(Cell::new(false));
                let (c, f) = (clock.clone(), fired.clone());
                let sleeper = tokio::task::spawn_local(async move {
                    c.sleep(50).await;
                    f.set(true);
                });
                tokio::task::yield_now().await;
                assert_eq!(clock.next_due(), Some(1_050));
                let notice: serde_json::Value =
                    serde_json::from_slice(&out.next().await.unwrap()).unwrap();
                assert_eq!(notice["method"], "sf.clock.next");
                assert_eq!(notice["params"]["due_monotonic_ms"], 1_050);

                tokio::time::sleep(Duration::from_millis(80)).await;
                assert!(!fired.get());

                clock.advance(1_049, 5_049).unwrap();
                tokio::task::yield_now().await;
                assert!(!fired.get());
                clock.advance(1_050, 5_050).unwrap();
                sleeper.await.unwrap();
                assert!(fired.get());
                assert_eq!(clock.unix_ms(), 5_050);
                assert_eq!(clock.next_due(), None);
                assert!(clock.advance(1_000, 5_000).is_err());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_timeout_that_loses_removes_its_timer() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let clock = Clock::default();
                clock.use_host(Outbox::default(), 0, 0);
                assert_eq!(clock.timeout(1_000, async { 7 }).await, Ok(7));
                assert_eq!(clock.next_due(), None);
            })
            .await;
    }
}
