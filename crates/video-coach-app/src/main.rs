//! Coach Cuts: the window.
//!
//! ```text
//! cargo run -p video-coach-app [-- <project folder>]
//! ```
//!
//! The binary is `coach-cuts`, the application ID.
//!
//! With a folder, opens (or creates) the project there; otherwise reopens the
//! last project. The UI thread owns only the window: the bus thread owns the
//! project and the player, takes [`Command`]s and answers with [`Event`]s,
//! which are handed to the UI thread with `upgrade_in_event_loop`.

mod pickers;
mod video;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use slint::{ComponentHandle, DataTransfer, Model, ModelRc, SharedString, VecModel};
use uuid::Uuid;

use video_coach_app::bus::{
    self, export_targets, whisper, whisper_model_override, Bus, BusHandle, CaptureKind, Command,
    Event, ExportRun, ExportTargetRun, Finish, RecordingStatus, Snapshot, Stage, StateFile,
    TargetState, TranscriptionState, WindowSize,
};
use video_coach_app::color_picker;
use video_coach_app::drawing::{path_commands, InProgress, Pen};
use video_coach_app::format::{finish_at, format_hms, format_hms_tenths, sentence};
use video_coach_app::highlight_view::{self, LiveHighlight as Ring};
use video_coach_app::match_panel::{
    self, parse_hex, parse_minutes, parse_overtime_periods, parse_periods,
};
use video_coach_app::wheel::Wheel;
use video_coach_app::zoom_input::{self, DragPan, Viewport};
use video_coach_core::avatar;
use video_coach_core::highlight::{highlight_shapes, HighlightEdit};
use video_coach_core::layout::{self, avatar_self_view_rect, self_view_rect, STROKE_LINE_WIDTH};
use video_coach_core::match_entry::{self, PendingMatchEvent};
use video_coach_core::plan::{ExportTarget, ScoreboardMode};
use video_coach_core::project::{Clip, Inset, Project, Quality, Resolution};
use video_coach_core::scoreboard::{
    MatchEventKind, MatchFormat, ReelEnd, ScoreboardConfig, ScoreboardContext, ScoreboardState,
    TeamConfig,
};
use video_coach_core::stroke::{Rgba, Stroke};
use video_coach_core::tag::{normalize_tags, tag_suggestions, tag_summaries, take_suggestion};
use video_coach_core::undo::ClipEdit;
use video_coach_core::zoom::{Zoom, SNAP_NOTCHES};
use video_coach_media::{
    avatar_drawn, list_devices, now_ns, Devices, PositionHandle, PreviewPosition,
    ScoreboardRenderer, SinkKind, WhisperModel, LEVEL_INTERVAL_NS,
};

use pickers::{Pick, Pickers};

slint::include_modules!();

/// How often the readout and scrubber follow the player (spec D8).
const TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);
/// How long a notice stays up.
const NOTICE: Duration = Duration::from_secs(6);
/// How long the self-view stays up without a new frame. It is hidden when
/// its frames stop, rather than frozen on the last one: a failed or stalled
/// self-view says nothing about the recording, and a frozen one would look
/// like the camera's picture.
const SELF_VIEW_QUIET: Duration = Duration::from_secs(1);
/// How far apart the recorder's `level` messages are, in seconds: the step the
/// **live** pulse estimator is smoothed at (avatar spec D1). Derived from the
/// recorder's own interval rather than restated, so the two cannot drift. The
/// rendered estimator steps at 1/30 through the same filter and the same
/// constants.
const LEVEL_DT: f64 = LEVEL_INTERVAL_NS as f64 / 1e9;
/// The side the project's avatar is drawn at for the UI (`media::avatar_drawn`).
/// One size serves both places it is shown — the Devices popover's 40 px
/// thumbnail and the corner during a take — because the corner is the larger
/// of the two and a square this size is a megabyte: the point of the cap is
/// only that a phone photo isn't uploaded whole to draw an inset with.
const AVATAR_SIZE: u32 = 512;
/// What a drag over the picture says outside a recording, where it can only
/// pan and at 1× visibly does nothing (`zoom_input::drawing_hint`).
const DRAWING_HINT: &str = "Drawing works while recording — press R";
/// What a drag in the H tool says while the picture plays (spec H3). A key
/// sits on the frame it was placed on, and while the picture runs that frame
/// is gone before the drag ends.
const HIGHLIGHT_PAUSE_HINT: &str = "Pause to place a highlight (Space)";
/// A drag shorter than this, in content-rect pixels, is a click: it selects
/// the ring under it rather than ringing a sliver of pitch.
const HIGHLIGHT_CLICK: f64 = 6.0;
/// How long after the pen lifts a drawing clears, with Auto-clear on (Phase 6
/// spec D3). The overlay's expiry is on `now_ns()`'s clock — the same anchor
/// the logged rule counts from — and the same span goes into the stroke, so
/// replay clears with the overlay.
const AUTO_CLEAR_NS: u64 = 5_000_000_000;
/// ... in seconds, which is the unit the stroke carries.
const AUTO_CLEAR: f64 = AUTO_CLEAR_NS as f64 / 1e9;

/// What the UI thread knows of the bus's state, from its events, and the
/// zoom, which is the UI's own.
struct UiState {
    /// The open project, from the latest `ProjectOpened` or `ProjectChanged`.
    snapshot: Option<Snapshot>,
    /// The source the player holds (or is loading).
    source_index: usize,
    /// While a seek is outstanding, where it's headed, concat seconds.
    target_abs: Option<f64>,
    /// The last successful position query, source seconds. Kept when a query
    /// fails (mid-load, nothing loaded).
    last_secs: f64,
    /// The player's zoom (spec D9). Not persisted; reset on project open.
    zoom: Zoom,
    /// The primary-button drag over the player, from its last press.
    drag: Option<DragPan>,
    /// The recording's t0 on `now_ns()`'s clock, once its video started,
    /// for the elapsed-time readout.
    recording_t0: Option<u64>,
    /// When the self-view's latest frame arrived, during this recording.
    self_view_at: Option<Instant>,
    /// This take's inset is the project's avatar, not a camera (avatar G2).
    /// Taken once, at the start of the take: neither avatar command is on
    /// the recording allow-list, so the mode cannot change under a running
    /// one.
    avatar_take: bool,
    /// How loud the coach is right now, `0..=1`, the **live** estimator of
    /// the pulse (avatar D1): stepped on each `Event::Level` through core's
    /// own filter, and read by the placement callback. Never persisted — the
    /// recording the render reads is (D4). 1.0 on a camera take, where
    /// `avatar_rect` is exactly `pip_rect`.
    self_view_level: f64,
    /// The drawings on screen, each with the `now_ns()` moment it auto-clears
    /// (Phase 6 spec D3) — the pen-up the logged rule counts from, on the
    /// same clock. Live, "now" only moves forward and a finished stroke is
    /// always fully drawn, so this is the whole of the replay rule for the
    /// live case; `visible_strokes` is for saved clips.
    live_strokes: Vec<(Stroke, Option<u64>)>,
    /// The drawing under the pen, if the coach is mid-stroke.
    drawing: Option<InProgress>,
    /// The pen a new stroke is drawn with, as `state.json` remembers it.
    pen: Pen,
    /// The content rect the window's `live-paths` were built for: their
    /// commands are in its pixels, so a resize has to rebuild them.
    paths_rect: (f64, f64),
    /// The player highlights the window is drawing (spec H5), as the last
    /// tick built them. Kept so the tick can set the model **only when it
    /// changed**: Slint re-parses every path on every set, and this runs at
    /// 30 Hz over a picture that usually has no highlight on it at all.
    highlight_rings: Vec<Ring>,
    /// The stream time of the scan frame on screen, as `video.rs` last drew
    /// one (spec H3, H6). A highlight key is placed at this time, so the box
    /// and the time describe one frame and `Decoder::frame_at` picks exactly
    /// that frame for it in export. `None` before the first frame, while a
    /// preview holds the shared mailbox, or on a frame with no stream time.
    shown_stream_time: Option<f64>,
    /// The drag in the H tool, from its press.
    highlight_drag: Option<HighlightDrag>,
    /// When the notice line clears, if one is up.
    notice_until: Option<Instant>,
    /// The previewed clip's duration while a preview is open. The transport
    /// then runs over the clip rather than the concat timeline (spec P6).
    preview_duration: Option<f64>,
    /// What the export sheet's rows stand for, in its order (Phase 8 E8).
    /// The window holds the labels and the ticks; the targets are here, since
    /// it has no type for one.
    export_targets: Vec<ExportTarget>,
    /// The scoreboard the Match panel's clock is read from (Phase 9 S2).
    /// Rebuilt on every project change and **never carried across one**: a
    /// source add, move, remove or relink moves the offsets it froze.
    scoreboard: Option<ScoreboardContext>,
    /// Draws the scoreboard the scan picture carries (spec S3) — **media's
    /// own rasterizer**, so the board on screen is the board the export burns
    /// in, pixel for pixel. Built on first use: a project with no scoreboard
    /// never pays for its fonts.
    board_renderer: Option<ScoreboardRenderer>,
    /// What the board on screen stands for: the teams, what it reads, and the
    /// device pixels it was drawn for. The tick rasterizes only when this
    /// changes — which is when the clock ticks (once a second), the score
    /// changes or the window is resized, not every frame.
    board_key: Option<BoardKey>,
    /// The wheel over the scrubber, part-way to its next notch.
    wheel: Wheel,
    /// The transcription queue (Phase 10 S5).
    transcription: Transcription,
    /// What the avatar image in hand was decoded from: the file name, its
    /// length and its modification time. A project change that didn't touch
    /// the image then costs a `stat` rather than a decode on the UI thread,
    /// and one that replaced it under the same name still costs a decode.
    /// `None` when there is no image, or its file has gone.
    avatar_shown: Option<AvatarFile>,
    /// That image, drawn as the export draws it (`media::avatar_drawn`): the
    /// popover's thumbnail and, when a take starts, the corner.
    ///
    /// **Decoded once, here, at the project change** — never when the coach
    /// presses R. A decode is a GStreamer pipeline with a ten-second bound on
    /// it, and the start of a take is the one moment on the UI thread that
    /// cannot afford one.
    avatar_image: Option<slint::Image>,
}

/// Which file the avatar thumbnail stands for. Not the path: it is always
/// the open project's folder, and the folder changing brings a project
/// change with it.
type AvatarFile = (String, u64, Option<SystemTime>);

/// What the scan picture's scoreboard is drawn from: the teams, what the board
/// reads at the displayed frame, and the frame it is laid out on in **device**
/// pixels. Equal keys draw the same board, so the tick can skip the raster.
type BoardKey = (ScoreboardConfig, ScoreboardState, u32, u32);

/// A drag in the H tool, from its press (spec H3).
struct HighlightDrag {
    /// Where it started, content-rect pixels.
    press: (f64, f64),
    /// Which frame was on screen then: the source and the stream time a key
    /// carries, captured at the input event as the bus contract requires.
    /// `None` while the picture plays, where the drag places nothing.
    at: Option<(usize, f64)>,
}

/// The transcription as the UI knows it (Phase 10 spec S5): what the bus
/// last published, and the one thing it doesn't say.
#[derive(Default)]
struct Transcription {
    /// The queue, the job running and how the last one ended, whole.
    state: TranscriptionState,
    /// When this UI first saw `state.running`'s clip at its current kind of
    /// [`Stage`] — so after a download, the clock is whisper's alone.
    ///
    /// The one genuinely window-local field: the inspector's readout is this
    /// clock, not the percent, because whisper's progress callback fires at
    /// the top of a loop advancing in ≤30 s chunks and never reports 100, so
    /// a clip shorter than one chunk reports 0 exactly once and nothing
    /// after.
    since: Option<Instant>,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            snapshot: None,
            source_index: 0,
            target_abs: None,
            last_secs: 0.0,
            zoom: Zoom::IDENTITY,
            drag: None,
            recording_t0: None,
            self_view_at: None,
            avatar_take: false,
            self_view_level: 1.0,
            live_strokes: Vec::new(),
            drawing: None,
            pen: Pen::default(),
            paths_rect: (0.0, 0.0),
            highlight_rings: Vec::new(),
            shown_stream_time: None,
            highlight_drag: None,
            notice_until: None,
            preview_duration: None,
            export_targets: Vec::new(),
            scoreboard: None,
            board_renderer: None,
            board_key: None,
            wheel: Wheel::default(),
            transcription: Transcription::default(),
            avatar_shown: None,
            avatar_image: None,
        }
    }
}

thread_local! {
    /// Bus events arrive on the UI thread through `upgrade_in_event_loop`,
    /// whose closure must be `Send`, so the state they update lives here.
    static UI: RefCell<UiState> = RefCell::default();
}

