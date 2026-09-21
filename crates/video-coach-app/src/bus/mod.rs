//! The command bus (spec D5): one thread that owns the [`Project`], its folder,
//! and the [`SourcePlayer`]. The UI talks to it only through [`Command`]s and
//! hears back only through [`Event`]s, so everything here runs headless — the
//! harness drives it with a system-memory sink and no Slint.
//!
//! One input channel carries commands, the player's forwarded GStreamer
//! messages, and the recorder's and the exporter's messages, so the thread
//! never has to choose between queues. The loop waits with `recv_timeout` on
//! the earlier of two deadlines (the skip debounce and the recording start
//! timeout); with neither armed it simply blocks.

mod clips;
mod export;
mod preview;
mod project;
mod recording;
mod scoreboard;
mod sources;
mod state;
mod transcribe;
mod transport;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Instant;

use gstreamer as gst;
use gstreamer_gl as gst_gl;
use uuid::Uuid;
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{AspectMismatch, Project, Quality, Resolution, SourceReferenced};
use video_coach_core::scoreboard::{MatchEventKind, ScoreboardConfig};
use video_coach_core::skip::SkipCoordinator;
use video_coach_core::store::StoreError;
use video_coach_core::stroke::Stroke;
use video_coach_core::undo::{ClipEdit, UndoController};
use video_coach_core::zoom::Zoom;
use video_coach_media::{
    ExportMessage, FrameMailbox, Gl, PositionHandle, PreviewMessage, PreviewPosition, ProbeError,
    RecorderMessage, SinkKind, SourcePlayer, TranscribeKind, TranscribeMessage, WhisperModel,
};

