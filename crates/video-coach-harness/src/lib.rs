//! Headless integration tests driven over the command bus, with no window.
//!
//! [`Harness`] runs a real [`Bus`] with a system-memory video sink and
//! `fakesink sync=true` audio, and records every [`Event`] it emits. Tests wait
//! on events with a timeout — never on a sleep — and use
//! [`Harness::shutdown`] as a barrier when they need to assert that something
//! did **not** happen.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use video_coach_app::bus::{Bus, BusHandle, Command, Event, StateFile};
use video_coach_media::SinkKind;

/// Generous: waits normally finish in milliseconds.
pub const TIMEOUT: Duration = Duration::from_secs(15);

pub struct Harness {
    bus: BusHandle,
    rx: mpsc::Receiver<Event>,
    log: Vec<Event>,
    /// Events before this index have been consumed by `wait_for`.
    cursor: usize,
}

impl Harness {
    /// A bus whose last-project state file lives under `config_dir`.
    pub fn new(config_dir: &Path) -> Self {
        let (tx, rx) = mpsc::channel();
        let bus = Bus::spawn_with_state(
            SinkKind::System,
            StateFile::in_config_dir(config_dir),
            Box::new(move |event| {
                let _ = tx.send(event);
            }),
        );
        Harness {
            bus,
            rx,
            log: Vec::new(),
            cursor: 0,
        }
    }

    pub fn send(&self, cmd: Command) {
        self.bus.send(cmd);
    }

    /// Waits until an unconsumed event matches `pred`, and consumes every
    /// event up to and including it. Panics after [`TIMEOUT`].
    pub fn wait_for(&mut self, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(i) = self.log[self.cursor..].iter().position(&pred) {
                let event = self.log[self.cursor + i].clone();
                self.cursor += i + 1;
                return event;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(event) => self.log.push(event),
                Err(_) => panic!(
                    "timed out waiting for {what}; unconsumed events: {:#?}",
                    &self.log[self.cursor..]
                ),
            }
        }
    }

    /// Waits until `cond` holds, receiving events meanwhile, and polling it
    /// at least every 10 ms. For state the bus doesn't announce, such as the
    /// playback position. Panics after [`TIMEOUT`].
    pub fn poll_until(&mut self, what: &str, mut cond: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !cond(self) {
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {what}; unconsumed events: {:#?}",
                    &self.log[self.cursor..]
                );
            }
            if let Ok(event) = self.rx.recv_timeout(Duration::from_millis(10)) {
                self.log.push(event);
            }
        }
    }

    /// Every event received so far, consumed or not.
    pub fn log(&self) -> &[Event] {
        &self.log
    }

    /// The pipeline's position in its current source, in seconds.
    pub fn position_secs(&self) -> Option<f64> {
        self.bus.position_handle().query_position()
    }

    /// Waits for the next `Error` event and returns its payload.
    pub fn wait_for_error(&mut self) -> video_coach_app::bus::UserError {
        match self.wait_for("an error", |e| matches!(e, Event::Error(_))) {
            Event::Error(e) => e,
            _ => unreachable!(),
        }
    }

    /// Waits until no seek is outstanding: a `Position` with no target.
    pub fn wait_settled(&mut self) -> usize {
        match self.wait_for("a settled position", |e| {
            matches!(
                e,
                Event::Position {
                    target_abs: None,
                    ..
                }
            )
        }) {
            Event::Position { source_index, .. } => source_index,
            _ => unreachable!(),
        }
    }

    /// Shuts the bus down, which handles every command sent before it, and
    /// returns the events not yet consumed. Use it as a barrier to assert that
    /// something did not happen.
    pub fn shutdown(mut self) -> Vec<Event> {
        self.bus.shutdown();
        self.log.extend(self.rx.try_iter());
        self.log.split_off(self.cursor)
    }
}