fn main() {
    // D2: Skia over EGL. FemtoVG on X11 uses GLX, which GStreamer can't
    // import DMABuf into; `video` checks for EGL at runtime too.
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia-opengl".into())
        .require_opengl_es()
        .select()
        .expect("unable to select Slint's winit backend with the skia-opengl renderer");
    // The window's Wayland `app_id` and X11 `WM_CLASS`, matching the desktop
    // entry's `StartupWMClass`. Only valid once a backend is selected.
    slint::set_xdg_app_id("coach-cuts").expect("set the application ID");

    let window = AppWindow::new().expect("create the window");

    // The last project, the chosen speech model and pen and the window's
    // size, all this machine's and none the project's.
    let state = StateFile::default_location();
    let model = state.whisper_model();
    show_transcribe_model(&window, model);
    window.set_pen_colors(ModelRc::new(VecModel::from(
        Pen::ALL.map(|p| slint_color(p.color())).to_vec(),
    )));
    set_pen(&window, state.pen());
    // The size the window last closed at. Whether it was maximised isn't
    // kept: winit asks for that straight after mapping the window, before the
    // window manager has taken it on, and Cinnamon's drops the request. A
    // maximised window's own size does the job instead: too big for the
    // screen with a frame on, it is maximised again.
    let size = state.window_size();
    window.window().set_size(slint::LogicalSize::new(
        size.width as f32,
        size.height as f32,
    ));
    // Written once the bus is gone, which also writes this file.
    let state_on_close = state.clone();

    let weak = window.as_weak();
    let bus = Bus::spawn(
        SinkKind::Gl,
        CaptureKind::Devices,
        // Downloaded into the cache on first use (Phase 11 spec S3), unless
        // `$COACH_CUTS_WHISPER_MODEL` names a file of the coach's own. Which
        // model the coach picked is remembered in `state`, and the bus
        // rewrites this when they pick another.
        whisper(model),
        state,
        Box::new(move |event| {
            let _ = weak.upgrade_in_event_loop(move |w| on_event(&w, event));
        }),
    );
    let position = bus.position_handle().clone();
    let preview_position = bus.preview_position().clone();
    let bus = Rc::new(RefCell::new(bus));
    video::install(&window, bus.clone());
    // The one pen width, from core: the live stroke layer and the highlight
    // rings are drawn with it, and it never changes while the window is up.
    window.set_stroke_line_width(STROKE_LINE_WIDTH as f32);
    wire_callbacks(&window, &bus);
    wire_zoom(&window, &bus);
    wire_drawing(&window, &bus);
    wire_highlights(&window, &bus);

    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, TICK, {
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                tick(&w, &position, &preview_position);
            }
        }
    });

    bus.borrow().send(match std::env::args_os().nth(1) {
        Some(folder) => Command::OpenProject(PathBuf::from(folder)),
        None => Command::RestoreLastProject,
    });

    window.run().expect("run the window");
    // Normally already done by the renderer's teardown; idempotent.
    bus.borrow_mut().shutdown();
    state_on_close.set_window_size(closing_window_size(&window));
}

/// The size to reopen at, read from the window after it has closed, which
/// still knows its last size.
fn closing_window_size(w: &AppWindow) -> WindowSize {
    let window = w.window();
    let size = window.size().to_logical(window.scale_factor());
    WindowSize {
        width: size.width.round() as u32,
        height: size.height.round() as u32,
    }
}

/// Turns the window's callbacks into bus commands. Values are passed on as
/// they are: the bus is the one place that sanitizes them (BACKLOG #28).
fn wire_callbacks(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    let send = |bus: &Rc<RefCell<BusHandle>>| {
        let bus = bus.clone();
        move |cmd: Command| bus.borrow().send(cmd)
    };
    let pickers = Pickers::default();

    window.on_open_project({
        let (weak, pickers, send) = (window.as_weak(), pickers.clone(), send(bus));
        move || {
            let Some(w) = weak.upgrade() else { return };
            let send = send.clone();
            pickers.open(&w, Pick::ProjectFolder, move |folder| {
                send(Command::OpenProject(folder))
            });
        }
    });
    window.on_add_source({
        let (weak, pickers, send) = (window.as_weak(), pickers.clone(), send(bus));
        move || {
            let Some(w) = weak.upgrade() else { return };
            let send = send.clone();
            let pick = Pick::Videos {
                title: "Add Source Videos",
            };
            pickers.open(&w, pick, move |path| send(Command::AddSource(path)));
        }
    });
    window.on_relink_source({
        let (weak, pickers, send) = (window.as_weak(), pickers.clone(), send(bus));
        move |index| {
            let (Some(w), Ok(index)) = (weak.upgrade(), usize::try_from(index)) else {
                return;
            };
            let send = send.clone();
            let pick = Pick::Video {
                title: "Locate the Missing Video",
            };
            pickers.open(&w, pick, move |path| {
                send(Command::RelinkSource(index, path))
            });
        }
    });
    window.on_choose_avatar({
        let (weak, pickers, send) = (window.as_weak(), pickers.clone(), send(bus));
        move || {
            let Some(w) = weak.upgrade() else { return };
            let send = send.clone();
            let pick = Pick::Image {
                title: "Choose an Avatar Image",
            };
            pickers.open(&w, pick, move |path| send(Command::SetAvatar(path)));
        }
    });
    window.on_remove_avatar({
        let send = send(bus);
        move || send(Command::ClearAvatar)
    });
    window.on_remove_source({
        let send = send(bus);
        move |index| {
            if let Ok(index) = usize::try_from(index) {
                send(Command::RemoveSource(index));
            }
        }
    });
    window.on_move_source({
        let send = send(bus);
        move |from, to| {
            if let (Ok(from), Ok(to)) = (usize::try_from(from), usize::try_from(to)) {
                send(Command::MoveSource { from, to });
            }
        }
    });
    window.on_rename_project({
        let (weak, send) = (window.as_weak(), send(bus));
        move |name| {
            let Some(w) = weak.upgrade() else { return };
            let name = name.trim();
            let unchanged =
                UI.with_borrow(|ui| ui.snapshot.as_ref().is_none_or(|s| s.project.name == name));
            if name.is_empty() || unchanged {
                return;
            }
            send(Command::RenameProject(name.into()));
            // What the field shows once it loses focus, which accepting
            // moves; the bus confirms it with `ProjectChanged`.
            w.set_saved_project_name(name.into());
        }
    });
    // Both capture their moment and position here, at the input event (the
    // bus contract): queue delay would put a recording's log behind.
    window.on_toggle_play({
        let (send, position) = (send(bus), bus.borrow().position_handle().clone());
        move || {
            send(Command::TogglePlay {
                host_ns: now_ns(),
                source_secs: position.query_position(),
            })
        }
    });
    window.on_skip(cmd(bus, |delta| Command::Skip {
        delta,
        host_ns: now_ns(),
    }));
    window.on_step_frame({
        let send = send(bus);
        move |forward| send(Command::StepFrame { forward })
    });
    window.on_step_scan_speed({
        let send = send(bus);
        move |step| {
            send(Command::ScanSpeed(match step {
                ScanStep::Faster => bus::ScanStep::Faster,
                ScanStep::Slower => bus::ScanStep::Slower,
                ScanStep::Cycle => bus::ScanStep::Cycle,
            }))
        }
    });
    window.on_scrub_move(cmd(bus, |abs| Command::ScrubMove { abs }));
    window.on_scrub_release(cmd(bus, |abs| Command::ScrubRelease { abs }));
    // A wheel over the scrubber skips, by whole notches, through the same
    // coordinator a held arrow key drives (`wheel.rs`). The moment is
    // captured here, at the input event, as the bus contract requires.
    window.on_scrub_scroll({
        let send = send(bus);
        move |dx, dy, shift| {
            let (Some(dx), Some(dy)) = (finite(dx), finite(dy)) else {
                return;
            };
            if let Some(delta) = UI.with_borrow_mut(|ui| ui.wheel.scrolled(dx, dy, shift)) {
                send(Command::Skip {
                    delta,
                    host_ns: now_ns(),
                });
            }
        }
    });
    window.on_volume_changed(cmd(bus, |value| Command::SetVolume {
        value,
        commit: false,
    }));
    window.on_volume_released(cmd(bus, |value| Command::SetVolume {
        value,
        commit: true,
    }));
    // The bus decides start or stop: the UI's status can lag it.
    window.on_toggle_recording({
        let send = send(bus);
        move || {
            let zoom = UI.with_borrow(|ui| ui.zoom);
            send(Command::ToggleRecording { zoom })
        }
    });
    window.on_stop_recording({
        let send = send(bus);
        move || send(Command::StopRecording)
    });
    wire_devices(window, bus);
    wire_clips(window, bus);
    wire_export(window, bus);
    wire_preview(window, bus);
    wire_match(window, bus);
    wire_match_editor(window, bus);
    // Drag-to-reorder carries the list's name and the dragged row's index,
    // so a source dropped on the clip list (or back) is refused.
    window.on_drag_payload(|list, index| {
        DataTransfer::from(SharedString::from(format!("{list}:{index}")))
    });
    window.on_dropped_index(|list, data| {
        data.plain_text()
            .ok()
            .and_then(|text| {
                let (from, index) = text.split_once(':')?;
                if from != list.as_str() {
                    return None;
                }
                index.parse().ok()
            })
            .unwrap_or(-1)
    });
}

/// The Clips list and the undo keys (Phase 3 C5). Rows name their clip by
/// its UUID; the selection is the window's `selected-clip`.
fn wire_clips(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    let by_id = |bus: &Rc<RefCell<BusHandle>>, command: fn(Uuid) -> Command| {
        let bus = bus.clone();
        move |id: SharedString| {
            if let Some(id) = parse_id(&id) {
                bus.borrow().send(command(id));
            }
        }
    };
    window.on_jump_to_clip(by_id(bus, Command::JumpToClip));
    window.on_delete_clip(by_id(bus, Command::DeleteClip));
    window.on_move_clip({
        let bus = bus.clone();
        move |from, to| {
            if let (Ok(from), Ok(to)) = (usize::try_from(from), usize::try_from(to)) {
                bus.borrow().send(Command::MoveClip { from, to });
            }
        }
    });
    window.on_sort_clips({
        let bus = bus.clone();
        move || bus.borrow().send(Command::SortClipsBySource)
    });
    window.on_undo({
        let bus = bus.clone();
        move || bus.borrow().send(Command::Undo)
    });
    window.on_redo({
        let bus = bus.clone();
        move || bus.borrow().send(Command::Redo)
    });
    wire_inspector(window, bus);
    window.on_filter_changed({
        let weak = window.as_weak();
        move || {
            let Some(w) = weak.upgrade() else { return };
            UI.with_borrow(|ui| {
                if let Some(s) = &ui.snapshot {
                    show_clips(&w, &s.project);
                }
            });
        }
    });
}

/// Export (Phase 8 E8): the sheet is the only export UI. The Export… button
/// opens it over the whole target list, the clip menu's "Export video…" opens
/// it on that one clip, and Export hands the bus what's ticked. Every target
/// goes into the project's own `exports/` folder, so there is no save picker.
fn wire_export(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    window.on_open_export({
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                // The selected clip gets a row of its own, unticked: the
                // sheet's default is everything else (spec E8).
                open_export_sheet(&w, selected_id(&w), false);
            }
        }
    });
    window.on_export_clip({
        let weak = window.as_weak();
        move |id| {
            let (Some(w), Some(id)) = (weak.upgrade(), parse_id(&id)) else {
                return;
            };
            // This clip was asked for, so it is the only thing ticked.
            open_export_sheet(&w, Some(id), true);
        }
    });
    window.on_tick_target({
        let weak = window.as_weak();
        move |index, ticked| {
            let (Some(w), Ok(index)) = (weak.upgrade(), usize::try_from(index)) else {
                return;
            };
            let rows = w.get_export_targets();
            let Some(row) = rows.row_data(index) else {
                return;
            };
            rows.set_row_data(index, TargetRow { ticked, ..row });
            w.set_export_any_ticked(rows.iter().any(|row| row.ticked));
            w.set_export_whole_match_ticked(whole_match_ticked(rows.iter()));
        }
    });
    window.on_start_export({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move || {
            let Some(w) = weak.upgrade() else { return };
            let ticked = w.get_export_targets();
            let targets = UI.with_borrow(|ui| {
                ui.export_targets
                    .iter()
                    .zip(ticked.iter())
                    .filter(|(_, row)| row.ticked)
                    .map(|(target, _)| target.clone())
                    .collect()
            });
            bus.borrow().send(Command::Export {
                targets,
                resolution: match w.get_export_resolution() {
                    0 => Resolution::R720,
                    _ => Resolution::R1080,
                },
                quality: match w.get_export_quality() {
                    0 => Quality::Low,
                    2 => Quality::High,
                    _ => Quality::Medium,
                },
                // 0 is "Default", which is the target's own (spec M1).
                scoreboard: match w.get_export_scoreboard() {
                    1 => Some(ScoreboardMode::Burned),
                    2 => Some(ScoreboardMode::Track),
                    _ => None,
                },
            });
        }
    });
    window.on_cancel_export({
        let bus = bus.clone();
        move || bus.borrow().send(Command::CancelExport)
    });
}

/// Opens the export sheet: every target the project offers, with `clip`'s own
/// row ticked or everything but it (spec E8), and the pickers at the
/// project's last choice (spec E4).
fn open_export_sheet(w: &AppWindow, clip: Option<Uuid>, only_clip: bool) {
    let Some((resolution, quality, scoreboard, rows)) = UI.with_borrow_mut(|ui| {
        let project = &ui.snapshot.as_ref()?.project;
        let targets = export_targets(project, clip);
        let rows: Vec<TargetRow> = targets
            .iter()
            .map(|row| {
                let plural = if row.count == 1 { "" } else { "s" };
                TargetRow {
                    label: row.label.as_str().into(),
                    detail: format!(
                        "{} {}{plural} · {}",
                        row.count,
                        row.unit,
                        format_hms(row.seconds)
                    )
                    .into(),
                    // The clip's row is ticked only when the sheet was
                    // opened on it. The reel is never ticked by default: it
                    // renders 26 s a goal, which is a long wait nobody asked
                    // for on an ordinary export. Nor is the whole match,
                    // which is the longest render there is (spec W1).
                    ticked: match row.target {
                        ExportTarget::Clip(_) => only_clip,
                        ExportTarget::Reel(_) | ExportTarget::WholeMatch => false,
                        _ => !only_clip,
                    },
                }
            })
            .collect();
        let prefs = &project.preferences;
        let picked = (
            prefs.last_export_resolution,
            prefs.last_export_quality,
            prefs.last_export_scoreboard,
            rows,
        );
        // The rows the sheet shows and the targets a tick means, in the same
        // order: the sheet reads back only the ticks.
        ui.export_targets = targets.into_iter().map(|row| row.target).collect();
        Some(picked)
    }) else {
        return;
    };
    w.set_export_resolution(match resolution {
        Resolution::R720 => 0,
        // 2160p is kept in the format but not offered (E8), so it shows as
        // 1080p — and a run started here saves it as that.
        _ => 1,
    });
    w.set_export_quality(match quality {
        Quality::Low => 0,
        Quality::Medium => 1,
        Quality::High => 2,
    });
    w.set_export_scoreboard(match scoreboard {
        None => 0,
        Some(ScoreboardMode::Burned) => 1,
        Some(ScoreboardMode::Track) => 2,
    });
    w.set_export_any_ticked(rows.iter().any(|row| row.ticked));
    w.set_export_whole_match_ticked(whole_match_ticked(rows.iter().cloned()));
    w.set_export_targets(ModelRc::new(VecModel::from(rows)));
    w.set_export_sheet_open(true);
}

