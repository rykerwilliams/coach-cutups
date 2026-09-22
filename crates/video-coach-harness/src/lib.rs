//! Headless integration tests driven over the command bus, with no window.
//!
//! [`Harness`] runs a real [`Bus`] with a system-memory video sink and
//! `fakesink sync=true` audio, and records every [`Event`] it emits. Tests wait
//! on events with a timeout — never on a sleep — and use
//! [`Harness::shutdown`] as a barrier when they need to assert that something
//! did **not** happen.

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, Instant};

use uuid::Uuid;
use video_coach_app::bus::{
    Bus, BusHandle, CaptureKind, Command, Event, ExportRun, RecordingStatus, Snapshot, StateFile,
    TranscriptionState, UserError,
};
use video_coach_core::project::{Clip, Project, SourceRef};
use video_coach_core::store;
use video_coach_media::{fixtures, frame_times, now_ns, probe, Frame, SinkKind, TranscribeKind};

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
    /// A bus whose last-project state file lives under `config_dir`, recording
    /// from test sources whose video starts at once.
    pub fn new(config_dir: &Path) -> Self {
        Self::with_capture(
            config_dir,
            CaptureKind::Test {
                video_delay: Duration::ZERO,
            },
        )
    }

    /// [`Harness::new`] recording from `capture`: test sources with a video
    /// delay, as a camera warming up.
    pub fn with_capture(config_dir: &Path, capture: CaptureKind) -> Self {
        // Nothing to say, at once: stopping a recording queues its clip
        // (Phase 10 spec S6), and a transcript nobody asked about must not
        // change the project under a test that isn't about one.
        Self::with_transcribe(
            config_dir,
            capture,
            TranscribeKind::Test {
                delay: Duration::ZERO,
                text: String::new(),
            },
        )
    }

    /// [`Harness::with_capture`] transcribing with `transcribe`: canned text
    /// after a delay, and never a model (spec S8).
    pub fn with_transcribe(
        config_dir: &Path,
        capture: CaptureKind,
        transcribe: TranscribeKind,
    ) -> Self {
        Self::spawn(config_dir, SinkKind::System, capture, transcribe)
    }

    /// A bus with the app's own sinks: the GL video sink on a surfaceless EGL
    /// display, so hardware decoders hand it DMABufs as they do in the app,
    /// and `autoaudiosink` — **real speakers**, so a test should turn the
    /// volume down. CI has neither, and runs it on Mesa's llvmpipe and
    /// `autoaudiosink`'s fake fallback: fine for small fixtures, while real
    /// footage stays in `#[ignore]`d tests.
    pub fn production(config_dir: &Path) -> Self {
        let h = Self::spawn(
            config_dir,
            SinkKind::Gl,
            CaptureKind::Test {
                video_delay: Duration::ZERO,
            },
            TranscribeKind::Test {
                delay: Duration::ZERO,
                text: String::new(),
            },
        );
        let (display, context) = surfaceless_gl().clone();
        h.send(Command::GlReady { display, context });
        h
    }

    fn spawn(
        config_dir: &Path,
        sinks: SinkKind,
        capture: CaptureKind,
        transcribe: TranscribeKind,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let bus = Bus::spawn(
            sinks,
            capture,
            transcribe,
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

    /// Play/pause as the UI sends it: the moment and position read now.
    pub fn toggle_play(&self) {
        self.send(Command::TogglePlay {
            host_ns: now_ns(),
            source_secs: self.position_secs(),
        });
    }

    /// A skip as the UI sends it, with the moment read now.
    pub fn skip(&self, delta: f64) {
        self.send(Command::Skip {
            delta,
            host_ns: now_ns(),
        });
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

    /// Waits for the next `Recording`.
    pub fn wait_recording(&mut self) -> RecordingStatus {
        self.wait_map("Recording", |e| match e {
            Event::Recording(s) => Some(*s),
            _ => None,
        })
    }

    /// Waits for the next `Export`: the whole run as it stood.
    pub fn wait_export(&mut self) -> ExportRun {
        self.wait_map("Export", |e| match e {
            Event::Export(run) => Some(run.clone()),
            _ => None,
        })
    }

    /// Waits for the next `Preview`: the clip now on screen, or `None` once
    /// it closed.
    pub fn wait_preview(&mut self) -> Option<Uuid> {
        self.wait_map("Preview", |e| match e {
            Event::Preview(previewing) => Some(*previewing),
            _ => None,
        })
    }

    /// Waits for the next `Transcription` that `f` accepts, skipping the ones
    /// before it: the state travels whole in every event, so a test waits for
    /// the state it means rather than counting events.
    pub fn wait_transcription(
        &mut self,
        what: &str,
        f: impl Fn(&TranscriptionState) -> bool,
    ) -> TranscriptionState {
        self.wait_map(what, |e| match e {
            Event::Transcription(state) => f(state).then(|| state.clone()),
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

    /// Waits for the next `Select`.
    pub fn wait_select(&mut self) -> Uuid {
        self.wait_map("Select", |e| match e {
            Event::Select(id) => Some(*id),
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

    /// Takes the recording's newest self-view frame, if one arrived since the
    /// last take, as the UI's redraw does.
    pub fn take_self_view(&self) -> Option<Frame> {
        self.bus.self_view().take()
    }

    /// Takes the scan player's newest frame, if one arrived since the last
    /// take, as the UI's redraw does. A test that takes it leaves the UI's
    /// slot empty, which only matters to a test drawing it too.
    pub fn take_frame(&self) -> Option<Frame> {
        self.bus.mailbox().take()
    }

    /// Sends `seek`, a command that moves the paused scan player, and waits
    /// for it to land: its target published, then settled, then its frame.
    /// Returns the position the player reports then, and the frame it put up.
    pub fn seek_and_settle(&mut self, seek: Command) -> (f64, Frame) {
        let what = format!("{seek:?}");
        self.take_frame();
        self.send(seek);
        self.wait_map(&format!("the target of {what}"), |e| {
            matches!(
                e,
                Event::Position {
                    target_abs: Some(_),
                    ..
                }
            )
            .then_some(())
        });
        self.wait_settled();
        let mut frame = None;
        self.poll_until(&format!("the frame {what} lands on"), |h| {
            frame = h.take_frame();
            frame.is_some()
        });
        let reported = self.position_secs().expect("a settled position");
        (reported, frame.expect("polled until some"))
    }

    /// Seconds into the previewed clip, as the UI's tick reads them (spec
    /// P3's one position path). Meaningless with no preview open: the UI
    /// reads it only while one is.
    pub fn preview_secs(&self) -> f64 {
        self.bus.preview_position().seconds()
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

/// The process's one surfaceless EGL display and context, standing in for the
/// UI's. One per process and never dropped, as `Gl::shared` explains:
/// finalizing any surfaceless display terminates them all.
fn surfaceless_gl() -> &'static (gstreamer_gl::GLDisplay, gstreamer_gl::GLContext) {
    use gstreamer_gl::prelude::*;
    static GL: OnceLock<(gstreamer_gl::GLDisplay, gstreamer_gl::GLContext)> = OnceLock::new();
    GL.get_or_init(|| {
        let display = gstreamer_gl_egl::GLDisplayEGL::new_surfaceless()
            .expect("a surfaceless EGL display")
            .upcast::<gstreamer_gl::GLDisplay>();
        let context = {
            let lock = display.object_lock();
            gstreamer_gl::GLDisplay::create_context(&lock, None::<&gstreamer_gl::GLContext>)
        }
        .expect("an EGL context");
        (display, context)
    })
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

/// Writes a project to `folder` whose one source is `video`, stored by its
/// absolute path, which `join` keeps as is. Returns what was written.
pub fn write_one_source_project(folder: &Path, video: &Path) -> Project {
    let p = probe(video).expect("probe the video");
    let mut project = Project::new("Game");
    project.source_videos.push(SourceRef {
        relative_path: video.to_string_lossy().into_owned(),
        display_name: "source".into(),
        duration_seconds: p.duration_seconds,
        display_aspect: p.display_aspect,
    });
    store::write(folder, &mut project).expect("write the one-source project");
    project
}

/// How far a scrub may land from its target: one frame at 30 fps. Fixed,
/// never the source's own rate, which reads 0/1 on an HLS remux.
pub const FRAME: f64 = 1.0 / 30.0;

/// How far apart two frame times may be and still name the same frame: the
/// decoder's `SLACK`, nanosecond rounding.
pub const SAME_FRAME: f64 = 1e-6;

/// Where one paused scrub landed ([`round_trip`]), in source seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Landing {
    pub target: f64,
    /// The player's position once the seek settled: what a tag made now
    /// would store.
    pub reported: f64,
    /// The frame the scan player put up, as [`Frame::stream_time`] and
    /// [`Frame::stream_end`]. Its start is clipped to the seek's target, so
    /// only its end says which frame it is.
    pub displayed: (Option<f64>, Option<f64>),
    /// The frame export shows for `reported`, start to end in stream time.
    pub export: Range<f64>,
}

impl Landing {
    /// Asserts the round trip (spec H6): the scan player displays the frame
    /// export picks for the position it reports, and that position is within
    /// a [`FRAME`] of the target.
    pub fn check(&self) {
        let end = self
            .displayed
            .1
            .unwrap_or_else(|| panic!("the displayed frame has no stream end: {self}"));
        assert!(
            (end - self.export.end).abs() <= SAME_FRAME,
            "the scan player shows a different frame than export picks: {self}"
        );
        assert!(
            (self.reported - self.target).abs() <= FRAME,
            "the scrub landed more than a frame off its target: {self}"
        );
    }
}

impl fmt::Display for Landing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let time = |t: Option<f64>| t.map_or_else(|| "none".to_owned(), |t| format!("{t:.4}"));
        write!(
            f,
            "target {:.4}, reported {:.4} ({:+.4}), displayed {}..{}, export {:.4}..{:.4}",
            self.target,
            self.reported,
            self.reported - self.target,
            time(self.displayed.0),
            time(self.displayed.1),
            self.export.start,
            self.export.end
        )
    }
}

/// Scrubs to each of `targets` while paused and records where it landed: the
/// position the player reports once the seek settles, and the stream time of
/// the frame it puts up. Then asks export's decoder, on `source`, which frame
/// it shows for each reported position. Prints every landing; asserts
/// nothing, so a caller can print a whole run before [`Landing::check`]ing it.
///
/// `h` has `source` open as its only source, paused, so a target is both
/// concat and source seconds.
pub fn round_trip(h: &mut Harness, source: &Path, targets: &[f64]) -> Vec<Landing> {
    let mut landed = Vec::new();
    for &target in targets {
        let (reported, frame) = h.seek_and_settle(Command::ScrubRelease { abs: target });
        landed.push((target, reported, (frame.stream_time, frame.stream_end)));
    }
    let reported: Vec<f64> = landed.iter().map(|&(_, r, _)| r).collect();
    let export = frame_times(source, &reported).expect("export's frame times");
    landed
        .into_iter()
        .zip(export)
        .map(|((target, reported, displayed), export)| {
            let landing = Landing {
                target,
                reported,
                displayed,
                export,
            };
            eprintln!("{landing}");
            landing
        })
        .collect()
}

/// A clip on `source_index`, starting 0.5 s in, with a fresh recording
/// filename and no file.
pub fn clip(source_index: usize) -> Clip {
    Clip {
        id: Uuid::new_v4(),
        name: format!("clip on {source_index}"),
        notes: String::new(),
        tags: Vec::new(),
        source_index,
        start_source_seconds: 0.5,
        recording_duration: 1.0,
        recording_filename: format!("{}.mkv", Uuid::new_v4()),
        events: Vec::new(),
        show_pip: true,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    }
}

/// Appends a [`clip`] on each of `source_indices` to `project`, each with a
/// small stand-in recording in `recordings/` holding its id, and saves it to
/// `folder`. Returns the new clips, in order.
pub fn add_clips(folder: &Path, project: &mut Project, source_indices: &[usize]) -> Vec<Clip> {
    let recordings = folder.join(store::RECORDINGS_DIRNAME);
    std::fs::create_dir_all(&recordings).expect("create recordings/");
    let added: Vec<Clip> = source_indices.iter().map(|&i| clip(i)).collect();
    for c in &added {
        std::fs::write(recordings.join(&c.recording_filename), c.id.to_string())
            .expect("write a stand-in recording");
        let mut c = c.clone();
        c.sort_index = project.clips.len() as i64;
        project.clips.push(c);
    }
    store::write(folder, project).expect("write the project with its clips");
    added
}

/// Makes a folder read-only until dropped, so a failing test still leaves a
/// temp dir that can be deleted.
pub struct ReadOnly(PathBuf);

impl ReadOnly {
    pub fn new(folder: &Path) -> Self {
        set_mode(folder, 0o555);
        ReadOnly(folder.to_owned())
    }

    /// Whether the kernel enforces it: not when running as root. A test that
    /// needs a failing write skips when this is false.
    pub fn enforced(&self) -> bool {
        let probe = self.0.join(".read-only-probe");
        let written = std::fs::write(&probe, b"").is_ok();
        let _ = std::fs::remove_file(&probe);
        !written
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        set_mode(&self.0, 0o755);
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
