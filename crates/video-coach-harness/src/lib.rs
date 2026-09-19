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

use video_coach_app::bus::{Bus, BusHandle, Command, Event, Snapshot, StateFile, UserError};
use video_coach_core::project::{Project, SourceRef};
use video_coach_core::store;
use video_coach_media::{fixtures, probe, SinkKind};

/// Generous: waits normally finish in milliseconds.
pub const TIMEOUT: Duration = Duration::from_secs(15);

pub struct Harness {
    bus: BusHandle,
    rx: mpsc::Receiver<Event>,
    log: Vec<Event>,
    /// Events before this index have been consumed by `wait_map`.
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

    /// Waits until `f` maps an unconsumed event to `Some`, consumes every
    /// event up to and including it, and returns the mapped value. Panics
    /// after [`TIMEOUT`].
    pub fn wait_map<T>(&mut self, what: &str, f: impl Fn(&Event) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let found = self.log[self.cursor..]
                .iter()
                .enumerate()
                .find_map(|(i, e)| f(e).map(|t| (i, t)));
            if let Some((i, t)) = found {
                self.cursor += i + 1;
                return t;
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

    /// Waits for the next `ProjectOpened`.
    pub fn wait_opened(&mut self) -> Snapshot {
        self.wait_map("ProjectOpened", |e| match e {
            Event::ProjectOpened(s) => Some(s.clone()),
            _ => None,
        })
    }

    /// Waits for the next `ProjectChanged`.
    pub fn wait_changed(&mut self) -> Snapshot {
        self.wait_map("ProjectChanged", |e| match e {
            Event::ProjectChanged(s) => Some(s.clone()),
            _ => None,
        })
    }

    /// Waits for the next `Position`: source index and target.
    pub fn wait_position(&mut self) -> (usize, Option<f64>) {
        self.wait_map("Position", |e| match e {
            Event::Position {
                source_index,
                target_abs,
            } => Some((*source_index, *target_abs)),
            _ => None,
        })
    }

    /// Waits for the next `Playing`.
    pub fn wait_playing(&mut self) -> bool {
        self.wait_map("Playing", |e| match e {
            Event::Playing(p) => Some(*p),
            _ => None,
        })
    }

    /// Waits for the next `Error` event and returns its payload.
    pub fn wait_for_error(&mut self) -> UserError {
        self.wait_map("an error", |e| match e {
            Event::Error(e) => Some(e.clone()),
            _ => None,
        })
    }

    /// Waits until no seek is outstanding: a `Position` with no target.
    /// Returns its source index.
    pub fn wait_settled(&mut self) -> usize {
        self.wait_map("a settled position", |e| match e {
            Event::Position {
                source_index,
                target_abs: None,
            } => Some(*source_index),
            _ => None,
        })
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

    /// Shuts the bus down, which handles every command sent before it, and
    /// returns the events not yet consumed. Use it as a barrier to assert that
    /// something did not happen.
    pub fn shutdown(mut self) -> Vec<Event> {
        self.bus.shutdown();
        self.log.extend(self.rx.try_iter());
        self.log.split_off(self.cursor)
    }
}

/// Writes a project to `folder` whose sources are 16:9 30 fps WebM fixtures
/// of the given names and lengths in seconds, created in `media`, which must
/// be a sibling of `folder` (the stored paths are `../<media>/<name>`).
/// Returns what was written.
pub fn write_project(folder: &Path, media: &Path, videos: &[(&str, u32)]) -> Project {
    let media_name = media
        .file_name()
        .expect("media is a named folder")
        .to_string_lossy();
    let mut project = Project::new("Game");
    for &(name, secs) in videos {
        let path = fixtures::webm(media, name, secs, 320, 180, 30, 15);
        let p = probe(&path).expect("probe a fixture");
        project.source_videos.push(SourceRef {
            relative_path: format!("../{media_name}/{name}"),
            display_name: name.into(),
            duration_seconds: p.duration_seconds,
            display_aspect: p.display_aspect,
        });
    }
    store::write(folder, &mut project).expect("write the fixture project");
    project
}