/// Whether the whole-match row is among the ticked ones.
///
/// It is the one target a run ever copies, so it is also the one whose
/// effective mode can be "separate track" — which is what the sheet's line
/// under the Scoreboard picker is about (spec M1).
fn whole_match_ticked(rows: impl Iterator<Item = TargetRow>) -> bool {
    UI.with_borrow(|ui| {
        ui.export_targets
            .iter()
            .zip(rows)
            .any(|(target, row)| row.ticked && matches!(target, ExportTarget::WholeMatch))
    })
}

/// Preview (Phase 7 P6): the inspector's button and the clip menu's "Preview
/// clip" open one, Close and Esc shut it. Opening is explicit — Space goes on
/// meaning "play the game video" until one is open (P5).
fn wire_preview(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    window.on_open_preview({
        let bus = bus.clone();
        move |id| {
            if let Some(id) = parse_id(&id) {
                bus.borrow().send(Command::OpenPreview(id));
            }
        }
    });
    window.on_close_preview({
        let bus = bus.clone();
        move || bus.borrow().send(Command::ClosePreview)
    });
}

/// The Match panel and the three tag keys (Phase 9 S4). Tagging, deleting and
/// the setup all go through the bus; the panel renders what comes back.
fn wire_match(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    window.on_tag_match_event({
        let (bus, position) = (bus.clone(), bus.borrow().position_handle().clone());
        move |tag| {
            let Some((source_index, source_seconds)) = scan_source_position(&position) else {
                return;
            };
            bus.borrow().send(Command::TagMatchEvent {
                kind: match tag {
                    MatchTag::HomeGoal => MatchEventKind::HomeGoal,
                    MatchTag::AwayGoal => MatchEventKind::AwayGoal,
                    MatchTag::StartStop => MatchEventKind::StartStop,
                },
                source_index,
                source_seconds,
            });
        }
    });
    // One frame-accurate seek, the same one a scrub release makes.
    window.on_seek_match_event({
        let bus = bus.clone();
        move |id| {
            let Some(abs) = parse_id(&id).and_then(|id| {
                UI.with_borrow(|ui| {
                    let project = &ui.snapshot.as_ref()?.project;
                    let event = project.match_events.iter().find(|m| m.id == id)?;
                    Some(project.abs_seconds(event.source_index, event.source_seconds))
                })
            }) else {
                return;
            };
            bus.borrow().send(Command::ScrubRelease { abs });
        }
    });
    // The panel's `×` and the editor's alike.
    window.on_delete_match_event({
        let bus = bus.clone();
        move |id| {
            if let Some(id) = parse_id(&id) {
                bus.borrow().send(Command::DeleteMatchEvent(id));
            }
        }
    });
    // A goal's reel span (R3), set where the coach is looking: the scan
    // position, taken at the click as a tag's is.
    window.on_reel_trim({
        let (bus, position) = (bus.clone(), bus.borrow().position_handle().clone());
        move |id, trim| {
            let Some(goal) = parse_id(&id) else { return };
            let (end, here) = match trim {
                ReelTrim::StartHere => (ReelEnd::Start, true),
                ReelTrim::EndHere => (ReelEnd::End, true),
                ReelTrim::ResetStart => (ReelEnd::Start, false),
                ReelTrim::ResetEnd => (ReelEnd::End, false),
            };
            // `None` is a reset, so a set with no position sends nothing.
            let at = if here {
                let Some(at) = scan_source_position(&position) else {
                    return;
                };
                Some(at)
            } else {
                None
            };
            bus.borrow().send(Command::SetReelTrim { goal, end, at });
        }
    });
    // `[` and `]` (C1): the same frame-accurate seek as a row's.
    window.on_jump_chapter({
        let (bus, position) = (bus.clone(), bus.borrow().position_handle().clone());
        move |forward| {
            let Some(abs) = UI.with_borrow_mut(|ui| {
                let project = ui.snapshot.as_ref()?.project.clone();
                let now = scan_abs(ui, &project, &position);
                let chapters = match_panel::match_abs(&project);
                let abs = if forward {
                    match_panel::next_chapter(now, &chapters)
                } else {
                    match_panel::previous_chapter(now, &chapters)
                }?;
                // Where the seek is headed, before the bus says so: a second
                // press that comes first steps on from here, not from the
                // chapter this one is leaving.
                ui.target_abs = Some(abs);
                Some(abs)
            }) else {
                return;
            };
            bus.borrow().send(Command::ScrubRelease { abs });
        }
    });
    window.on_open_match_setup({
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                open_match_setup(&w);
            }
        }
    });
    window.on_save_match_setup({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move || {
            let Some(w) = weak.upgrade() else { return };
            // Save is disabled while a field doesn't parse, so this is a UI
            // bug rather than something the coach did.
            match match_setup(&w) {
                Some(config) => bus.borrow().send(Command::SetScoreboard(config)),
                None => show_error(&w, "that match setup couldn't be read"),
            }
        }
    });
    // The sheet's validators. Each takes the text it judges, so the bindings
    // that call it re-evaluate as it's typed.
    window.on_hex_color(|text| match parse_hex(&text) {
        Some(c) => slint::Color::from_rgb_f32(c.r as f32, c.g as f32, c.b as f32),
        None => slint::Color::from_argb_u8(0, 0, 0, 0),
    });
    // The name the command sees: `match_setup` trims it, so spaces alone are
    // no name.
    window.on_valid_name(|text| !text.trim().is_empty());
    window.on_valid_hex(|text| parse_hex(&text).is_some());
    // The colour picker: its row of ready-made colours, and the two
    // conversions it is drawn and read with. Both are `color_picker`'s, so
    // the picker lands on exactly the bytes the field stores and the
    // scoreboard draws — see that module's header.
    window.set_kit_colors(ModelRc::new(VecModel::from(
        color_picker::KIT_COLORS
            .iter()
            .map(|kit| KitColor {
                name: kit.name.into(),
                hex: kit.hex.into(),
            })
            .collect::<Vec<_>>(),
    )));
    window.on_hsv_of_hex(|text| {
        // Half-typed text has no position; the sheet gates on `valid-hex`
        // before it moves the picker, so this is never what it stands on.
        let hsv = color_picker::hsv_of_hex(&text).unwrap_or(color_picker::Hsv {
            hue: 0.0,
            sat: 0.0,
            val: 0.0,
        });
        Hsv {
            hue: hsv.hue,
            sat: hsv.sat,
            val: hsv.val,
        }
    });
    window.on_hex_of_hsv(|hue, sat, val| color_picker::hex_of_hsv(hue, sat, val).into());
    // One validator per field, sharing its range with the parse that builds
    // the config, so a field can't read good and then fail to save.
    window.on_valid_periods(|text| parse_periods(&text).is_some());
    window.on_valid_overtime_periods(|text| parse_overtime_periods(&text).is_some());
    window.on_valid_minutes(|text| parse_minutes(&text).is_some());
    // The back-anchor is in here because it takes a period: ticking the box
    // moves the warning, so the sheet passes the box as it currently stands.
    window.on_match_over_cap(|regulation, overtime, back_anchor| {
        let Some((regulation, overtime)) =
            parse_periods(&regulation).zip(parse_overtime_periods(&overtime))
        else {
            return SharedString::new();
        };
        UI.with_borrow(|ui| {
            ui.snapshot.as_ref().map_or_else(SharedString::new, |s| {
                match_panel::over_cap_warning(&s.project, regulation + overtime, back_anchor).into()
            })
        })
    });
}

/// The match event editor (spec T, B): the sheet's rows, its row field and
/// its paste box.
///
/// Every verdict is the bus's own (`bus::editor_line`, `core::match_entry`),
/// so a field marked good is never refused; what is here is the seeding and
/// the marks.
fn wire_match_editor(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    window.on_open_match_editor({
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                open_match_editor(&w);
            }
        }
    });
    // A row was clicked: its field is seeded with that event as a line, which
    // is the only thing the selection carries.
    window.on_select_match_event({
        let weak = window.as_weak();
        move |id| {
            let Some(w) = weak.upgrade() else { return };
            w.set_match_editor_selected(id.clone());
            seed_match_line(&w, &id);
        }
    });
    // Esc in the row's field cancels rather than commits (spec P3): the field
    // goes back to the line it was seeded with, and the window then drops
    // focus, which commits that same line — an edit of nothing.
    window.on_reseed_match_line({
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                let id = w.get_match_editor_selected();
                seed_match_line(&w, &id);
            }
        }
    });
    // The field's ✕, and the guard on its commit: the bus's own reader, so a
    // line it will refuse — the start/stop cap included — is marked before it
    // is ever sent (spec T5, V4).
    window.on_valid_match_line(|id, line| matches!(editor_verdict(&id, &line), Some(Ok(_))));
    window.on_edit_match_event({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move |id, line| {
            let Some(w) = weak.upgrade() else { return };
            let Some(id) = parse_id(&id) else { return };
            w.set_match_editor_message(SharedString::new());
            bus.borrow().send(Command::EditMatchEvent {
                id,
                line: line.to_string(),
            });
        }
    });
    window.on_add_match_events({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move || {
            let Some(w) = weak.upgrade() else { return };
            // The box is left exactly as the bus's own parse leaves it, which
            // arrives as `Event::MatchPasteLeftover` (spec B5). Stripping the
            // lines this side thought had been added would lose any the bus
            // read differently: they would be in neither the box nor the
            // project.
            w.set_match_editor_message(SharedString::new());
            bus.borrow().send(Command::AddMatchEvents {
                text: w.get_match_editor_paste().to_string(),
                default_source: w.get_match_editor_source().max(0) as usize,
            });
        }
    });
    // The paste box's echo, re-read on every keystroke: it takes the text,
    // the picker and the project's revision, so the binding that calls it
    // re-evaluates as any of the three change. It is declared `pure` and
    // reads the project, which is what the revision is for — without it,
    // deleting a row with its `×` would leave the echo saying "already
    // tagged" and Add disabled with no way back.
    window.on_check_paste(|text, default_source, _revision| {
        UI.with_borrow(|ui| {
            let Some(s) = ui.snapshot.as_ref() else {
                return PasteEcho::default();
            };
            let batch = match_entry::parse_batch(&s.project, default_source.max(0) as usize, &text);
            let echo = match_panel::paste_echo(&batch);
            let lines: Vec<PasteLine> = echo
                .lines
                .into_iter()
                .map(|line| PasteLine {
                    glyph: line.glyph.into(),
                    text: line.text.into(),
                })
                .collect();
            PasteEcho {
                lines: ModelRc::new(VecModel::from(lines)),
                summary: echo.summary.into(),
                adding: batch.events.len() as i32,
                button: echo.button.into(),
            }
        })
    });
}

/// How the bus will read one line of the row field, for the row named by the
/// id the field carries; `None` when there is no such event, which leaves the
/// field unmarked.
fn editor_verdict(id: &str, line: &str) -> Option<Result<PendingMatchEvent, String>> {
    let id = parse_id(id)?;
    UI.with_borrow(|ui| {
        let project = &ui.snapshot.as_ref()?.project;
        let record = project.match_events.iter().find(|m| m.id == id)?;
        Some(bus::editor_line(project, record, line))
    })
}

/// Fills the row field with the selected event as a line — core's own
/// rendering, which `bus::editor_line` rebuilds as the seed when the line
/// comes back, so an edit that leaves the time alone keeps the stored seconds
/// to the last decimal. Empty for an id with no event behind it.
fn seed_match_line(w: &AppWindow, id: &str) {
    let line = parse_id(id).and_then(|id| {
        UI.with_borrow(|ui| {
            let record = ui
                .snapshot
                .as_ref()?
                .project
                .match_events
                .iter()
                .find(|m| m.id == id)?;
            Some(match_entry::format_line(
                record.kind,
                record.source_index,
                record.source_seconds,
            ))
        })
    });
    w.set_match_editor_line(line.unwrap_or_default().into());
}

/// Where the game video is, as the source and offset a command that places
/// something carries. Captured at the input event, as the bus contract
/// requires, and mapped back through `locate`: reading the index and the
/// offset separately would pair a new source with the old one's offset
/// across a cross-source seek (spec S5). `None` with no project or no source.
fn scan_source_position(position: &PositionHandle) -> Option<(usize, f64)> {
    UI.with_borrow_mut(|ui| {
        let project = ui.snapshot.as_ref()?.project.clone();
        let abs = scan_abs(ui, &project, position);
        (!project.source_videos.is_empty()).then(|| project.locate(abs))
    })
}

/// Which frame is on screen, as the source and offset a **highlight key**
/// carries (spec H3): the displayed frame's own stream time, so the box and
/// the time describe one frame and `Decoder::frame_at` picks exactly that
/// frame for it in export (H6). `None` with no project or no source.
fn shown_source_position(position: &PositionHandle) -> Option<(usize, f64)> {
    UI.with_borrow_mut(|ui| {
        let project = ui.snapshot.as_ref()?.project.clone();
        if project.source_videos.is_empty() {
            return None;
        }
        let abs = scan_abs(ui, &project, position);
        Some(shown_position(ui, &project, abs))
    })
}

