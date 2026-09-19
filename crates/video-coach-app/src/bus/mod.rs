//! The command bus (spec D5): one thread that owns the [`Project`], its folder,
//! and the [`SourcePlayer`]. The UI talks to it only through [`Command`]s and
//! hears back only through [`Event`]s, so everything here runs headless — the
//! harness drives it with a system-memory sink and no Slint.
//!
//! One input channel carries both commands and the player's forwarded
//! GStreamer messages, so the thread never has to choose between two queues.
//! The loop waits with `recv_timeout` on an optional deadline (the skip
//! debounce); with no deadline it simply blocks.

mod project;
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
use video_coach_media::{FrameMailbox, PositionHandle, ProbeError, SinkKind, SourcePlayer};

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

    // Transport. Positions are concat-timeline seconds.
    TogglePlay,
    /// Skip by `delta` seconds; presses in quick succession accumulate.
    Skip {
        delta: f64,
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
    #[error("{0}")]
    Io(String),
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
    deadline: Option<Instant>,
}

impl Bus {
    /// Starts the bus thread with the player's video sink built from `sinks`,
    /// and the last-project state file in the user's config directory.
    ///
    /// `sinks` picks the audio sink too: `Gl` is production, with
    /// `autoaudiosink`; `System` is headless tests, with `fakesink sync=true`,
    /// so playback still runs in real time without a sound device.
    ///
    /// `events` is called on the bus thread.
    pub fn spawn(sinks: SinkKind, events: Box<dyn Fn(Event) + Send>) -> BusHandle {
        Self::spawn_with_state(sinks, StateFile::default_location(), events)
    }

    /// [`Bus::spawn`] with an explicit state file, for tests.
    pub fn spawn_with_state(
        sinks: SinkKind,
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
            player,
            position: position.clone(),
            state,
            open: None,
            current: 0,
            last_position: (0, None),
            playing: false,
            missing: Arc::new([]),
            skip: SkipCoordinator::default(),
            deadline: None,
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
            let input = match self.deadline {
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
                None => {
                    self.deadline = None;
                    self.deadline_passed();
                }
                Some(Input::Gst(msg)) => {
                    let events = self.player.handle(&msg);
                    self.player_events(events);
                }
                Some(Input::Cmd(Command::Shutdown { ack })) => {
                    // Dropping the player takes the pipeline to NULL before
                    // the ack, which the UI's GL teardown waits for.
                    drop(self);
                    let _ = ack.send(());
                    return;
                }
                Some(Input::Cmd(cmd)) => self.command(cmd),
            }
            self.publish_position();
        }
    }

    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::OpenProject(folder) => self.open_project(folder),
            Command::RestoreLastProject => self.restore_last_project(),
            Command::RenameProject(name) => self.rename_project(name),
            Command::AddSource(path) => self.add_source(path),
            Command::RemoveSource(index) => self.remove_source(index),
            Command::MoveSource { from, to } => self.move_source(from, to),
            Command::RelinkSource(index, path) => self.relink_source(index, path),
            Command::TogglePlay => self.toggle_play(),
            Command::Skip { delta } => self.skip(delta),
            Command::ScrubMove { abs } => self.scrub(abs, false),
            Command::ScrubRelease { abs } => self.scrub(abs, true),
            Command::SetVolume { value, commit } => self.set_volume(value, commit),
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
