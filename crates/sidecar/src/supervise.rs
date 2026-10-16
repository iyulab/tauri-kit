//! Keep a sidecar running for as long as the app wants it.
//!
//! Starting a sidecar and waiting until it is ready is the easy half. After that every app answers
//! the same questions again: is it still alive, what happens when it dies, when does the app stop
//! starting it again, and in which order does it come down. The failures are quiet — a sidecar that
//! died leaves every later request failing until the app is restarted; one started again in a loop
//! hides a crash that happens on every start; a watcher that holds the sidecar's lock while it asks
//! something that takes the same lock freezes whoever waits on it.
//!
//! [`Supervisor`] takes a function that starts the sidecar once and does the rest:
//!
//! - it notices the sidecar ending, and with [`Health`], a sidecar that is alive but no longer
//!   answers, which it stops;
//! - it starts it again with the same function, up to a [`RestartPolicy`] — so many starts within
//!   a time window — and after that leaves it down until the app asks ([`Supervisor::restart`]);
//! - it swaps what the start produced (an [`Endpoint`](crate::loopback::Endpoint), say) together
//!   with the process, so no caller pairs an old port with a new token;
//! - it reports each of these as an [`Event`];
//! - [`Supervisor::stop`] brings it down for good — before an update installs, or as the app quits —
//!   and a start that was under way when it was called is undone, not left running.
//!
//! ```no_run
//! # #[cfg(feature = "loopback")]
//! # fn main() -> Result<(), tauri_kit_sidecar::loopback::StartError> {
//! use std::process::Command;
//! use std::time::Duration;
//! use tauri_kit_sidecar::loopback::{Loopback, LoopbackOptions};
//! use tauri_kit_sidecar::supervise::{Event, Started, SuperviseOptions, Supervisor};
//!
//! let options = LoopbackOptions::new("HELPER_TOKEN", "helper ready port=");
//! let helper = Supervisor::start(
//!     move || Loopback::start(Command::new("helper"), &options).map(Started::from),
//!     SuperviseOptions::default(),
//!     |event| {
//!         if let Event::Restarted { before, after, .. } = event {
//!             let moved = before.port() != after.port();
//!             // reload whatever pointed at the old address
//! #           let _ = moved;
//!         }
//!     },
//! )?;
//! let endpoint = helper.current(); // the running sidecar's address and token
//! # let _ = endpoint;
//! helper.stop(Duration::from_secs(5), |_endpoint| { /* ask it to quit first, if it can be asked */ });
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "loopback"))]
//! # fn main() {}
//! ```

use std::io;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::Sidecar;

/// One start of a sidecar: the process, and what the app needs from that start to reach it — an
/// endpoint, a port, or nothing (`()`).
pub struct Started<T> {
    /// The running process.
    pub sidecar: Sidecar,
    /// What this start produced. Replaced, together with the process, when the sidecar is started
    /// again.
    pub value: T,
}

/// How many times a sidecar that ended is started again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    /// Starts allowed within [`window`](RestartPolicy::window), failed ones included. Once they are
    /// used up the sidecar stays down. `0` never starts it again on its own.
    pub limit: u32,
    /// How far back starts are counted.
    pub window: Duration,
}

impl RestartPolicy {
    /// Never start it again on its own; the app calls [`Supervisor::restart`] when it wants to.
    pub const NEVER: RestartPolicy = RestartPolicy {
        limit: 0,
        window: Duration::ZERO,
    };
}

/// Three starts within five minutes.
impl Default for RestartPolicy {
    fn default() -> Self {
        RestartPolicy {
            limit: 3,
            window: Duration::from_secs(300),
        }
    }
}

/// Whether a running sidecar still answers: a probe the supervisor calls, with the value of the
/// running start, every so often. A sidecar that fails it `misses` times in a row is stopped and
/// handled as one that ended ([`Ended::Unresponsive`]).
///
/// The probe runs on the supervisor's thread without any of its locks held, so it may take its
/// time — but every check waits for it.
pub struct Health<T> {
    probe: Box<dyn Fn(&T) -> bool + Send + Sync>,
    every: Duration,
    misses: u32,
}