/// [`shown_source_position`] once the scan position is already in hand, which
/// is how the 30 Hz tick asks without querying the player twice.
///
/// The displayed frame's time, or — with no frame yet, or a seek outstanding,
/// where `ui.source_index` is already the *target's* and pairing it with the
/// old frame's time would name the wrong source — the scan position through
/// `locate`, as every other caller-captured position is taken.
fn shown_position(ui: &UiState, project: &Project, abs: f64) -> (usize, f64) {
    match (ui.shown_stream_time, ui.target_abs) {
        (Some(t), None) if ui.source_index < project.source_videos.len() => (ui.source_index, t),
        _ => project.locate(abs),
    }
}

/// Seeds the setup sheet from the project's scoreboard — or from a blank one
/// when there is none yet — and opens it. The only way in, so the fields are
/// never stale.
fn open_match_setup(w: &AppWindow) {
    let Some(config) = UI.with_borrow(|ui| {
        let project = &ui.snapshot.as_ref()?.project;
        Some(
            project
                .scoreboard
                .clone()
                .unwrap_or_else(match_panel::blank_config),
        )
    }) else {
        return;
    };
    // The sheet writes whole minutes back, so this only ever rounds a config
    // some other build wrote.
    let minutes = |seconds: u32| SharedString::from(((seconds + 30) / 60).max(1).to_string());
    w.set_match_home_name(config.home.name.as_str().into());
    w.set_match_home_primary(match_panel::hex(config.home.primary_color).into());
    w.set_match_home_secondary(match_panel::hex(config.home.secondary_color).into());
    w.set_match_home_font(match_panel::hex(config.home.font_color).into());
    w.set_match_away_name(config.away.name.as_str().into());
    w.set_match_away_primary(match_panel::hex(config.away.primary_color).into());
    w.set_match_away_secondary(match_panel::hex(config.away.secondary_color).into());
    w.set_match_away_font(match_panel::hex(config.away.font_color).into());
    w.set_match_regulation_periods(config.format.regulation_periods.to_string().into());
    w.set_match_regulation_minutes(minutes(config.format.regulation_period_seconds));
    w.set_match_overtime_periods(config.format.overtime_periods.to_string().into());
    w.set_match_overtime_minutes(minutes(config.format.overtime_period_seconds));
    w.set_match_back_anchor(config.auto_back_anchor_p1);
    w.set_match_sheet_open(true);
}

/// The setup sheet's fields as a config; `None` if one of them doesn't parse.
/// The bus has the last word on it — it refuses a team without a name.
fn match_setup(w: &AppWindow) -> Option<ScoreboardConfig> {
    let team =
        |name: SharedString, primary: SharedString, secondary: SharedString, font: SharedString| {
            Some(TeamConfig {
                name: name.trim().to_string(),
                primary_color: parse_hex(&primary)?,
                secondary_color: parse_hex(&secondary)?,
                font_color: parse_hex(&font)?,
            })
        };
    let seconds = |text: SharedString| Some(parse_minutes(&text)? * 60);
    Some(ScoreboardConfig {
        home: team(
            w.get_match_home_name(),
            w.get_match_home_primary(),
            w.get_match_home_secondary(),
            w.get_match_home_font(),
        )?,
        away: team(
            w.get_match_away_name(),
            w.get_match_away_primary(),
            w.get_match_away_secondary(),
            w.get_match_away_font(),
        )?,
        format: MatchFormat {
            regulation_periods: parse_periods(&w.get_match_regulation_periods())?,
            regulation_period_seconds: seconds(w.get_match_regulation_minutes())?,
            overtime_periods: parse_overtime_periods(&w.get_match_overtime_periods())?,
            overtime_period_seconds: seconds(w.get_match_overtime_minutes())?,
        },
        auto_back_anchor_p1: w.get_match_back_anchor(),
    })
}

/// A goal's scrubber mark when there is no scoreboard, so no team colour:
/// amber, which reads against the track and against a start/stop's white.
const GOAL_MARK: slint::Color = slint::Color::from_rgb_u8(0xf5, 0xb0, 0x00);

/// The Match panel's rows, and what its actions are gated on. The live score
/// and clock aren't here: they follow the scan, so the tick renders them.
fn show_match(w: &AppWindow, project: &Project) {
    let rows = match_panel::match_rows(project);
    // A chapter mark per row (C1): a goal in its team's colour, or one fixed
    // goal colour with no teams to take one from, and a start/stop in white.
    let color = |c: Rgba| slint::Color::from_rgb_f32(c.r as f32, c.g as f32, c.b as f32);
    let marks: Vec<Mark> = rows
        .iter()
        .map(|row| Mark {
            at: row.abs as f32,
            color: match (row.kind, &project.scoreboard) {
                (MatchEventKind::HomeGoal, Some(c)) => color(c.home.primary_color),
                (MatchEventKind::AwayGoal, Some(c)) => color(c.away.primary_color),
                (MatchEventKind::HomeGoal | MatchEventKind::AwayGoal, None) => GOAL_MARK,
                (MatchEventKind::StartStop, _) => slint::Color::from_rgb_u8(255, 255, 255),
            },
        })
        .collect();
    w.set_match_marks(ModelRc::new(VecModel::from(marks)));
    // A goal's row has a second line for its reel span.
    let goals = rows.iter().filter(|r| r.reel_span.is_some()).count();
    w.set_match_list_lines((rows.len() + goals) as i32);
    let rows: Vec<MatchRow> = rows
        .into_iter()
        .map(|row| MatchRow {
            id: row.id.to_string().into(),
            time: row.time.into(),
            label: row.label.into(),
            role_less: row.role_less,
            reel_span: row.reel_span.unwrap_or_default().into(),
        })
        .collect();
    w.set_match_rows(ModelRc::new(VecModel::from(rows)));
    w.set_match_configured(project.scoreboard.is_some());
    w.set_match_at_cap(project.start_stops_at_cap());
}

/// Seeds the editor from the project and opens it, as `open_match_setup`
/// seeds the setup sheet: the rows, the picker's videos and nothing selected.
///
/// **The paste box's text is not touched.** It is the session's, so a coach
/// who pasted a block, pressed a row's Go and came back finds it (spec F2).
fn open_match_editor(w: &AppWindow) {
    let Some(sources) = UI.with_borrow(|ui| {
        let project = &ui.snapshot.as_ref()?.project;
        Some(
            project
                .source_videos
                .iter()
                .enumerate()
                .map(|(i, s)| SharedString::from(format!("{} · {}", i + 1, s.display_name)))
                .collect::<Vec<_>>(),
        )
    }) else {
        return;
    };
    if sources.is_empty() {
        return;
    }
    w.set_match_editor_sources(ModelRc::new(VecModel::from(sources)));
    // The picker resets to the first video on every open (spec F2).
    w.set_match_editor_source(0);
    w.set_match_editor_selected(SharedString::new());
    w.set_match_editor_line(SharedString::new());
    w.set_match_editor_message(SharedString::new());
    UI.with_borrow(|ui| {
        if let Some(s) = ui.snapshot.as_ref() {
            show_match_editor(w, &s.project);
        }
    });
    w.set_match_editor_open(true);
}

/// The editor's rows, and the selected row's line re-seeded from its record.
///
/// **The one thing that must not happen is a rebuild under the row's field
/// while it is being typed in** (spec T3): a `LineEdit` inside a `for` can
/// only be bound one way, so re-seeding it would overwrite what is in it. So
/// `show_project` calls this on every project change *except* while that
/// field has focus — which is a state the coach's hands are in and nothing
/// else, since every commit drops focus (spec T4). Not "only after the
/// editor's own command": the sheet can have two commands in flight, and a
/// command the bus refuses publishes nothing at all.
fn show_match_editor(w: &AppWindow, project: &Project) {
    let rows = match_panel::match_rows(project);
    let model: Vec<MatchEditorRow> = rows
        .iter()
        .map(|row| MatchEditorRow {
            id: row.id.to_string().into(),
            where_text: match_panel::editor_row_where(row).into(),
            label: row.label.as_str().into(),
            role_less: row.role_less,
        })
        .collect();
    w.set_match_editor_rows(ModelRc::new(VecModel::from(model)));
    // A selection whose event is gone — deleted, or undone away — is dropped,
    // as the clip list's is.
    let selected = parse_id(&w.get_match_editor_selected())
        .and_then(|id| rows.iter().find(|row| row.id == id));
    match selected {
        Some(row) => w.set_match_editor_line(match_panel::editor_row_line(row).into()),
        None => {
            w.set_match_editor_selected(SharedString::new());
            w.set_match_editor_line(SharedString::new());
        }
    }
}

/// The Highlights panel's rows (spec H3), in stored order.
///
/// A highlight that is gone — its last key deleted, or an undo — takes the
/// selection with it, so the panel never offers a label field for a player
/// who isn't there.
fn show_highlights(w: &AppWindow, project: &Project) {
    let rows: Vec<HighlightRow> = project
        .player_highlights
        .iter()
        .map(|h| HighlightRow {
            id: h.id.to_string().into(),
            ink: slint_color(h.color),
            label: h.label.as_str().into(),
        })
        .collect();
    w.set_highlight_rows(ModelRc::new(VecModel::from(rows)));
    let selected =
        selected_highlight(w).and_then(|id| project.player_highlights.iter().find(|h| h.id == id));
    match selected {
        // Not while the label is being typed in, which this would overwrite.
        Some(h) if w.get_editing_highlight_id().is_empty() => {
            w.set_highlight_label(h.label.as_str().into())
        }
        Some(_) => {}
        None => select_highlight(w, project, None),
    }
}

/// The inspector (Phase 3 C7, C8). A commit names its clip: the one the
/// field was editing, which needn't be the selection any more.
fn wire_inspector(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    window.on_edit_clip({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move |id, field, text| {
            let Some(w) = weak.upgrade() else { return };
            let edit = match field {
                ClipField::Name => ClipEdit::Name(text.into()),
                ClipField::Tags => ClipEdit::Tags(normalize_tags(&text)),
                ClipField::Notes => ClipEdit::Notes(text.into()),
                // The coach's own edit of a transcript is an undo step like
                // any other; the machine's write of one is not (spec S7).
                ClipField::Transcript => ClipEdit::Transcript(text.into()),
            };
            let id = parse_id(&id);
            // Whether the bus will apply it: it skips an edit that sets the
            // value already there.
            let changes = UI.with_borrow(|ui| {
                ui.snapshot
                    .as_ref()
                    .and_then(|s| s.project.clips.iter().find(|c| Some(c.id) == id))
                    .is_some_and(|clip| clip.clone().set(edit.clone()) != edit)
            });
            match id {
                // Its `ProjectChanged` re-renders the fields. Rendering them
                // now would flash the old value.
                Some(id) if changes => bus.borrow().send(Command::EditClip { id, edit }),
                // The bus would send nothing back, so the field shows the
                // clip again itself: an edit that normalizes away, or of a
                // clip that has gone.
                _ => show_clip(&w),
            }
        }
    });
    window.on_set_show_pip({
        let bus = bus.clone();
        move |id, on| {
            if let Some(id) = parse_id(&id) {
                bus.borrow().send(Command::EditClip {
                    id,
                    edit: ClipEdit::ShowPip(on),
                });
            }
        }
    });
    window.on_show_clip({
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                show_clip(&w);
            }
        }
    });
    window.on_transcribe({
        let bus = bus.clone();
        move |id| {
            if let Some(clip_id) = parse_id(&id) {
                bus.borrow().send(Command::Transcribe { clip_id });
            }
        }
    });
    window.on_cancel_transcription({
        let bus = bus.clone();
        move || bus.borrow().send(Command::CancelTranscription)
    });
    window.on_set_transcribe_model({
        let bus = bus.clone();
        let weak = window.as_weak();
        move |index| {
            // The picker's rows are `WhisperModel::ALL`, in its order; under
            // `$COACH_CUTS_WHISPER_MODEL` it is disabled and holds one row
            // that stands for no choice, so an index it yields is dropped.
            let Some(&model) = usize::try_from(index)
                .ok()
                .filter(|_| whisper_model_override().is_none())
                .and_then(|i| WhisperModel::ALL.get(i))
            else {
                return;
            };
            bus.borrow().send(Command::SetTranscribeModel(model));
            // The button names the download, and this one may not need it.
            if let Some(w) = weak.upgrade() {
                UI.with_borrow(|ui| show_transcription(&w, ui));
            }
        }
    });
    window.on_suggest_tags(|text| {
        let tags = UI.with_borrow(|ui| {
            ui.snapshot.as_ref().map_or_else(Vec::new, |s| {
                tag_suggestions(&tag_summaries(&s.project.clips), &text)
            })
        });
        let tags: Vec<SharedString> = tags.into_iter().map(SharedString::from).collect();
        ModelRc::new(VecModel::from(tags))
    });
    window.on_take_suggestion(|text, tag| take_suggestion(&text, &tag).into());
}

/// A clip or match-event id from the UI, which always sends valid ones: a bad
/// one is logged, as a bug.
fn parse_id(id: &str) -> Option<Uuid> {
    Uuid::parse_str(id)
        .inspect_err(|_| eprintln!("ui: not an id: {id:?}"))
        .ok()
}

/// The Devices popover (R2): listed on a short-lived thread each time it
/// opens, since the first enumeration takes ~250 ms. A row carries its
/// `node_name`, empty for the system default, and picking it saves that one
/// preference.
fn wire_devices(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    window.on_list_devices({
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                w.set_cameras(ModelRc::default());
                w.set_mics(ModelRc::default());
            }
            let weak = weak.clone();
            std::thread::spawn(move || {
                let devices = list_devices();
                let _ = weak.upgrade_in_event_loop(move |w| show_devices(&w, devices));
            });
        }
    });
    let choice = |node_name: SharedString| (!node_name.is_empty()).then(|| node_name.into());
    window.on_choose_camera({
        let bus = bus.clone();
        move |node_name| bus.borrow().send(Command::SetCamera(choice(node_name)))
    });
    window.on_choose_mic({
        let bus = bus.clone();
        move |node_name| bus.borrow().send(Command::SetMic(choice(node_name)))
    });
}

