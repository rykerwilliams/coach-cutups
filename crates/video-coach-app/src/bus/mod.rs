//! The command bus (spec D5): one thread that owns the [`Project`], its folder,
//! and the [`SourcePlayer`]. The UI talks to it only through [`Command`]s and
//! hears back only through [`Event`]s, so everything here runs headless — the
//! harness drives it with a system-memory sink and no Slint.
//!
//! One input channel carries commands, the player's forwarded GStreamer
//! messages and the recorder's messages, so the thread never has to choose
//! between queues. The loop waits with `recv_timeout` on the earlier of two
//! deadlines (the skip debounce and the recording start timeout); with
//! neither armed it simply blocks.

mod project;
mod recording;
mod sources;
mod state;
mod transport;

use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Instant;

use gstreamer as gst;
use gstreamer_gl as gst_gl;
use video_coach_core::project::{AspectMismatch, Project, SourceReferenced};
use video_coach_core::skip::SkipCoordinator;
use video_coach_core::store::StoreError;
use video_coach_core::zoom::Zoom;
use video_coach_media::{
    FrameMailbox, PositionHandle, ProbeError, RecorderMessage, SinkKind, SourcePlayer,
};

pub use recording::{CaptureKind, RecordingStatus};
pub use state::StateFile;

/// What the UI asks the bus to do.
#[derive(Debug)]
pub enum Command {
    // Project.
    /// Open the project in a folder, creating one if it has no `project.json`.
    OpenProject(PathBuf),
    /// Reopen the last successfully opened project, if it still exists. Never
    /// creates one.
    RestoreLastProject,
    RenameProject(String),

    // Sources.
    AddSource(PathBuf),
    RemoveSource(usize),
    MoveSource {
        from: usize,
        to: usize,
    },
    RelinkSource(usize, PathBuf),

    // Transport. Positions are concat-timeline seconds unless named `source_`.
    // `host_ns` is `now_ns()` at the input event, captured by the caller
    // (never by the bus: queue delay would drift the recording's log).
    /// `source_secs` is the player's position the UI read at the keypress,
    /// the pause or play anchor when no seek is outstanding (R10).
    TogglePlay {
        host_ns: u64,
        source_secs: Option<f64>,
    },
    /// Skip by `delta` seconds; presses in quick succession accumulate.
    Skip {
        delta: f64,
        host_ns: u64,
    },
    /// Live scrub preview: a keyframe seek, latest wins.
    ScrubMove {
        abs: f64,
    },
    /// Scrub release: one frame-accurate seek.
    ScrubRelease {
        abs: f64,
    },
    /// Linear slider value in `0..=1`. Persisted to `scan_volume` only when
    /// `commit` is set (on slider release).
    SetVolume {
        value: f64,
        commit: bool,
    },

    // Recording (spec R6).
    /// R: while idle, starts a recording from where the player is heading,
    /// with `zoom` (the UI's) as the log's first event; while recording,
    /// [`Command::StopRecording`].
    ToggleRecording {
        zoom: Zoom,
    },
    /// Stops the recording, or aborts it if no video has arrived yet.
    StopRecording,
    /// The UI's zoom changed. Logged while recording, ignored otherwise.
    Zoom {
        host_ns: u64,
        zoom: Zoom,
    },
    /// The project's preferred camera, by PipeWire `node.name`; `None` is
    /// the system default.
    SetCamera(Option<String>),
    /// The project's preferred microphone, likewise.
    SetMic(Option<String>),

    // Lifecycle.
    /// The UI's wrapped GL display and context (spec D3). Until it arrives, a
    /// GL-sink player stays in NULL.
    GlReady {
        display: gst_gl::GLDisplay,
        context: gst_gl::GLContext,
    },
    /// Take the pipeline to NULL, send on `ack`, and exit. Also serves as the
    /// UI's `RenderingTeardown`: GStreamer must stop using the GL context
    /// before the UI destroys it.
    Shutdown {
        ack: mpsc::Sender<()>,
    },
}

/// The open project as the UI sees it.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub project: Arc<Project>,
    /// One entry per source: `true` if its file doesn't exist. Re-checked
    /// on open, after every source-list change and after a player error.
    pub missing: Arc<[bool]>,
}