pub use export::{export_targets, ExportRun, ExportTargetRow, ExportTargetRun, TargetState};
pub use recording::{CaptureKind, RecordingStatus};
pub use state::StateFile;
pub use transcribe::{whisper, whisper_model_override, Finish, Stage, TranscriptionState};

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

    // Clips (Phase 3 spec C5). Each mutation is one undo step, and none is
    // one if it changes nothing.
    /// Set one field of a clip. Tags arrive normalized.
    EditClip {
        id: Uuid,
        edit: ClipEdit,
    },
    /// Move the clip to `.trash`, undoably.
    DeleteClip(Uuid),
    /// Move the clip at list position `from` to `to` (the `Vec::remove` +
    /// `Vec::insert` convention).
    MoveClip {
        from: usize,
        to: usize,
    },
    /// Order the clips by source, then start.
    SortClipsBySource,
    /// Pause the game video at the clip's start.
    JumpToClip(Uuid),
    Undo,
    Redo,

    // Match events (Phase 9 spec S5). Tagging and deleting are undo steps of
    // their own; the setup is not.
    /// Tag a goal or a start/stop where the game video is. The position is
    /// the readout's at the keypress, captured by the caller like `host_ns`
    /// (never by the bus: queue delay would move where the event landed).
    TagMatchEvent {
        kind: MatchEventKind,
        source_index: usize,
        source_seconds: f64,
    },
    DeleteMatchEvent(Uuid),
    /// The teams, their colours, the match format and the back-anchor flag,
    /// from the setup sheet. A team without a name is refused.
    SetScoreboard(ScoreboardConfig),

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
    /// A drawing finished (Phase 6 spec D4). `host_ns` is the moment of its
    /// **last** point, the pen-up. Logged while recording, ignored otherwise.
    Stroke {
        host_ns: u64,
        stroke: Stroke,
    },
    /// The coach wiped every drawing. Logged while recording, ignored
    /// otherwise.
    ClearAll {
        host_ns: u64,
    },
    /// The project's preferred camera, by PipeWire `node.name`; `None` is
    /// the system default.
    SetCamera(Option<String>),
    /// The project's preferred microphone, likewise.
    SetMic(Option<String>),

    // Export (Phase 5 spec X4, Phase 8 spec E1).
    /// Render each target into `<project>/exports/`, one after another, in
    /// the background. `resolution` and `quality` are the sheet's pickers,
    /// and become the project's (spec E4). Refused while another run is
    /// going; dropped while recording.
    Export {
        targets: Vec<ExportTarget>,
        resolution: Resolution,
        quality: Quality,
    },
    /// Stop the running export, if any. Its outcome still arrives as the
    /// run's own: a cancel too late to stop a target reports it done.
    CancelExport,

    // Transcription (Phase 10 spec S5, S6).
    /// Queue the clip's commentary for transcription, behind whatever is
    /// already running. Does nothing if it is queued or running already;
    /// re-running a clip that has a transcript overwrites it, as the coach
    /// asked. Refused while recording, like every other edit.
    Transcribe {
        clip_id: Uuid,
    },
    /// Stop the transcription running **and drop the queue behind it**. The
    /// clip goes back to idle, not to a failure; one that had already
    /// finished keeps its words.
    CancelTranscription,
    /// Which speech model to run, from the inspector's picker. Remembered for
    /// every project on this machine (`state.json`), since it describes how
    /// fast the machine is and not the match. **The job running keeps the
    /// model it started with**; everything queued picks this one up.
    SetTranscribeModel(WhisperModel),

    // Preview (Phase 7 spec P5).
    /// Show the clip's composite -- its source edited by the coach's plays,
    /// zoomed as they zoomed, with the webcam inset, the drawings and the
    /// commentary -- in place of the game video, which pauses.
    OpenPreview(Uuid),
    /// Close the preview and go back to the game video.
    ClosePreview,

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
    /// The project's folder, absolute and canonical.
    pub folder: PathBuf,
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
    /// The export run: every target, how far each has got, and the rate.
    /// Sent as each target's whole percent moves, and last with nothing left
    /// running.
    Export(ExportRun),
    /// The clip being previewed, or `None` once the preview closed.
    Preview(Option<Uuid>),
    /// The whole transcription state (Phase 10 spec S5), so no view is left
    /// holding something the bus has moved past.
    ///
    /// The words themselves arrive as an [`Event::ProjectChanged`] sent
    /// before this one — when there are any. A run that wrote nothing sends
    /// no `ProjectChanged` at all, which is exactly why the state carries
    /// [`Finish::Silent`]: the view cannot tell "found nothing to say" from
    /// "never ran" by watching the project.
    Transcription(TranscriptionState),
    /// Select this clip: an undo restored or edited it, or a redo edited it.
    /// Always sent after that change's `ProjectChanged`, which drops a
    /// selection whose clip is gone.
    Select(Uuid),
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
    /// Export is refused: there's nothing (or no way) to export yet.
    #[error("can't export: {0}")]
    CantExport(String),
    /// Preview is refused, or the one running gave up.
    #[error("can't preview: {0}")]
    CantPreview(String),
    /// A notice: a match command is refused out loud (spec S5) — a team
    /// without a name, or a start/stop past the format's last period. Both
    /// controls are already disabled where this can fire, so it is a backstop;
    /// a modal for it could land over a live commentary take, where `v` is on
    /// the recording allow-list, and swallow the transport keys.
    #[error("{0}")]
    Scoreboard(&'static str),
    #[error("{0}")]
    Io(String),
}