/// Fills the Devices popover's lists: "System default" first, then what was
/// found, with the project's choice checked. A chosen device that isn't
/// connected keeps a row of its own, since the choice is kept (R2).
fn show_devices(w: &AppWindow, devices: Devices) {
    UI.with_borrow(|ui| {
        let Some(prefs) = ui.snapshot.as_ref().map(|s| &s.project.preferences) else {
            return;
        };
        let cameras = devices
            .cameras
            .iter()
            .map(|c| (c.node_name.as_str(), c.label.as_str()));
        let rows = device_rows(cameras, prefs.preferred_camera_id.as_deref(), "camera");
        w.set_cameras(ModelRc::new(VecModel::from(rows)));
        let mics = devices
            .mics
            .iter()
            .map(|m| (m.node_name.as_str(), m.label.as_str()));
        let rows = device_rows(mics, prefs.preferred_mic_id.as_deref(), "microphone");
        w.set_mics(ModelRc::new(VecModel::from(rows)));
    });
}

/// One list's rows, from `(node_name, label)` pairs and the current choice.
fn device_rows<'a>(
    found: impl Iterator<Item = (&'a str, &'a str)>,
    chosen: Option<&str>,
    what: &str,
) -> Vec<DeviceRow> {
    let row = |node_name: &str, label: SharedString, chosen: bool| DeviceRow {
        node_name: node_name.into(),
        label,
        chosen,
    };
    let mut rows = vec![row("", "System default".into(), chosen.is_none())];
    let mut listed = false;
    for (node_name, label) in found {
        let is_chosen = chosen == Some(node_name);
        listed |= is_chosen;
        rows.push(row(node_name, label.into(), is_chosen));
    }
    if let (Some(chosen), false) = (chosen, listed) {
        let label = format!("The chosen {what} (not connected)");
        rows.push(row(chosen, label.into(), true));
    }
    rows
}

/// A callback taking one Slint `float` that sends `command(value)`.
fn cmd(bus: &Rc<RefCell<BusHandle>>, command: impl Fn(f64) -> Command + 'static) -> impl Fn(f32) {
    let bus = bus.clone();
    move |value| bus.borrow().send(command(value.into()))
}

/// Zoom and pan (spec D9). The state lives in [`UiState`]; the window gets
/// a copy to draw from. Everything is recomputed from input: no snapping, no
/// throttle, always clamped (by `zoom_input`, through core's `Zoom`).
fn wire_zoom(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    let notches: Vec<f32> = SNAP_NOTCHES.iter().map(|&n| n as f32).collect();
    window.set_zoom_notches(ModelRc::new(VecModel::from(notches)));

    window.on_place_self_view(|content, cam_aspect, level, avatar| {
        let picture = layout::Rect {
            x: content.x.into(),
            y: content.y.into(),
            w: content.width.into(),
            h: content.height.into(),
        };
        // One placement rule per kind of take (avatar G2): the avatar has a
        // box of its own, smaller than the inset a camera fills, and the
        // camera's lands on `pip_rect_over_picture` as it always has.
        let placed = match avatar {
            true => avatar_self_view_rect(picture, level.into()),
            false => self_view_rect(picture, cam_aspect.into(), level.into()),
        };
        // Nothing to place on before the first layout, or with no picture.
        let Some(r) = placed else {
            return PictureRect::default();
        };
        PictureRect {
            x: r.x as f32,
            y: r.y as f32,
            width: r.w as f32,
            height: r.h as f32,
        }
    });
    window.on_place_picture(|zoom, frame_w, frame_h, area_w, area_h| {
        let zoom = Zoom::new(zoom.scale.into(), zoom.pan_x.into(), zoom.pan_y.into());
        let Some(vp) = Viewport::new(frame_w.into(), frame_h.into(), area_w.into(), area_h.into())
        else {
            return PictureRect::default();
        };
        let r = vp.picture(zoom);
        PictureRect {
            x: r.x as f32,
            y: r.y as f32,
            width: r.width as f32,
            height: r.height as f32,
        }
    });
    window.on_zoom_scroll({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move |x, y, dx, dy, ctrl, shift| {
            let (Some(w), Some(x), Some(y), Some(dx), Some(dy)) =
                (weak.upgrade(), finite(x), finite(y), finite(dx), finite(dy))
            else {
                return;
            };
            update_zoom(&w, &bus, |zoom, vp| {
                zoom_input::scrolled(zoom, vp, (x, y), dx, dy, ctrl, shift)
            });
        }
    });
    window.on_zoom_press(|x, y| {
        if let (Some(x), Some(y)) = (finite(x), finite(y)) {
            UI.with_borrow_mut(|ui| ui.drag = Some(DragPan::new(x, y)));
        }
    });
    window.on_zoom_drag({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move |x, y| {
            let (Some(w), Some(x), Some(y)) = (weak.upgrade(), finite(x), finite(y)) else {
                return;
            };
            let Some((delta, first, zoom)) = UI.with_borrow_mut(|ui| {
                let drag = ui.drag.as_mut()?;
                let first = !drag.is_dragging();
                Some((drag.moved(x, y)?, first, ui.zoom))
            }) else {
                return;
            };
            // Once a drag, and only a drag: a click never gets this far. Not
            // in the H tool (spec H3), where a drag over the picture rings a
            // player; this one is in the letterbox bars, which the tool's
            // touch area doesn't cover.
            if first
                && !w.get_in_highlight_tool()
                && zoom_input::drawing_hint(zoom, w.get_recording())
            {
                show_notice(&w, DRAWING_HINT.into());
            }
            let (dx, dy) = delta;
            update_zoom(&w, &bus, |zoom, vp| zoom_input::panned(zoom, vp, dx, dy));
        }
    });
    window.on_zoom_reset({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move || {
            if let Some(w) = weak.upgrade() {
                update_zoom(&w, &bus, |_, _| Zoom::IDENTITY);
            }
        }
    });
    window.on_zoom_step({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move |delta, over_player, x, y| {
            let (Some(w), Some(delta)) = (weak.upgrade(), finite(delta)) else {
                return;
            };
            let pointer = match (over_player, finite(x), finite(y)) {
                (true, Some(x), Some(y)) => Some((x, y)),
                _ => None,
            };
            update_zoom(&w, &bus, |zoom, vp| {
                zoom_input::stepped(zoom, vp, pointer, delta)
            });
        }
    });
}

/// Applies a gesture's `change` to the zoom, if the player has a size yet,
/// and sends it to the bus, which logs it while recording and ignores it
/// otherwise.
fn update_zoom(
    w: &AppWindow,
    bus: &RefCell<BusHandle>,
    change: impl FnOnce(Zoom, &Viewport) -> Zoom,
) {
    let host_ns = now_ns();
    let Some(vp) = Viewport::new(
        w.get_frame_width().into(),
        w.get_frame_height().into(),
        w.get_player_width().into(),
        w.get_player_height().into(),
    ) else {
        return;
    };
    let zoom = change(UI.with_borrow(|ui| ui.zoom), &vp);
    set_zoom(w, zoom);
    bus.borrow().send(Command::Zoom { host_ns, zoom });
}

/// The one place the zoom changes.
fn set_zoom(w: &AppWindow, zoom: Zoom) {
    UI.with_borrow_mut(|ui| ui.zoom = zoom);
    w.set_zoom(ZoomState {
        scale: zoom.scale as f32,
        pan_x: zoom.pan_x as f32,
        pan_y: zoom.pan_y as f32,
    });
}

fn finite(value: f32) -> Option<f64> {
    let value = f64::from(value);
    value.is_finite().then_some(value)
}

/// Drawing on the picture while recording (Phase 6 spec D2). The window's
/// drawing area is the content rect, so its coordinates already are; the
/// clock is read here, at the input event, as the bus contract requires.
fn wire_drawing(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    window.on_draw_press({
        let weak = window.as_weak();
        move |x, y| {
            let (Some(w), Some(x), Some(y)) = (weak.upgrade(), finite(x), finite(y)) else {
                return;
            };
            let now_ns = now_ns();
            UI.with_borrow_mut(|ui| {
                // The pen as it is now: the whole stroke keeps it.
                let start = InProgress::start(now_ns, x, y, ui.pen.color());
                // A press already draws its dot.
                w.set_drawing_ink(slint_color(ui.pen.color()));
                w.set_drawing_path(start.commands().into());
                ui.drawing = Some(start);
            });
        }
    });
    window.on_draw_move({
        let weak = window.as_weak();
        move |x, y| {
            let (Some(w), Some(x), Some(y)) = (weak.upgrade(), finite(x), finite(y)) else {
                return;
            };
            let now_ns = now_ns();
            // Only when the point was kept: Slint re-parses the path and
            // rebuilds it in Skia on every set.
            let commands = UI.with_borrow_mut(|ui| {
                let ip = ui.drawing.as_mut()?;
                ip.moved(x, y, now_ns).then(|| ip.commands())
            });
            if let Some(commands) = commands {
                w.set_drawing_path(commands.into());
            }
        }
    });
    window.on_draw_release({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move |x, y| {
            let (Some(w), Some(x), Some(y)) = (weak.upgrade(), finite(x), finite(y)) else {
                return;
            };
            let now_ns = now_ns();
            let auto = w.get_auto_clear().then_some(AUTO_CLEAR);
            w.set_drawing_path(SharedString::new());
            // Without a content rect there's nothing to normalize against;
            // the stroke is dropped rather than stored wrong. The player has
            // one whenever a press could reach the drawing area.
            let Some((host_ns, stroke)) = UI.with_borrow_mut(|ui| {
                let ip = ui.drawing.take()?;
                let rect = content_size(&w)?;
                let (host_ns, stroke) = ip.release(x, y, now_ns, rect, auto);
                let expiry = auto.is_some().then(|| host_ns + AUTO_CLEAR_NS);
                ui.live_strokes.push((stroke.clone(), expiry));
                show_strokes(&w, ui, rect);
                Some((host_ns, stroke))
            }) else {
                return;
            };
            bus.borrow().send(Command::Stroke { host_ns, stroke });
        }
    });
    window.on_draw_cancel({
        let weak = window.as_weak();
        move || {
            let Some(w) = weak.upgrade() else { return };
            w.set_drawing_path(SharedString::new());
            UI.with_borrow_mut(|ui| ui.drawing = None);
        }
    });
    window.on_pick_pen({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move |index| {
            let (Some(w), Some(&pen)) = (
                weak.upgrade(),
                usize::try_from(index).ok().and_then(|i| Pen::ALL.get(i)),
            ) else {
                return;
            };
            // New strokes only: every stroke on screen or in the log already
            // carries its own colour.
            set_pen(&w, pen);
            bus.borrow().send(Command::SetPen(pen));
            // In the H tool the swatch row also recolours the selected
            // highlight (spec H2) — but never while recording, where placing
            // a key is the one highlight edit allowed.
            if w.get_in_highlight_tool() && !w.get_recording() {
                if let Some(id) = selected_highlight(&w) {
                    bus.borrow().send(Command::EditHighlight {
                        id,
                        edit: HighlightEdit::Color(pen.color()),
                    });
                }
            }
        }
    });
    window.on_clear_drawings({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move || {
            let host_ns = now_ns();
            let Some(w) = weak.upgrade() else { return };
            // Nothing on screen, nothing to log: a Clear on an empty picture
            // would otherwise put an event in the log that replays as a
            // no-op.
            if clear_drawings(&w) {
                bus.borrow().send(Command::ClearAll { host_ns });
            }
        }
    });
}