/// What the bus tells the UI.
#[derive(Debug, Clone)]
pub enum Event {
    /// A project was opened (or created). The UI resets zoom on it (D9).
    ProjectOpened(Snapshot),
    /// The open project changed (and a save was attempted), or which of its
    /// sources are missing did.
    ProjectChanged(Snapshot),
    /// The source the player holds or is heading to, and while a seek is
    /// outstanding its target in concat seconds. A target is published
    /// **before** its request is issued, so the readout never combines a new
    /// source's position with the old index; the settled position (no
    /// target) once the player is idle. Never repeated unchanged.
    Position {
        source_index: usize,
        target_abs: Option<f64>,
    },
    Playing(bool),
    Recording(RecordingStatus),
    /// The microphone's loudest channel peak over the last 100 ms, in dB,
    /// while a recording runs.
    Level(f64),
    /// A failure, or a notice (see [`UserError::is_notice`]).
    Error(UserError),
}

/// A failure the user is told about. The `Display` text is the message.
#[derive(thiserror::Error, Debug, Clone, PartialEq)]
pub enum UserError {
    #[error(
        "the video's shape ({attempted:.3}:1) doesn't match the project's other videos \
         ({existing:.3}:1)"
    )]
    AspectMismatch { existing: f64, attempted: f64 },
    /// A chosen source file was refused by the probe.
    #[error(transparent)]
    Source(#[from] ProbeError),
    /// The player failed on a source it had accepted, e.g. one changed on
    /// disk since. Play reloads it.
    #[error("playback failed: {0}")]
    Playback(String),
    #[error("the project file is unreadable: {0}")]
    UnreadableProject(String),
    #[error(
        "this project was created by the macOS version of Coach Cuts (format v{found}) \
         and cannot be opened"
    )]
    LegacyProject { found: u32 },
    #[error("this project was made by a newer version of Coach Cuts (format v{found})")]
    TooNewProject { found: u32 },
    #[error("that video is still used by a clip or match event; delete those first")]
    SourceReferenced { index: usize },
    /// Recording is refused: the project isn't ready for it.
    #[error("can't record: {0}")]
    CantRecord(&'static str),
    /// Recording is refused: no camera meets R3's rule.
    #[error("no camera with a 16:9, 30 fps mode up to 1280 wide was found")]
    NoCamera,
    #[error("recording failed: {0}")]
    RecordingFailed(String),
    /// A notice: the chosen device is absent, and the recording goes ahead on
    /// the default one. The preference is kept.
    #[error("the chosen {what} isn't connected, so the default one is recording")]
    DeviceFallback { what: &'static str },
    /// A notice: the recording didn't finalize cleanly. The clip was kept.
    #[error(
        "the recording didn't finish cleanly; its clip was kept, but its end may be cut short"
    )]
    StopNotClean,
    #[error("{0}")]
    Io(String),
}

impl UserError {
    /// Something the user should know that needs no answer: the UI shows it
    /// without blocking, since it can arrive mid-recording.
    pub fn is_notice(&self) -> bool {
        matches!(
            self,
            UserError::DeviceFallback { .. } | UserError::StopNotClean
        )
    }
}

impl From<StoreError> for UserError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::LegacyProject { found, .. } => UserError::LegacyProject { found },
            StoreError::TooNew { found, .. } => UserError::TooNewProject { found },
            StoreError::Malformed(msg) => UserError::UnreadableProject(msg),
            // Only reachable if a caller maps it rather than creating a
            // project; report it as the read failure it is.
            e @ StoreError::MissingProjectJson(_) => UserError::UnreadableProject(e.to_string()),
            StoreError::NotSerializable(msg) => UserError::Io(msg),
            StoreError::Io(e) => UserError::Io(e.to_string()),
        }
    }
}

impl From<AspectMismatch> for UserError {
    fn from(e: AspectMismatch) -> Self {
        UserError::AspectMismatch {
            existing: e.existing,
            attempted: e.attempted,
        }
    }
}

impl From<SourceReferenced> for UserError {
    fn from(e: SourceReferenced) -> Self {
        UserError::SourceReferenced { index: e.index }
    }
}

/// Everything that wakes the bus thread.
enum Input {
    Cmd(Command),
    Gst(gst::Message),
    /// From the recorder with this generation. Never routed to the player.
    Recorder(u64, RecorderMessage),
}