impl<T> Health<T> {
    /// Calls `probe` every `every`; `misses` failures in a row (at least one) count as unresponsive.
    pub fn new(
        every: Duration,
        misses: u32,
        probe: impl Fn(&T) -> bool + Send + Sync + 'static,
    ) -> Self {
        Health {
            probe: Box::new(probe),
            every,
            misses: misses.max(1),
        }
    }
}

/// How a [`Supervisor`] watches and restarts.
pub struct SuperviseOptions<T> {
    /// How often the process is checked, and how long to wait after a start that failed before the
    /// next one. Default one second.
    pub check_every: Duration,
    /// How many times it is started again. Default [`RestartPolicy::default`].
    pub restart: RestartPolicy,
    /// A probe for a sidecar that is running but may have stopped answering. Default none: only an
    /// ended process counts.
    pub health: Option<Health<T>>,
}

impl<T> Default for SuperviseOptions<T> {
    fn default() -> Self {
        SuperviseOptions {
            check_every: Duration::from_secs(1),
            restart: RestartPolicy::default(),
            health: None,
        }
    }
}

/// Why the supervisor stepped in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The process exited on its own.
    Exited(ExitStatus),
    /// It was running but failed the [`Health`] probe too many times in a row, and was stopped.
    Unresponsive,
}

/// What the supervisor did. Reported on its own thread, with none of its locks held — but from
/// inside the call that reports it, so a handler must not call [`Supervisor::restart`] or
/// [`Supervisor::stop`]; hand the work to another thread instead.
#[derive(Debug)]
pub enum Event<'a, T> {
    /// The sidecar ended and was started again. `attempt` counts the starts this ending took.
    Restarted {
        /// What ended.
        ended: Ended,
        /// The starts this ending took, this one included.
        attempt: u32,
        /// The value of the start that ended.
        before: &'a T,
        /// The value of the new start — what [`Supervisor::current`] now returns.
        after: &'a T,
    },
    /// A start after an ending failed. The supervisor tries again after
    /// [`check_every`](SuperviseOptions::check_every), while the policy allows.
    RestartFailed {
        /// What ended.
        ended: Ended,
        /// The starts this ending took, this one included.
        attempt: u32,
        /// Why the start failed.
        error: &'a str,
    },
    /// The sidecar ended and the policy allows no more starts: it stays down until
    /// [`Supervisor::restart`].
    Down {
        /// What ended.
        ended: Ended,
        /// The starts this ending took before the policy ran out.
        attempts: u32,
    },
}

/// Where a supervised sidecar is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Running, and watched.
    Running,
    /// It ended; the supervisor is starting it again.
    Restarting,
    /// It ended and will not be started again unless [`Supervisor::restart`] is called.
    Down,
    /// [`Supervisor::stop`] brought it down for good.
    Stopped,
}

type Launch<T> = Box<dyn Fn() -> Result<Started<T>, String> + Send + Sync>;
type Handler<T> = Box<dyn for<'a> FnMut(Event<'a, T>) + Send>;

struct Inner<T> {
    launch: Launch<T>,
    /// The process. Locked only to check on it or to swap it — never across a wait, a probe or a
    /// start, and whoever stops it takes it out and stops it outside the lock.
    sidecar: Mutex<Option<Sidecar>>,
    /// What the running start produced. Swapped under the `sidecar` lock, read on its own.
    value: Mutex<T>,
    state: Mutex<State>,
    /// One recovery at a time: an ending the watcher handles, or a [`Supervisor::restart`].
    cycle: Mutex<()>,
    stopped: AtomicBool,
    watching: AtomicBool,
    wake: (Mutex<()>, Condvar),
    handler: Mutex<Handler<T>>,
    check_every: Duration,
    restart: RestartPolicy,
    health: Option<Health<T>>,
}

/// A sidecar kept running by a function that starts it. See the [module documentation](self).
///
/// Dropping it stops the sidecar at once, as [`stop`](Supervisor::stop) with no grace would.
pub struct Supervisor<T: Clone + Send + 'static> {
    inner: Arc<Inner<T>>,
}