impl UserError {
    /// Something the user should know that needs no answer: the UI shows it
    /// without blocking, since it can arrive mid-recording.
    pub fn is_notice(&self) -> bool {
        matches!(
            self,
            UserError::DeviceFallback { .. } | UserError::StopNotClean | UserError::Scoreboard(_)
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
    /// From the running exporter: there is only ever one.
    Export(ExportMessage),
    /// From the preview with this generation. A closed preview's last
    /// message can still be in the channel.
    Preview(u64, PreviewMessage),
    /// From the transcription with this generation, about this clip. A
    /// cancelled job's last message can still be in the channel, and the clip
    /// is what lets its words be kept anyway (spec S5).
    Transcription(u64, Uuid, TranscribeMessage),
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
    /// The one mailbox: the player and the preview both fill it, and the bus
    /// keeps only one of them PLAYING.
    mailbox: FrameMailbox,
    /// Which sinks the player was built with, and with them which GL context
    /// a preview may composite on (spec P1).
    sinks: SinkKind,
    /// The UI's GL display and context, once they arrive. Never set headless,
    /// where a preview composites on `Gl::shared()` instead (spec P1).
    gl: Option<Gl>,
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
    /// The clip undo history (Phase 3 spec C1). Cleared on every open.
    history: UndoController,
    /// The export run in progress, until its last target finishes.
    export: Option<export::Active>,
    /// The preview on screen.
    preview: Option<preview::Active>,
    /// Where the preview is, for the UI's tick: handed to each one started,
    /// as the mailbox is, so there is no position event of the preview's own
    /// (spec P3). Meaningless with no preview open.
    preview_position: PreviewPosition,
    /// The latest preview's generation. Messages from any other are stale.
    preview_generation: u64,
    /// Where transcripts come from (spec S8), chosen at [`Bus::spawn`]. For
    /// whisper it carries the model **a job starting now would run**:
    /// [`Bus::set_transcribe_model`] rewrites it, and the job in flight keeps
    /// the copy it was started with.
    transcribe: TranscribeKind,
    /// The coach's machine-wide choice of speech model, as `state.json`
    /// remembers it. Not always what `transcribe` points at:
    /// `$COACH_CUTS_WHISPER_MODEL` overrides the file without changing what
    /// was picked.
    transcribe_model: WhisperModel,
    /// The clips waiting to be transcribed, in order. The clip running is
    /// **not** in here, which is why enqueueing checks both.
    transcribe_queue: VecDeque<Uuid>,
    /// The transcription in progress.
    transcribing: Option<transcribe::Active>,
    /// The latest transcription's generation. Messages from any other are
    /// stale: without this a cancelled job's `Finished` clears `running` and
    /// a second job starts beside the one already going.
    transcribe_generation: u64,
    /// How the last transcription ended, when that is something to say —
    /// as macOS kept its failure: one slot, in memory, cleared on that clip's
    /// next try, its next success and on project open.
    transcribe_finished: Option<(Uuid, Finish)>,
}

impl Bus {
    /// Starts the bus thread with the player's video sink built from `sinks`.
    ///
    /// `sinks` picks the audio sink too: `Gl` is production, with
    /// `autoaudiosink`; `System` is headless tests, with `fakesink sync=true`,
    /// so playback still runs in real time without a sound device.
    ///
    /// `capture` picks where recordings come from: the camera and microphone,
    /// or test sources. `transcribe` picks where transcripts come from the
    /// same way: whisper with a model, or canned text (spec S8).
    ///
    /// `state` is the app's own state file — the last project and the chosen
    /// speech model. Production passes [`StateFile::default_location`]; tests
    /// pass a scratch directory, so the user's own is never touched.
    ///
    /// `events` is called on the bus thread.
    pub fn spawn(
        sinks: SinkKind,
        capture: CaptureKind,
        transcribe: TranscribeKind,
        state: StateFile,
        events: Box<dyn Fn(Event) + Send>,
    ) -> BusHandle {
        gst::init().expect("GStreamer failed to initialize");
        let (tx, rx) = mpsc::channel();
        let mailbox = FrameMailbox::default();
        let preview_position = PreviewPosition::default();
        let player = SourcePlayer::new(sinks, mailbox.clone(), {
            let tx = tx.clone();
            move |msg| {
                // Fails only once the bus thread has exited.
                let _ = tx.send(Input::Gst(msg));
            }
        });
        let position = player.position_handle();
        let transcribe_model = state.whisper_model();
        let bus = Bus {
            events,
            tx: tx.clone(),
            player,
            position: position.clone(),
            mailbox: mailbox.clone(),
            sinks,
            gl: None,
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
            history: UndoController::default(),
            export: None,
            preview: None,
            preview_position: preview_position.clone(),
            preview_generation: 0,
            transcribe,
            transcribe_model,
            transcribe_queue: VecDeque::new(),
            transcribing: None,
            transcribe_generation: 0,
            transcribe_finished: None,
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
            preview_position,
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
                Some(Input::Export(msg)) => self.export_message(msg),
                Some(Input::Preview(generation, msg)) => self.preview_message(generation, msg),
                Some(Input::Transcription(generation, clip, msg)) => {
                    self.transcription_message(generation, clip, msg)
                }
                Some(Input::Cmd(Command::Shutdown { ack })) => {
                    // This arm returns without reaching the loop's tail, so
                    // nothing new is transcribed from here however the
                    // teardown below moves the queue; the job in flight is
                    // cancelled — and not waited for — when the bus drops,
                    // because a whisper abort takes seconds and the ack
                    // below is what the UI's GL teardown is waiting on
                    // (`Bus::stop_transcription`).
                    //
                    // A recording keeps its clip (or is aborted while still
                    // starting) before anything is torn down. The preview's
                    // pipelines use the UI's GL context, so they go to NULL
                    // before the ack the UI's teardown waits for.
                    self.stop_recording();
                    self.close_preview();
                    // Dropping the player takes the pipeline to NULL before
                    // the ack, which the UI's GL teardown waits for. An
                    // export is cancelled and joined, its partial file
                    // deleted: its GL is its own, not the UI's.
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
            // The one place a transcription starts (Phase 10 spec S5), which
            // is enough because a recording, an export and a preview can only
            // end while the bus is handling an input or a deadline -- so the
            // machine can never come free with the thread parked in `recv`.
            // The alternative was a call at each of the eight places one of
            // the three ends, which was both easy to miss and, through
            // `load`'s close of the preview, wrong: it started a job
            // underneath a preview that was about to open. After
            // `dispatch_deadlines`, since one of those aborts a recording.
            self.run_next_if_idle();
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
                    | Command::Stroke { .. }
                    | Command::ClearAll { .. }
                    | Command::ToggleRecording { .. }
                    | Command::StopRecording
                    | Command::GlReady { .. }
                    // A metadata edit can't disturb a recording, and a
                    // field's focus-loss commit arrives after the
                    // `ToggleRecording` that took its focus.
                    | Command::EditClip { .. }
                    // The coach tags the match while scanning *or* recording
                    // (spec S4): the three keys are live throughout. Deleting
                    // and the setup sheet wait, as every other edit does.
                    | Command::TagMatchEvent { .. }
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
            Command::EditClip { id, edit } => self.edit_clip(id, edit),
            Command::DeleteClip(id) => self.delete_clip(id),
            Command::MoveClip { from, to } => self.move_clip(from, to),
            Command::SortClipsBySource => self.sort_clips_by_source(),
            Command::JumpToClip(id) => self.jump_to_clip(id),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::TagMatchEvent {
                kind,
                source_index,
                source_seconds,
            } => self.tag_match_event(kind, source_index, source_seconds),
            Command::DeleteMatchEvent(id) => self.delete_match_event(id),
            Command::SetScoreboard(config) => self.set_scoreboard(config),
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
            Command::Stroke { host_ns, stroke } => self.log_stroke(host_ns, stroke),
            Command::ClearAll { host_ns } => self.log_clear_all(host_ns),
            Command::SetCamera(camera) => self.set_camera(camera),
            Command::SetMic(mic) => self.set_mic(mic),
            Command::Export {
                targets,
                resolution,
                quality,
            } => self.export(targets, resolution, quality),
            Command::CancelExport => self.cancel_export(),
            Command::Transcribe { clip_id } => self.transcribe(clip_id),
            Command::CancelTranscription => self.cancel_transcription(),
            Command::SetTranscribeModel(model) => self.set_transcribe_model(model),
            Command::OpenPreview(id) => self.open_preview(id),
            Command::ClosePreview => self.close_preview(),
            Command::GlReady { display, context } => {
                self.gl = Some(Gl::wrapped(display.clone(), context.clone()));
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
    preview_position: PreviewPosition,
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

    /// Where the preview is, while one is open; the game video's position
    /// comes from [`BusHandle::position_handle`] otherwise (spec P3). With
    /// none open it holds whatever the last one left.
    pub fn preview_position(&self) -> &PreviewPosition {
        &self.preview_position
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