/// The H tool and the Highlights panel (spec H3).
///
/// **A key's position is the caller's**, as the bus contract requires: the
/// stream time of the frame that was on screen when the drag began, read here
/// and not by the bus, which by then may be looking at another frame.
fn wire_highlights(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    window.on_highlight_press({
        let (weak, position) = (window.as_weak(), bus.borrow().position_handle().clone());
        move |x, y| {
            let (Some(w), Some(x), Some(y)) = (weak.upgrade(), finite(x), finite(y)) else {
                return;
            };
            // A key sits on one frame, and a running picture has left that
            // frame before the drag ends: the drag places nothing and says
            // so, as a drag outside a recording says drawing needs one.
            let at = if w.get_playing() {
                show_notice(&w, HIGHLIGHT_PAUSE_HINT.into());
                None
            } else {
                shown_source_position(&position)
            };
            UI.with_borrow_mut(|ui| ui.highlight_drag = Some(HighlightDrag { press: (x, y), at }));
        }
    });
    window.on_highlight_release({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move |x, y| {
            let (Some(w), Some(x), Some(y)) = (weak.upgrade(), finite(x), finite(y)) else {
                return;
            };
            let Some(drag) = UI.with_borrow_mut(|ui| ui.highlight_drag.take()) else {
                return;
            };
            // Nothing to place: the picture was playing, or there is no
            // content rect to map the box against.
            let (Some((source_index, source_seconds)), Some((cw, ch))) =
                (drag.at, content_size(&w))
            else {
                return;
            };
            let (project, zoom, pen) = UI.with_borrow(|ui| {
                (
                    ui.snapshot.as_ref().map(|s| s.project.clone()),
                    ui.zoom,
                    ui.pen,
                )
            });
            let Some(project) = project else { return };
            // What is on the picture at the frame the drag started on, which
            // is what the press can have landed on.
            let shapes = highlight_shapes(
                &project.player_highlights,
                source_index,
                source_seconds,
                zoom,
                cw,
                ch,
            );
            // A click, not a box: it selects the ring under it, and on bare
            // pitch changes nothing — Esc is what deselects.
            if (x - drag.press.0).hypot(y - drag.press.1) < HIGHLIGHT_CLICK {
                if let Some(id) = highlight_view::hit_test(&shapes, drag.press.0, drag.press.1) {
                    select_highlight(&w, &project, Some(id));
                }
                return;
            }
            let Some(rect) = highlight_view::drag_rect(drag.press, (x, y), zoom, cw, ch) else {
                return;
            };
            // The ring under the press, else the selection if it is near
            // enough, else a new highlight, which the drag then selects.
            let id = highlight_view::target_for_drag(
                &project,
                &shapes,
                selected_highlight(&w),
                source_index,
                source_seconds,
                drag.press,
            )
            .unwrap_or_else(Uuid::new_v4);
            bus.borrow().send(Command::SetHighlightKey {
                id,
                source_index,
                source_seconds,
                rect,
                color: pen.color(),
            });
            select_highlight(&w, &project, Some(id));
        }
    });
    // The grab was taken away mid-drag (the tool left, the window closing):
    // the box is dropped, as a stroke under the pen is.
    window.on_highlight_cancel(|| UI.with_borrow_mut(|ui| ui.highlight_drag = None));
    window.on_select_highlight({
        let weak = window.as_weak();
        move |id| {
            let (Some(w), Some(id)) = (weak.upgrade(), parse_id(&id)) else {
                return;
            };
            let project = UI.with_borrow(|ui| ui.snapshot.as_ref().map(|s| s.project.clone()));
            if let Some(project) = project {
                select_highlight(&w, &project, Some(id));
            }
        }
    });
    window.on_delete_highlight({
        let bus = bus.clone();
        move |id| {
            if let Some(id) = parse_id(&id) {
                bus.borrow().send(Command::DeleteHighlight(id));
            }
        }
    });
    // "Delete key here": the key at the displayed frame's stream time, which
    // is the number that placed it (spec H2), so no tolerance is involved.
    window.on_delete_highlight_key({
        let (bus, position) = (bus.clone(), bus.borrow().position_handle().clone());
        move |id| {
            let (Some(id), Some((source_index, source_seconds))) =
                (parse_id(&id), shown_source_position(&position))
            else {
                return;
            };
            // The button is only enabled when this highlight has a key on
            // this frame (`has_key_at`, matching the source as well as the
            // time), so the command can't reach another video's key.
            if !UI.with_borrow(|ui| {
                ui.snapshot.as_ref().is_some_and(|s| {
                    highlight_view::has_key_at(&s.project, id, source_index, source_seconds)
                })
            }) {
                return;
            }
            bus.borrow()
                .send(Command::DeleteHighlightKey { id, source_seconds });
        }
    });
    let label = |bus: &Rc<RefCell<BusHandle>>, weak: slint::Weak<AppWindow>| {
        let bus = bus.clone();
        move |text: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            // The highlight the field was editing, which needn't be the
            // selection any more — the clip inspector's rule.
            let editing = w.get_editing_highlight_id();
            let Some(id) = (!editing.is_empty()).then(|| parse_id(&editing)).flatten() else {
                return;
            };
            bus.borrow().send(Command::EditHighlight {
                id,
                edit: HighlightEdit::Label(text.to_string()),
            });
        }
    };
    window.on_commit_highlight_label(label(bus, window.as_weak()));
    window.on_end_highlight_label(label(bus, window.as_weak()));
}

/// The one place the highlight selection changes from Rust: the panel's rows
/// and a drag on a ring. The label field follows it, since only the selected
/// row shows one. (Esc clears the selection in the window itself, where the
/// field is then hidden anyway.)
fn select_highlight(w: &AppWindow, project: &Project, id: Option<Uuid>) {
    let label = id
        .and_then(|id| project.player_highlights.iter().find(|h| h.id == id))
        .map_or("", |h| h.label.as_str());
    w.set_highlight_label(label.into());
    w.set_selected_highlight(id.map(|id| id.to_string()).unwrap_or_default().into());
}

/// The selected highlight's id; `None` for no selection.
fn selected_highlight(w: &AppWindow) -> Option<Uuid> {
    let id = w.get_selected_highlight();
    (!id.is_empty()).then(|| parse_id(&id)).flatten()
}

/// Wipes the live overlay: the drawings on screen and the one under the pen,
/// which is discarded rather than logged (spec D2, macOS parity). Returns
/// whether anything was there to wipe.
fn clear_drawings(w: &AppWindow) -> bool {
    w.set_drawing_path(SharedString::new());
    w.set_live_paths(ModelRc::default());
    UI.with_borrow_mut(|ui| {
        let had_any = !ui.live_strokes.is_empty() || ui.drawing.is_some();
        ui.live_strokes.clear();
        ui.drawing = None;
        had_any
    })
}

/// Rebuilds the window's live `Path` layer from [`UiState::live_strokes`],
/// over a content rect of `rect`. Only on a change (spec D5): Slint re-parses
/// every command string it's given, and a finished stroke's geometry is
/// static.
fn show_strokes(w: &AppWindow, ui: &mut UiState, rect: (f64, f64)) {
    let paths: Vec<LiveStroke> = ui
        .live_strokes
        .iter()
        .map(|(s, _)| LiveStroke {
            commands: path_commands(&s.points, rect.0, rect.1).into(),
            ink: slint_color(s.color),
        })
        .collect();
    ui.paths_rect = rect;
    w.set_live_paths(ModelRc::new(VecModel::from(paths)));
}

/// A live highlight as the window's struct. The ring's commands are already
/// in the content rect's pixels, which is what the `Path` takes.
fn slint_highlight(ring: &Ring) -> LiveHighlight {
    LiveHighlight {
        commands: ring.commands.as_str().into(),
        ink: slint_color(ring.ink),
        label: ring.label.as_str().into(),
        label_ink: slint_color(ring.label_ink),
        label_x: ring.label_x as f32,
        label_y: ring.label_y as f32,
        font_size: ring.font_size as f32,
        label_pad: ring.label_pad as f32,
        pill_height: ring.pill_h as f32,
    }
}

/// Draws the scan picture's scoreboard for `key` and hands it to the window,
/// or clears it for `None`. The one writer of the three properties, so the
/// image and the size it is drawn at can never disagree.
///
/// The board is rasterized in **device** pixels (`key`'s size already is) and
/// drawn at the logical size they came from, so it stays crisp on a scaled
/// display rather than being a logical-size image stretched over it.
fn show_board(w: &AppWindow, ui: &mut UiState, key: Option<BoardKey>) {
    let drawn = key.as_ref().and_then(|(config, state, out_w, out_h)| {
        ui.board_renderer
            .get_or_insert_with(ScoreboardRenderer::new)
            .render(config, *state, f64::from(*out_w), f64::from(*out_h))
    });
    // Slint's scale factor is positive; a stray zero would divide the size
    // below to infinity, so it falls back to drawing the raster one to one.
    let scale = match f64::from(w.window().scale_factor()) {
        scale if scale > 0.0 => scale,
        _ => 1.0,
    };
    let (width, height) = drawn.as_ref().map_or((0.0, 0.0), |image| {
        (
            f64::from(image.width) / scale,
            f64::from(image.height) / scale,
        )
    });
    w.set_scoreboard(drawn.map_or_else(slint::Image::default, |image| {
        slint::Image::from_rgba8_premultiplied(slint::SharedPixelBuffer::clone_from_slice(
            &image.pixels,
            image.width,
            image.height,
        ))
    }));
    w.set_scoreboard_width(width as f32);
    w.set_scoreboard_height(height as f32);
    ui.board_key = key;
}

/// The one place the pen changes, in the window and for the next stroke.
fn set_pen(w: &AppWindow, pen: Pen) {
    UI.with_borrow_mut(|ui| ui.pen = pen);
    let index = Pen::ALL.iter().position(|&p| p == pen).unwrap_or(0);
    w.set_pen_index(index as i32);
}

/// A stored colour as Slint's.
fn slint_color(c: Rgba) -> slint::Color {
    slint::Color::from_argb_f32(c.a as f32, c.r as f32, c.g as f32, c.b as f32)
}

/// The content rect's size, as the window lays it out: the letterboxed
/// picture at 1×, which the drawing area is sized to and strokes are
/// normalized against. `None` before the first layout, or with nothing
/// loaded — there is nothing to normalize against then.
fn content_size(w: &AppWindow) -> Option<(f64, f64)> {
    let (width, height): (f64, f64) = (w.get_content_width().into(), w.get_content_height().into());
    (width > 0.0 && height > 0.0).then_some((width, height))
}

/// Applies a bus event on the UI thread.
fn on_event(w: &AppWindow, event: Event) {
    match event {
        Event::ProjectOpened(snapshot) => {
            UI.with_borrow_mut(|ui| {
                ui.source_index = 0;
                ui.target_abs = None;
                ui.last_secs = 0.0;
                ui.shown_stream_time = None;
                // The bus cancels and clears its queue on an open, and its
                // own event follows; these three are the window's own.
                ui.transcription = Transcription::default();
            });
            set_zoom(w, Zoom::IDENTITY);
            w.set_selected_clip(SharedString::new());
            w.set_tag_filter(SharedString::new());
            // Another project's run doesn't belong in this one's sheet, and
            // no more does another project's block of match events, whose
            // video numbers mean other videos (spec F2).
            w.set_export_run(ModelRc::default());
            w.set_export_finish(SharedString::new());
            w.set_match_editor_paste(SharedString::new());
            w.set_volume(snapshot.project.preferences.scan_volume as f32);
            w.set_project_name(snapshot.project.name.as_str().into());
            show_project(w, snapshot);
        }
        // The name field follows `saved-project-name`, set here, unless
        // it's being typed in.
        Event::ProjectChanged(snapshot) => show_project(w, snapshot),
        Event::Position {
            source_index,
            target_abs,
        } => UI.with_borrow_mut(|ui| {
            // A seek is on its way, or the source itself changed: the frame
            // still on screen is the one *before* it. Dropping its time here
            // — rather than waiting for the next frame to replace it — is
            // what stops a key placed in the gap between a seek settling and
            // the new frame being drawn from carrying the old frame's time.
            // `shown_position` then falls back to the scan position, as every
            // other caller-captured position is taken.
            if target_abs.is_some() || ui.source_index != source_index {
                ui.shown_stream_time = None;
            }
            ui.source_index = source_index;
            ui.target_abs = target_abs;
        }),
        Event::Playing(playing) => w.set_playing(playing),
        Event::ScanSpeed(speed) => w.set_scan_speed(speed as f32),
        Event::Recording(status) => {
            // On every transition, start and stop alike: drawings belong to
            // one recording, and the phase change disables the drawing area
            // under whatever is mid-stroke. Nothing is logged: the recording
            // this would belong to is over.
            let _ = clear_drawings(w);
            UI.with_borrow_mut(|ui| {
                ui.recording_t0 = match status {
                    RecordingStatus::Recording { t0_ns } => Some(t0_ns),
                    _ => None,
                };
                // The next recording's self-view waits for its own frames:
                // `video.rs` accepts none outside a recording.
                if status == RecordingStatus::Idle {
                    ui.self_view_at = None;
                    ui.avatar_take = false;
                    w.set_self_view_avatar(false);
                    w.set_self_view(slint::Image::default());
                }
            });
            w.set_recording_phase(match status {
                RecordingStatus::Idle => RecordingPhase::Idle,
                RecordingStatus::Starting => RecordingPhase::Starting,
                RecordingStatus::Recording { .. } => RecordingPhase::Recording,
            });
            if status == RecordingStatus::Starting {
                w.set_level_seen(false);
                w.set_level(0.0);
                w.set_recording_elapsed(format_hms(0.0).into());
                start_self_view(w);
            }
        }
        // −60…0 dBFS across the bar (R11). A meter shows peaks; the avatar's
        // own curve is `rms_db`'s, over different thresholds.
        Event::Level { peak_db, rms_db } => {
            // `max` then `min`, not `clamp`, which passes a NaN through: here
            // it reads as 0.
            #[allow(clippy::manual_clamp)]
            let fraction = ((peak_db + 60.0) / 60.0).max(0.0).min(1.0);
            w.set_level(fraction as f32);
            w.set_level_seen(true);
            // The live estimator (avatar D1), stepped here rather than in the
            // 30 Hz tick: the level is what moves, and it arrives at 10 Hz.
            UI.with_borrow_mut(|ui| {
                if ui.avatar_take {
                    let target = avatar::level_from_db(rms_db);
                    ui.self_view_level = avatar::smooth(ui.self_view_level, target, LEVEL_DT);
                    w.set_self_view_level(ui.self_view_level as f32);
                }
            });
        }
        // The whole run travels in every event, so the sheet renders what it
        // is handed; the last one -- with nothing left running -- also
        // reports how it went.
        Event::Export(run) => show_export(w, &run),
        // After the operation's `ProjectChanged`, so the clip is in the
        // project; a tag filter that hides it is cleared, so it's listed too.
        // The window re-renders the inspector when the selection changes.
        Event::Select(id) => {
            let filter = w.get_tag_filter();
            let hidden = !filter.is_empty()
                && UI.with_borrow(|ui| {
                    ui.snapshot
                        .as_ref()
                        .and_then(|s| s.project.clips.iter().find(|c| c.id == id))
                        .is_some_and(|c| !c.tags.iter().any(|t| *t == filter.as_str()))
                });
            if hidden {
                w.set_tag_filter(SharedString::new());
            }
            w.set_selected_clip(id.to_string().into());
        }
        // The transport runs over the clip while a preview is open, and the
        // window keys the indicator, the identity zoom and the hidden live
        // stroke layer off `previewing-clip` (P6).
        Event::Preview(previewing) => UI.with_borrow_mut(|ui| {
            let clip = previewing.and_then(|id| {
                let project = &ui.snapshot.as_ref()?.project;
                project.clips.iter().find(|c| c.id == id)
            });
            w.set_previewing_name(clip.map_or("", clip_name).into());
            w.set_previewing_clip(
                previewing
                    .map(|id| id.to_string())
                    .unwrap_or_default()
                    .into(),
            );
            // The duration alone: `tick` is the one writer of
            // `total-seconds`, and picks it up from here.
            ui.preview_duration = clip.map(|c| c.recording_duration);
            // Opening or closing, the frame on screen is about to be one
            // from the other pipeline (see `scan_frame_shown`).
            ui.shown_stream_time = None;
        }),
        // The whole state, every time (spec S5), including how the last run
        // ended: the bus knew that in one `match`, and a window that tried to
        // reconstruct it could only ever guess.
        Event::Transcription(state) => UI.with_borrow_mut(|ui| {
            let t = &mut ui.transcription;
            // A different clip restarts the clock, and so does a different
            // kind of stage — the clock says how long *transcribing* has
            // taken; the same one reporting a new percent keeps it.
            let kind = |s: &TranscriptionState| {
                s.running
                    .map(|(id, stage)| (id, matches!(stage, Stage::Downloading(_))))
            };
            if kind(&t.state) != kind(&state) {
                t.since = state.running.is_some().then(Instant::now);
            }
            t.state = state;
            show_transcription(w, ui);
        }),
        // What the bus's own parse left in the paste box (spec B5): the
        // refused lines, for the coach to fix in place and add again.
        Event::MatchPasteLeftover(text) => w.set_match_editor_paste(text.into()),
        // Never the modal dialog: it would swallow a recording's transport
        // keys.
        Event::Error(e) if e.is_notice() => {
            eprintln!("ui: notice: {e}");
            let text = sentence(&e.to_string());
            // A match refusal while the editor is up goes to its own line as
            // well (spec C5): the status bar renders behind the sheet's
            // scrim, so its copy is only readable once Done is pressed —
            // which is also why it keeps one.
            if w.get_match_editor_open() && matches!(e, bus::UserError::Scoreboard(_)) {
                w.set_match_editor_message(text.clone().into());
            }
            show_notice(w, text);
        }
        Event::Error(e) => show_error(w, &e.to_string()),
    }
}