fn lock<V>(mutex: &Mutex<V>) -> MutexGuard<'_, V> {
    // A panic in an app's handler or probe must not take the supervisor down with it.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<T: Clone + Send + 'static> Supervisor<T> {
    /// Starts the sidecar with `launch` and watches it from a thread of its own. A first start that
    /// fails is returned as it is; later starts go through the same function, and their errors are
    /// reported as text in [`Event::RestartFailed`].
    pub fn start<E: std::fmt::Display>(
        launch: impl Fn() -> Result<Started<T>, E> + Send + Sync + 'static,
        options: SuperviseOptions<T>,
        on_event: impl for<'a> FnMut(Event<'a, T>) + Send + 'static,
    ) -> Result<Self, E> {
        let first = launch()?;
        let inner = Arc::new(Inner {
            launch: Box::new(move || launch().map_err(|e| e.to_string())),
            sidecar: Mutex::new(Some(first.sidecar)),
            value: Mutex::new(first.value),
            state: Mutex::new(State::Running),
            cycle: Mutex::new(()),
            stopped: AtomicBool::new(false),
            watching: AtomicBool::new(true),
            wake: (Mutex::new(()), Condvar::new()),
            handler: Mutex::new(Box::new(on_event)),
            check_every: options.check_every,
            restart: options.restart,
            health: options.health,
        });
        spawn_watcher(Arc::clone(&inner));
        Ok(Supervisor { inner })
    }

    /// The value of the running start — the endpoint to call, say. After a restart it is the new
    /// one; after the sidecar went down or was stopped it is the last one.
    pub fn current(&self) -> T {
        lock(&self.inner.value).clone()
    }

    /// Where the sidecar is.
    pub fn state(&self) -> State {
        *lock(&self.inner.state)
    }

    /// Stops the sidecar, waiting up to `grace` for it to exit on its own first, and starts it
    /// again — when the app wants a fresh one, or one that went [`Down`](State::Down) back. The
    /// restart policy's count starts over. Fails after [`stop`](Supervisor::stop), or when the
    /// start fails; the sidecar is then down.
    pub fn restart(&self, grace: Duration) -> Result<(), String> {
        let inner = &self.inner;
        let _cycle = lock(&inner.cycle);
        if inner.stopped.load(Ordering::SeqCst) {
            return Err("the sidecar was stopped".into());
        }
        let running = lock(&inner.sidecar).take();
        if let Some(sidecar) = running {
            let _ = sidecar.shutdown(grace);
        }
        inner.set_state(State::Restarting);
        match (inner.launch)() {
            Ok(next) => {
                if !inner.install(next) {
                    return Err("the sidecar was stopped".into());
                }
                inner.set_state(State::Running);
                if !inner.watching.swap(true, Ordering::SeqCst) {
                    spawn_watcher(Arc::clone(inner));
                }
                Ok(())
            }
            Err(error) => {
                inner.set_state(State::Down);
                Err(error)
            }
        }
    }

    /// Brings the sidecar down for good: nothing starts it again afterwards. `ask` is called first
    /// with the running start's value — for a sidecar that can be asked to quit (a shutdown request,
    /// say); then it has up to `grace` to exit before it and everything it started are stopped.
    ///
    /// Returns how the process ended, or `None` when there was none to stop — it had gone down, or
    /// this was called before. Does not wait for a start under way: that start is undone when it
    /// finishes.
    pub fn stop(&self, grace: Duration, ask: impl FnOnce(&T)) -> Option<io::Result<ExitStatus>> {
        let inner = &self.inner;
        if inner.stopped.swap(true, Ordering::SeqCst) {
            return None;
        }
        inner.wake.1.notify_all();
        *lock(&inner.state) = State::Stopped; // the one place it is set; nothing overwrites it
        let running = lock(&inner.sidecar).take()?;
        ask(&self.current());
        Some(running.shutdown(grace))
    }
}

impl<T: Clone + Send + 'static> Drop for Supervisor<T> {
    fn drop(&mut self) {
        self.stop(Duration::ZERO, |_| {});
    }
}

fn spawn_watcher<T: Clone + Send + 'static>(inner: Arc<Inner<T>>) {
    thread::spawn(move || inner.watch());
}