/// The project the bus has open: folder and document, committed together.
struct Open {
    /// Absolute and canonical, so source paths resolve against it directly.
    folder: PathBuf,
    project: Project,
}

/// The bus thread's state. Constructed and owned by [`Bus::spawn`]; the UI
/// only ever holds a [`BusHandle`].
pub struct Bus {
    events: Box<dyn Fn(Event) + Send>,
    /// The bus's own input, for the recorder's messages.
    tx: mpsc::Sender<Input>,
    player: SourcePlayer,
    position: PositionHandle,
    state: StateFile,
    open: Option<Open>,
    /// Index of the latest request's source: the one the player holds, or is
    /// heading to. Whether it actually holds it, and where it's heading, are
    /// the player's to say (`SourcePlayer::holds`, `target_secs`).
    current: usize,
    /// The last `Position` published: source index and target.
    last_position: (usize, Option<f64>),
    playing: bool,
    /// One entry per source, from the last existence check.
    missing: Arc<[bool]>,
    /// Coalesces skip presses over concat time (spec D8).
    skip: SkipCoordinator,
    /// When the skip debounce fires, if armed.
    skip_deadline: Option<Instant>,
    /// When the skip burst's target was last where playback would be: its
    /// leading press, or play starting. See `Bus::apply_skip`.
    skip_since: Instant,
    /// Where recordings come from.
    capture: CaptureKind,
    /// The recording in progress.
    recording: Option<recording::Active>,
    /// The latest recorder's generation. Messages from any other are stale.
    generation: u64,
}

impl Bus {
    /// Starts the bus thread with the player's video sink built from `sinks`,
    /// and the last-project state file in the user's config directory.
    ///
    /// `sinks` picks the audio sink too: `Gl` is production, with
    /// `autoaudiosink`; `System` is headless tests, with `fakesink sync=true`,
    /// so playback still runs in real time without a sound device.
    ///
    /// `capture` picks where recordings come from: the camera and microphone,
    /// or test sources.
    ///
    /// `events` is called on the bus thread.
    pub fn spawn(
        sinks: SinkKind,
        capture: CaptureKind,
        events: Box<dyn Fn(Event) + Send>,
    ) -> BusHandle {
        Self::spawn_with_state(sinks, capture, StateFile::default_location(), events)
    }