/// Shows `text` in the error dialog, unless one is already up: one failure
/// can report several, and the first says what went wrong.
fn show_error(w: &AppWindow, text: &str) {
    eprintln!("ui: error: {text}");
    if w.get_error_message().is_empty() {
        w.set_error_message(sentence(text).into());
    }
}

/// Shows `text` on the notice line for [`NOTICE`].
fn show_notice(w: &AppWindow, text: String) {
    w.set_notice(text.into());
    UI.with_borrow_mut(|ui| ui.notice_until = Some(Instant::now() + NOTICE));
}

/// The run in the export sheet (spec E8): a row per target, and one line
/// saying when the whole run finishes. The sheet may well be closed — a run
/// goes on behind it — so the outcome is also reported in the window.
fn show_export(w: &AppWindow, run: &ExportRun) {
    let running = run.is_running();
    let rows: Vec<RunRow> = run.targets.iter().map(run_row).collect();
    w.set_export_run(ModelRc::new(VecModel::from(rows)));
    w.set_exporting(running);
    // While something is still rendering, and only once the rate is steady
    // (E5). `remaining_frames` covers the targets not started yet, so this is
    // the whole run's end, not the current target's.
    let finish = running
        .then(|| run.rate.filter(|rate| *rate > 0.0))
        .flatten()
        .and_then(|rate| finish_at(run.remaining_frames() as f64 / rate));
    w.set_export_finish(finish.unwrap_or_default().into());
    if running {
        return;
    }
    // A run stops at nothing: the first failure is what to say, since a run
    // of one target is still the common case.
    if let Some(why) = run.targets.iter().find_map(|t| match &t.state {
        TargetState::Failed(e) => Some(e.clone()),
        _ => None,
    }) {
        return show_error(w, &format!("export failed: {why}"));
    }
    let written = run
        .targets
        .iter()
        .filter(|t| matches!(t.state, TargetState::Done(_)))
        .count();
    if written > 0 {
        let plural = if written == 1 { "" } else { "s" };
        show_notice(
            w,
            format!("Exported {written} video{plural} to the project's exports folder"),
        );
    }
}

/// One target as the sheet's run list shows it: a bar while it renders, and a
/// word in its place otherwise. A failure says only that here — the dialog
/// carries the reason.
fn run_row(target: &ExportTargetRun) -> RunRow {
    let frames = target.frames.max(1);
    let done = match target.state {
        TargetState::Running(done) => Some(done),
        _ => None,
    };
    RunRow {
        label: target.label.as_str().into(),
        rendering: done.is_some(),
        progress: done.unwrap_or(0) as f32 / frames as f32,
        status: match target.state {
            TargetState::Pending => "Pending".into(),
            TargetState::Running(done) => format!("{}%", done * 100 / frames).into(),
            TargetState::Done(_) => "Done".into(),
            TargetState::Failed(_) => "Failed".into(),
            TargetState::Cancelled => "Cancelled".into(),
        },
    }
}

/// The sidebar, the missing-source card, whether playback is possible and
/// the inspector's column, from `snapshot`.
fn show_project(w: &AppWindow, snapshot: Snapshot) {
    let project = &snapshot.project;
    let missing = |i: usize| snapshot.missing.get(i).copied().unwrap_or(false);
    let rows: Vec<SourceRow> = project
        .source_videos
        .iter()
        .enumerate()
        .map(|(i, source)| SourceRow {
            name: source.display_name.as_str().into(),
            duration: format_hms(source.duration_seconds).into(),
            missing: missing(i),
            referenced: project.source_is_referenced(i),
        })
        .collect();
    let first_missing = (0..rows.len()).find(|&i| missing(i));
    w.set_has_project(true);
    w.set_saved_project_name(project.name.as_str().into());
    w.set_missing_index(first_missing.map_or(-1, |i| i as i32));
    w.set_missing_name(
        first_missing
            .map(|i| project.source_videos[i].display_name.as_str())
            .unwrap_or_default()
            .into(),
    );
    w.set_can_play(!rows.is_empty() && first_missing.is_none());
    w.set_sources(ModelRc::new(VecModel::from(rows)));
    // A selection whose clip is gone (deleted, or undone away) is dropped.
    if selected_id(w).is_some_and(|id| !project.clips.iter().any(|c| c.id == id)) {
        w.set_selected_clip(SharedString::new());
    }
    w.set_clip_count(project.clips.len() as i32);
    // Exactly when the sheet would have a row (spec R1), asked of the sheet's
    // own list rather than restated here: a project of goals and no clips has
    // its reel to export, and one of footage alone has its whole match (W1).
    w.set_can_export(!export_targets(project, None).is_empty());
    show_clips(w, project);
    let tags: Vec<TagRow> = tag_summaries(&project.clips)
        .into_iter()
        .map(|s| {
            let clips = if s.clip_count == 1 { "clip" } else { "clips" };
            TagRow {
                tag: s.tag.into(),
                detail: format!("{} {clips} · {}", s.clip_count, format_hms(s.total_seconds))
                    .into(),
            }
        })
        .collect();
    w.set_tag_rows(ModelRc::new(VecModel::from(tags)));
    show_match(w, project);
    // The editor's rows follow the project like every other list, and are
    // left alone only while the row's field has focus (spec T3).
    if w.get_match_editor_open() && !w.get_match_editor_line_focused() {
        show_match_editor(w, project);
    }
    show_highlights(w, project);
    show_avatar(w, &snapshot);
    UI.with_borrow_mut(|ui| {
        // Rebuilt here and nowhere else: a source add, move, remove or
        // relink moves the offsets a context froze (spec S2), and every one
        // of them arrives as one of these.
        ui.scoreboard = ScoreboardContext::for_project(&snapshot.project);
        ui.snapshot = Some(snapshot);
    });
    // Last, once the snapshot every `pure` callback reads is the new one:
    // the editor's echoes are read from the project, and this is what tells
    // their bindings to ask again. Without it the paste box's own text and
    // picker have not moved, so the echo would stand on the answer it gave
    // before a row was deleted with its `×` — "already tagged", with Add
    // disabled and no way back (spec B3).
    w.set_project_revision(w.get_project_revision() + 1);
    show_clip(w);
}

/// The Devices popover's Inset section (avatar spec G1): the project's avatar
/// image, its name, and whether its file has gone.
///
/// **This is where the image is decoded**, once per change of the file behind
/// it — the corner at the start of a take reads what this left in
/// `UiState::avatar_image`. It runs on the UI thread at every project change,
/// and most of those have nothing to do with the image, so an unchanged file
/// costs a `stat`.
///
/// The pixels are `media::avatar_drawn`'s: the circle the export draws,
/// cover-cropped and premultiplied, so the popover and the corner show what
/// the file will get.
fn show_avatar(w: &AppWindow, snapshot: &Snapshot) {
    let clear = |w: &AppWindow, missing| {
        w.set_avatar_missing(missing);
        w.set_avatar_thumb(slint::Image::default());
        UI.with_borrow_mut(|ui| {
            ui.avatar_shown = None;
            ui.avatar_image = None;
        });
    };
    let Some(name) = snapshot.project.avatar.clone() else {
        w.set_avatar_name(SharedString::new());
        return clear(w, false);
    };
    w.set_avatar_name(name.as_str().into());
    let path = snapshot.folder.join(&name);
    let Ok(meta) = std::fs::metadata(&path) else {
        return clear(w, true);
    };
    w.set_avatar_missing(false);
    let file: AvatarFile = (name, meta.len(), meta.modified().ok());
    if UI.with_borrow(|ui| ui.avatar_shown.as_ref() == Some(&file)) {
        return;
    }
    match avatar_drawn(&path, AVATAR_SIZE) {
        Ok(drawn) => {
            let image = drawn_image(&drawn);
            w.set_avatar_thumb(image.clone());
            UI.with_borrow_mut(|ui| {
                ui.avatar_shown = Some(file);
                ui.avatar_image = Some(image);
            });
        }
        Err(e) => {
            // The pick decoded, so this is a file swapped under the project.
            // The section still names it; there are simply no pixels to show.
            eprintln!("ui: the avatar {} won't decode: {e}", path.display());
            clear(w, false);
        }
    }
}

/// The avatar as drawn, as a Slint image. **Premultiplied**, which is what
/// `media::avatar_drawn` hands over and what the circle's soft edge needs: read
/// as straight alpha it would ring dark.
fn drawn_image(drawn: &video_coach_media::Drawn) -> slint::Image {
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(drawn.size, drawn.size);
    pixels.make_mut_bytes().copy_from_slice(&drawn.rgba);
    slint::Image::from_rgba8_premultiplied(pixels)
}

/// The Clips list: all of `project`'s clips in stored order (C3), or those
/// tagged with the window's `tag-filter` (C8).
fn show_clips(w: &AppWindow, project: &Project) {
    let filter = w.get_tag_filter();
    let clips: Vec<ClipRow> = project
        .clips
        .iter()
        .filter(|c| filter.is_empty() || c.tags.iter().any(|t| *t == filter.as_str()))
        .map(|c| ClipRow {
            id: c.id.to_string().into(),
            name: clip_name(c).into(),
            duration: format_hms(c.recording_duration).into(),
        })
        .collect();
    w.set_clips(ModelRc::new(VecModel::from(clips)));
}

/// A clip's name as the lists and the preview indicator show it.
fn clip_name(clip: &Clip) -> &str {
    if clip.name.is_empty() {
        "Untitled"
    } else {
        &clip.name
    }
}

/// The inspector's fields, from the selected clip (C7), and empty with none.
/// Not while a field is being edited: that would overwrite what's typed.
fn show_clip(w: &AppWindow) {
    // The transcript's *status line* is not a field, so it follows the
    // selection even mid-edit -- deliberately asymmetric with the transcript
    // box right above it, which stays frozen on the clip being edited until
    // the focus-loss commit. The line says what the queue is doing; freezing
    // it would leave the coach watching a clock that had stopped.
    UI.with_borrow(|ui| show_transcription(w, ui));
    if !w.get_editing_clip_id().is_empty() {
        return;
    }
    let selected = selected_id(w);
    UI.with_borrow(|ui| {
        let clip = ui
            .snapshot
            .as_ref()
            .and_then(|s| s.project.clips.iter().find(|c| Some(c.id) == selected));
        w.set_clip_name(clip.map_or("", |c| &c.name).into());
        w.set_clip_tags(clip.map_or_else(String::new, |c| c.tags.join(", ")).into());
        w.set_clip_notes(clip.map_or("", |c| &c.notes).into());
        w.set_clip_transcript(clip.map_or("", |c| &c.transcript).into());
        w.set_clip_show_pip(clip.is_some_and(|c| c.show_pip));
        w.set_clip_avatar(clip.is_some_and(|c| c.inset == Inset::Avatar));
    });
}

/// Fills the transcript row's model picker (Phase 10 S3).
///
/// **Written once, at start-up.** The choice is machine-wide — `state.json`,
/// not the project — and nothing but this picker ever changes it, so unlike
/// the rest of the row it follows no event: the bus's job is to remember it
/// and to run it, not to own it.
///
/// Under `$COACH_CUTS_WHISPER_MODEL` the control is **disabled and shows what
/// that variable points at**, rather than a choice that isn't what runs.
fn show_transcribe_model(w: &AppWindow, model: WhisperModel) {
    let override_path = whisper_model_override();
    let (rows, chosen) = match &override_path {
        Some(path) => (vec![override_name(path)], 0),
        None => (
            WhisperModel::ALL.iter().map(|m| m.label().into()).collect(),
            WhisperModel::ALL
                .iter()
                .position(|m| *m == model)
                .unwrap_or(0),
        ),
    };
    w.set_transcript_models(ModelRc::new(VecModel::from(rows)));
    w.set_transcript_model(chosen as i32);
    w.set_transcript_model_enabled(override_path.is_none());
}

/// What the picker shows for `$COACH_CUTS_WHISPER_MODEL`: the label when it
/// points at a model we ship, else the file's own name — all we can honestly
/// say about it. The control has a fixed width and elides what doesn't fit,
/// so a long name costs the row nothing.
fn override_name(path: &Path) -> SharedString {
    let file = path.file_name().unwrap_or_default().to_string_lossy();
    match WhisperModel::from_file_name(&file) {
        Some(model) => model.label().into(),
        None => file.as_ref().into(),
    }
}