impl<T: Clone + Send + 'static> Inner<T> {
    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Moves to `state` — unless stopped, which is final: a start that was under way when
    /// [`Supervisor::stop`] was called must not report the sidecar running again.
    fn set_state(&self, state: State) {
        let mut current = lock(&self.state);
        if *current != State::Stopped {
            *current = state;
        }
    }

    /// Waits `for_` or until stopped; true when stopped.
    fn pause(&self, for_: Duration) -> bool {
        let guard = lock(&self.wake.0);
        let _ = self
            .wake
            .1
            .wait_timeout_while(guard, for_, |_| !self.stopped());
        self.stopped()
    }

    fn emit(&self, event: Event<'_, T>) {
        (lock(&self.handler))(event);
    }

    /// Puts a new start in place of the old one, process and value together — unless the
    /// supervisor was stopped meanwhile, in which case the new start is undone. Checked under the
    /// lock [`Supervisor::stop`] takes the process out under, so a stop cannot miss it.
    fn install(&self, next: Started<T>) -> bool {
        let mut slot = lock(&self.sidecar);
        if self.stopped() {
            drop(slot);
            drop(next.sidecar);
            return false;
        }
        let ended = slot.replace(next.sidecar);
        *lock(&self.value) = next.value;
        drop(slot);
        drop(ended);
        true
    }

    fn watch(&self) {
        let mut starts: Vec<Instant> = Vec::new();
        let mut misses = 0;
        let mut probed = Instant::now();
        loop {
            if self.pause(self.check_every) {
                return;
            }
            let ended = match self.check() {
                Some(ended) => Some(ended),
                None => self.probe(&mut probed, &mut misses),
            };
            if let Some(ended) = ended {
                misses = 0;
                if !self.recover(ended, &mut starts) {
                    return;
                }
                probed = Instant::now();
            }
        }
    }

    /// Whether the process has exited. Nothing to check while there is none — a restart has taken
    /// it out, or a stop.
    fn check(&self) -> Option<Ended> {
        let mut slot = lock(&self.sidecar);
        match slot.as_mut()?.try_status() {
            Ok(Some(status)) => Some(Ended::Exited(status)),
            _ => None,
        }
    }

    /// Calls the health probe when it is due; stops a sidecar that failed it too often.
    fn probe(&self, probed: &mut Instant, misses: &mut u32) -> Option<Ended> {
        let health = self.health.as_ref()?;
        if probed.elapsed() < health.every {
            return None;
        }
        *probed = Instant::now();
        let value = lock(&self.value).clone();
        if (health.probe)(&value) {
            *misses = 0;
            return None;
        }
        *misses += 1;
        if *misses < health.misses {
            return None;
        }
        let running = lock(&self.sidecar).take()?;
        let _ = running.shutdown(Duration::ZERO);
        Some(Ended::Unresponsive)
    }

    /// Starts the sidecar again after `ended`, within the policy. False when the watcher is done:
    /// the sidecar is down or the supervisor stopped.
    fn recover(&self, ended: Ended, starts: &mut Vec<Instant>) -> bool {
        let _cycle = lock(&self.cycle);
        // A restart may have replaced the ended process while this waited for the cycle.
        if matches!(ended, Ended::Exited(_)) && self.check().is_none() && !self.stopped() {
            return true;
        }
        let mut attempt = 0;
        loop {
            if self.stopped() {
                return false;
            }
            let now = Instant::now();
            starts.retain(|at| now.duration_since(*at) < self.restart.window);
            if starts.len() as u32 >= self.restart.limit {
                self.set_state(State::Down);
                // Cleared under the cycle lock, so a restart that follows sees it and watches again.
                self.watching.store(false, Ordering::SeqCst);
                self.emit(Event::Down {
                    ended,
                    attempts: attempt,
                });
                return false;
            }
            self.set_state(State::Restarting);
            starts.push(now);
            attempt += 1;
            match (self.launch)() {
                Ok(next) => {
                    let before = lock(&self.value).clone();
                    let after = next.value.clone();
                    if !self.install(next) {
                        return false;
                    }
                    self.set_state(State::Running);
                    self.emit(Event::Restarted {
                        ended,
                        attempt,
                        before: &before,
                        after: &after,
                    });
                    return true;
                }
                Err(error) => {
                    self.emit(Event::RestartFailed {
                        ended,
                        attempt,
                        error: &error,
                    });
                    if self.pause(self.check_every) {
                        return false;
                    }
                }
            }
        }
    }
}