    /// [`Bus::spawn`] with an explicit state file, for tests.
    pub fn spawn_with_state(
        sinks: SinkKind,
        capture: CaptureKind,
        state: StateFile,
        events: Box<dyn Fn(Event) + Send>,
    ) -> BusHandle {
        gst::init().expect("GStreamer failed to initialize");
        let (tx, rx) = mpsc::channel();
        let player = SourcePlayer::new(sinks, {
            let tx = tx.clone();
            move |msg| {
                // Fails only once the bus thread has exited.
                let _ = tx.send(Input::Gst(msg));
            }
        });
        let position = player.position_handle();
        let mailbox = player.mailbox().clone();
        let bus = Bus {
            events,
            tx: tx.clone(),
            player,
            position: position.clone(),
            state,
            open: None,
            current: 0,
            last_position: (0, None),
            playing: false,
            missing: Arc::new([]),
            skip: SkipCoordinator::default(),
            skip_deadline: None,
            skip_since: Instant::now(),
            capture,
            recording: None,
            generation: 0,
        };
        let thread = std::thread::Builder::new()
            .name("bus".into())
            .spawn(move || bus.run(rx))
            .expect("spawn the bus thread");
        BusHandle {
            tx,
            thread: Some(thread),
            mailbox,
            position,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Input>) {
        loop {
            let deadline = self
                .skip_deadline
                .into_iter()
                .chain(self.start_deadline())
                .min();
            let input = match deadline {
                Some(deadline) => {
                    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                        Ok(input) => Some(input),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                None => match rx.recv() {
                    Ok(input) => Some(input),
                    Err(mpsc::RecvError) => return,
                },
            };
            match input {
                None => {}
                Some(Input::Gst(msg)) => {
                    let events = self.player.handle(&msg);
                    self.player_events(events);
                }
                Some(Input::Recorder(generation, msg)) => self.recorder_message(generation, msg),
                Some(Input::Cmd(Command::Shutdown { ack })) => {
                    // A recording keeps its clip (or is aborted while still
                    // starting) before anything is torn down.
                    self.stop_recording();
                    // Dropping the player takes the pipeline to NULL before
                    // the ack, which the UI's GL teardown waits for.
                    drop(self);
                    let _ = ack.send(());
                    return;
                }
                Some(Input::Cmd(cmd)) => self.command(cmd),
            }
            // After every input, not only on a timeout, so a busy channel
            // (level messages at 10 Hz) can't starve them.
            self.dispatch_deadlines();
            self.publish_position();
        }
    }

    /// Runs each deadline that has passed.
    fn dispatch_deadlines(&mut self) {
        let now = Instant::now();
        if self.skip_deadline.is_some_and(|d| d <= now) {
            self.skip_deadline = None;
            self.skip_debounce_passed();
        }
        if self.start_deadline().is_some_and(|d| d <= now) {
            self.start_timed_out();
        }
    }

    fn command(&mut self, cmd: Command) {
        // The one guard while recording (R6): everything not listed is
        // refused, so commands added later are too. The UI greys these out,
        // so reaching here is a UI bug.
        if self.recording.is_some()
            && !matches!(
                cmd,
                Command::TogglePlay { .. }
                    | Command::Skip { .. }
                    | Command::SetVolume { .. }
                    | Command::Zoom { .. }
                    | Command::ToggleRecording { .. }
                    | Command::StopRecording
                    | Command::GlReady { .. }
            )
        {
            return eprintln!("bus: refused while recording: {cmd:?}");
        }
        match cmd {
            Command::OpenProject(folder) => self.open_project(folder),
            Command::RestoreLastProject => self.restore_last_project(),
            Command::RenameProject(name) => self.rename_project(name),
            Command::AddSource(path) => self.add_source(path),
            Command::RemoveSource(index) => self.remove_source(index),
            Command::MoveSource { from, to } => self.move_source(from, to),
            Command::RelinkSource(index, path) => self.relink_source(index, path),
            Command::TogglePlay {
                host_ns,
                source_secs,
            } => self.toggle_play(host_ns, source_secs),
            Command::Skip { delta, host_ns } => self.skip(delta, host_ns),
            Command::ScrubMove { abs } => self.scrub(abs, false),
            Command::ScrubRelease { abs } => self.scrub(abs, true),
            Command::SetVolume { value, commit } => self.set_volume(value, commit),
            Command::ToggleRecording { zoom } => self.toggle_recording(zoom),
            Command::StopRecording => self.stop_recording(),
            Command::Zoom { host_ns, zoom } => self.log_zoom(host_ns, zoom),
            Command::SetCamera(camera) => self.set_camera(camera),
            Command::SetMic(mic) => self.set_mic(mic),
            Command::GlReady { display, context } => {
                let events = self.player.set_gl_context(display, context);
                self.player_events(events);
            }
            Command::Shutdown { .. } => unreachable!("handled in run"),
        }
    }

    fn emit(&self, event: Event) {
        (self.events)(event);
    }
}

/// The UI's side of the bus. Dropping it shuts the bus down and waits for it.
pub struct BusHandle {
    tx: mpsc::Sender<Input>,
    thread: Option<JoinHandle<()>>,
    mailbox: FrameMailbox,
    position: PositionHandle,
}

impl BusHandle {
    /// Queues a command. Silently dropped once the bus has shut down.
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(Input::Cmd(cmd));
    }

    /// Where the player's video sink delivers frames.
    pub fn mailbox(&self) -> &FrameMailbox {
        &self.mailbox
    }

    /// Position queries on the running pipeline — the one direct pipeline
    /// access permitted outside the bus thread (D5).
    pub fn position_handle(&self) -> &PositionHandle {
        &self.position
    }

    /// Sends [`Command::Shutdown`], waits for the ack and joins the thread.
    /// Every command sent before it has been handled, and every event it
    /// produced delivered, by the time this returns. Idempotent.
    pub fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        let (ack, acked) = mpsc::channel();
        self.send(Command::Shutdown { ack });
        // Errs if the thread already exited (e.g. a Shutdown sent by hand).
        let _ = acked.recv();
        let _ = thread.join();
    }
}

impl Drop for BusHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