/// The transcript row's state, its line and its button, for the selected
/// clip (spec S5).
fn show_transcription(w: &AppWindow, ui: &UiState) {
    w.set_transcribe_download(transcribe_download(w).into());
    let selected = selected_id(w);
    let (state, status) = ui
        .snapshot
        .as_ref()
        .and_then(|s| s.project.clips.iter().find(|c| Some(c.id) == selected))
        .map_or_else(
            || (TranscriptState::Idle, String::new()),
            |clip| transcript_row(&ui.transcription, clip),
        );
    w.set_transcript_state(state);
    w.set_transcript_status(status.into());
}

/// What the Transcribe button says instead, when the chosen model isn't on
/// disk and is ours to download; empty when it is just "Transcribe" (Phase
/// 11 spec S3).
///
/// **The button is the prompt.** It names the download's size, and pressing
/// it is the consent. A confirmation dialog would be the app's first
/// two-button modal, inside the Esc cascade, to ask a question the button
/// can ask itself. **The model's name is left to the picker above it:** the
/// column is 280 px, and "Download small.en (488 MB) and transcribe" does not
/// fit in it. Looked at on every refresh rather than remembered: a download
/// finishing, or the coach deleting the file, changes the answer, and a
/// `stat` costs nothing beside the redraw.
fn transcribe_download(w: &AppWindow) -> String {
    // Under `$COACH_CUTS_WHISPER_MODEL` the picker's one row stands for no
    // choice at all, and `whisper` downloads nothing there anyway.
    usize::try_from(w.get_transcript_model())
        .ok()
        .and_then(|i| WhisperModel::ALL.get(i).copied())
        .and_then(|m| {
            whisper(m)
                .will_download()
                .map(|fetch| format!("Download {:.0} MB and transcribe", fetch.bytes as f64 / 1e6))
        })
        .unwrap_or_default()
}

/// What the inspector says about `clip`'s transcription.
///
/// **The running line is a clock, not a bare percentage.** whisper's progress
/// callback fires at the top of a loop that advances in ≤30 s chunks and
/// never reports 100, so a 20 s clip reports 0 exactly once: a percentage on
/// its own would sit at 0% for the whole run and look stuck. It joins the
/// clock once it has moved off zero.
///
/// **And a run that wrote nothing says so.** `""` is how a clip says it was
/// never transcribed (spec S4) and whisper returns no segments at all over
/// silence, so an empty result would otherwise leave the inspector looking
/// exactly as it did before the coach pressed the button.
///
/// **A download says so, with its percent and no clock:** unlike whisper's,
/// its percent is honest, and the whisper clock starts when it ends.
fn transcript_row(t: &Transcription, clip: &Clip) -> (TranscriptState, String) {
    if let Some((_, stage)) = t.state.running.filter(|(id, _)| *id == clip.id) {
        let elapsed = || format_hms(t.since.map_or(0.0, |at| at.elapsed().as_secs_f64()));
        let line = match stage {
            Stage::Downloading(done) => format!("Downloading the speech model… {done}%"),
            Stage::Transcribing(0) => format!("Transcribing… {}", elapsed()),
            Stage::Transcribing(p) => format!("Transcribing… {} · {p}%", elapsed()),
        };
        return (TranscriptState::Running, line);
    }
    if t.state.queued.contains(&clip.id) {
        return (TranscriptState::Queued, "Queued".into());
    }
    match t
        .state
        .finished
        .as_ref()
        .filter(|(id, _)| *id == clip.id)
        .map(|(_, how)| how)
    {
        Some(Finish::Failed(why)) => (
            TranscriptState::Failed,
            sentence(&format!("couldn't transcribe: {why}")),
        ),
        // The guard is for the coach typing words in after a silent run: the
        // box is no longer empty, so the line no longer fits.
        Some(Finish::Silent) if clip.transcript.is_empty() => {
            (TranscriptState::Idle, "No speech found".into())
        }
        _ => (TranscriptState::Idle, String::new()),
    }
}

/// The selected clip's id; `None` for no selection.
fn selected_id(w: &AppWindow) -> Option<Uuid> {
    Uuid::parse_str(&w.get_selected_clip()).ok()
}

/// The corner at the start of a take (avatar spec G2).
///
/// In an avatar project it is the project's image as the export draws it —
/// **the copy `show_avatar` already decoded**, never a decode here: the coach
/// has just pressed R, and a decode is a GStreamer pipeline with a ten-second
/// bound on it. (It is also never `slint::Image::load_from_path`, which has no
/// decoder to reach for in this build: slint is built with
/// `default-features = false`.) It rests at level 0 until the first
/// `Event::Level`.
///
/// A take whose image has gone under the project is still an avatar take — the
/// image is the mode (B1) — and simply has no picture to show; the popover and
/// the inspector have already said so (A4).
///
/// A camera take clears the flag and gets no picture here: `video.rs`'s
/// frames fill the corner, and `self_view_rect` at level 1.0 places the inset
/// exactly where it always was.
fn start_self_view(w: &AppWindow) {
    UI.with_borrow_mut(|ui| {
        ui.avatar_take = ui
            .snapshot
            .as_ref()
            .is_some_and(|s| s.project.avatar.is_some());
        w.set_self_view_avatar(ui.avatar_take);
        w.set_self_view(ui.avatar_image.clone().unwrap_or_default());
        ui.self_view_level = if ui.avatar_take { 0.0 } else { 1.0 };
        w.set_self_view_level(ui.self_view_level as f32);
    });
}

/// The self-view drew a frame (`video.rs`).
fn self_view_arrived() {
    UI.with_borrow_mut(|ui| ui.self_view_at = Some(Instant::now()));
}

/// `video.rs` drew a frame from the shared mailbox: which frame is on screen
/// (spec H3, H6), for a highlight key and for "Delete key here".
///
/// **A preview fills the same mailbox**, and its frames are record time
/// within one clip rather than a place in the footage, so they say nothing
/// about where the game video is; `Event::Preview` drops the last scan
/// frame's time as well, so a closed preview leaves nothing stale behind.
fn scan_frame_shown(stream_time: Option<f64>) {
    UI.with_borrow_mut(|ui| {
        ui.shown_stream_time = ui
            .preview_duration
            .is_none()
            .then_some(stream_time)
            .flatten();
    });
}

/// The 30 Hz readout and scrubber update (spec D8): the scrubber's own value
/// while it's dragged, else the outstanding seek's target, else the player's
/// position on the current source. Also the recording's elapsed time (R11),
/// the notice's expiry, the drawings' (Phase 6 D5, which reuses this timer
/// rather than adding one), and whether the self-view is still arriving.
fn tick(w: &AppWindow, position: &PositionHandle, preview: &PreviewPosition) {
    let content = content_size(w);
    UI.with_borrow_mut(|ui| {
        // The quiet timer is the camera's: a frozen picture would lie about
        // it. An avatar take has no frames at all, and a still image lies
        // about nothing -- and is not still anyway, since its size is
        // following the microphone (avatar G2).
        w.set_self_view_shown(
            w.get_recording()
                && (ui.avatar_take
                    || ui
                        .self_view_at
                        .is_some_and(|at| at.elapsed() < SELF_VIEW_QUIET)),
        );
        if let Some(rect) = content {
            let now_ns = now_ns();
            let before = ui.live_strokes.len();
            ui.live_strokes
                .retain(|(_, at)| at.is_none_or(|at| at > now_ns));
            // Something auto-cleared, or the player resized and every
            // stroke moved with it: the commands are in content px.
            let resized = !ui.live_strokes.is_empty() && ui.paths_rect != rect;
            if ui.live_strokes.len() != before || resized {
                show_strokes(w, ui, rect);
            }
        }
        if let Some(t0_ns) = ui.recording_t0 {
            let elapsed = now_ns().saturating_sub(t0_ns) as f64 / 1e9;
            w.set_recording_elapsed(format_hms(elapsed).into());
        }
        // The transcript row's readout is a clock (see `transcript_row`), so
        // it moves with this timer and not with the bus's events -- which,
        // for a clip shorter than one of whisper's chunks, is once.
        if ui.transcription.state.running.is_some() {
            show_transcription(w, ui);
        }
        if ui.notice_until.is_some_and(|until| until <= Instant::now()) {
            ui.notice_until = None;
            w.set_notice(SharedString::new());
        }
        let Some(project) = ui.snapshot.as_ref().map(|s| s.project.clone()) else {
            return;
        };
        // While a preview is open the transport is over the clip, and its
        // position is the pump's frame index rather than a pipeline query
        // (spec P3).
        let total = ui
            .preview_duration
            .unwrap_or_else(|| project.total_source_duration());
        // Where the game video is, unless a preview is open. Queried once a
        // tick: the rings below are placed on the same frame the readout is.
        let scan = ui
            .preview_duration
            .is_none()
            .then(|| scan_abs(ui, &project, position));
        let current = if w.get_scrubbing() {
            f64::from(w.get_position_seconds())
        } else {
            let abs = match scan {
                Some(abs) => abs,
                None => preview.seconds(),
            };
            let abs = abs.clamp(0.0, total.max(0.0));
            w.set_position_seconds(abs as f32);
            abs
        };
        // The one writer of both: the transport's scale is the previewed
        // clip's while a preview is open, and the concat timeline's
        // otherwise, and the readout and the scrubber never disagree about
        // which.
        w.set_total_seconds(total as f32);
        // Tenths while paused, so a frame step shows; whole seconds while
        // playing, so the digits don't flicker.
        let now = if w.get_playing() {
            format_hms(current)
        } else {
            format_hms_tenths(current)
        };
        // The speed above 1x (spec S1). Opening a preview returns it to 1x.
        let speed = match w.get_scan_speed() {
            s if s > 1.0 => format!(" · {s}×"),
            _ => String::new(),
        };
        w.set_readout(format!("{now} / {}{speed}", format_hms(total)).into());
        // The player highlights on the live picture (spec H5). Not while
        // previewing: the preview's texture already carries its rings, and
        // its transport is record time within one clip rather than a place in
        // the footage.
        //
        // **Which frame is on screen**, asked once (spec H3, H6): the
        // displayed frame's own stream time. It is what a key is placed at
        // and what `Decoder::frame_at` picks for it in export, so the ring
        // drawn here is the ring export burns in, and "Delete key here" is
        // offered on exactly the frame whose key it would remove.
        let shown = scan.map(|abs| shown_position(ui, &project, abs));
        let rings = match (shown, content) {
            (Some((source_index, secs)), Some((cw, ch))) => {
                highlight_view::live_highlights(&project, source_index, secs, ui.zoom, cw, ch)
            }
            _ => Vec::new(),
        };
        if ui.highlight_rings != rings {
            w.set_live_highlights(ModelRc::new(VecModel::from(
                rings.iter().map(slint_highlight).collect::<Vec<_>>(),
            )));
            ui.highlight_rings = rings;
        }
        // The scoreboard over the scan picture (spec S3) — a viewing aid, and
        // nothing else: it changes no export and is stored nowhere. It is
        // rasterized by `media`'s own overlay code, so the board the coach
        // scans against is the board the export burns in, and it follows **the
        // displayed frame's** source time exactly as the rings above do.
        //
        // **Dropped while scrubbing**, restored on release: a drag is a burst
        // of flushing seeks, and the board is the one thing on screen that
        // would be redrawn by each of them. A fast scan (J/L) keeps it — the
        // shown frame's own time is as honest at 32× as at 1×, and the clock
        // simply ticks faster.
        let board = match (shown, content, &ui.scoreboard) {
            (Some((source_index, secs)), Some((cw, ch)), Some(scoreboard))
                if !w.get_scrubbing() =>
            {
                let scale = f64::from(w.window().scale_factor());
                scoreboard.state_at(source_index, secs).map(|state| {
                    (
                        scoreboard.config().clone(),
                        state,
                        (cw * scale).round() as u32,
                        (ch * scale).round() as u32,
                    )
                })
            }
            _ => None,
        };
        if ui.board_key != board {
            show_board(w, ui, board);
        }
        // Whether "Delete key here" has a key to remove (spec H3): the
        // selected highlight's, on the frame on screen — the same number a
        // key is placed at, so this is exact equality and not a tolerance.
        w.set_highlight_key_here(match (shown, selected_highlight(w)) {
            (Some((source_index, secs)), Some(id)) => {
                highlight_view::has_key_at(&project, id, source_index, secs)
            }
            _ => false,
        });
        // The Match panel's live line (spec S4), from the same anchor. It
        // **freezes while a preview is open**: the transport is then record
        // time within one clip, and the preview's own scoreboard is already
        // burned into its picture.
        if let (None, Some(scoreboard)) = (ui.preview_duration, &ui.scoreboard) {
            let state = scoreboard.state_at_abs(current);
            let line = match_panel::score_line(scoreboard.config(), state.as_ref());
            w.set_match_score(line.into());
            w.set_match_clock(match_panel::clock_text(state.as_ref()).into());
        }
    });
}

/// Where the game video is on the concat timeline, as the readout has it: an
/// outstanding seek's target — published before its request is issued, so a
/// new source's index is never paired with the old one's offset — else the
/// player's position on the source it holds. The query is the one direct
/// pipeline access outside the bus (D5), and `last_secs` stands in when it
/// fails (mid-load, nothing loaded).
///
/// Not a preview's position, which is record time within one clip: [`tick`]
/// takes that from the preview, and a tag is refused while one is open.
fn scan_abs(ui: &mut UiState, project: &Project, position: &PositionHandle) -> f64 {
    if let Some(target) = ui.target_abs {
        return target;
    }
    if project.source_videos.is_empty() {
        return 0.0;
    }
    if let Some(secs) = position.query_position() {
        ui.last_secs = secs;
    }
    project.abs_seconds(ui.source_index, ui.last_secs)
}
