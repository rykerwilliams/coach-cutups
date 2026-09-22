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
use std::time::{Duration, Instant};

use slint::{ComponentHandle, DataTransfer, Model, ModelRc, SharedString, VecModel};
use uuid::Uuid;

use video_coach_app::bus::{
    export_targets, whisper, whisper_model_override, Bus, BusHandle, CaptureKind, Command, Event,
    ExportRun, ExportTargetRun, Finish, RecordingStatus, Snapshot, Stage, StateFile, TargetState,
    TranscriptionState, WindowSize,
};
use video_coach_app::drawing::{path_commands, InProgress, Pen};
use video_coach_app::format::{finish_at, format_hms, format_hms_tenths, sentence};
use video_coach_app::match_panel::{
    self, parse_hex, parse_minutes, parse_overtime_periods, parse_periods,
};
use video_coach_app::zoom_input::{self, DragPan, Viewport};
use video_coach_core::layout;
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Clip, Project, Quality, Resolution};
use video_coach_core::scoreboard::{
    MatchEventKind, MatchFormat, ScoreboardConfig, ScoreboardContext, TeamConfig,
};
use video_coach_core::stroke::{Rgba, Stroke};
use video_coach_core::tag::{normalize_tags, tag_suggestions, tag_summaries, take_suggestion};
use video_coach_core::undo::ClipEdit;
use video_coach_core::zoom::{Zoom, SNAP_NOTCHES};
use video_coach_media::{
    list_devices, now_ns, Devices, PositionHandle, PreviewPosition, SinkKind, WhisperModel,
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
/// What a drag over the picture says outside a recording, where it can only
/// pan and at 1× visibly does nothing (`zoom_input::drawing_hint`).
const DRAWING_HINT: &str = "Drawing works while recording — press R";
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
    /// The transcription queue (Phase 10 S5).
    transcription: Transcription,
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
            live_strokes: Vec::new(),
            drawing: None,
            pen: Pen::default(),
            paths_rect: (0.0, 0.0),
            notice_until: None,
            preview_duration: None,
            export_targets: Vec::new(),
            scoreboard: None,
            transcription: Transcription::default(),
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
    wire_callbacks(&window, &bus);
    wire_zoom(&window, &bus);
    wire_drawing(&window, &bus);

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
    window.on_scrub_move(cmd(bus, |abs| Command::ScrubMove { abs }));
    window.on_scrub_release(cmd(bus, |abs| Command::ScrubRelease { abs }));
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
    let Some((resolution, quality, rows)) = UI.with_borrow_mut(|ui| {
        let project = &ui.snapshot.as_ref()?.project;
        let targets = export_targets(project, clip);
        let rows: Vec<TargetRow> = targets
            .iter()
            .map(|row| {
                let clips = if row.clips == 1 { "clip" } else { "clips" };
                TargetRow {
                    label: row.label.as_str().into(),
                    detail: format!("{} {clips} · {}", row.clips, format_hms(row.seconds)).into(),
                    // The clip's row is the one that differs: it is ticked
                    // when the sheet was opened on it, and only then.
                    ticked: matches!(row.target, ExportTarget::Clip(_)) == only_clip,
                }
            })
            .collect();
        let prefs = &project.preferences;
        let picked = (
            prefs.last_export_resolution,
            prefs.last_export_quality,
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
    w.set_export_any_ticked(rows.iter().any(|row| row.ticked));
    w.set_export_targets(ModelRc::new(VecModel::from(rows)));
    w.set_export_sheet_open(true);
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
            // Captured here, at the input event, as the bus contract
            // requires, and mapped back through `locate`: reading the index
            // and the offset separately would pair a new source with the old
            // one's offset across a cross-source seek (spec S5).
            let Some((source_index, source_seconds)) = UI.with_borrow_mut(|ui| {
                let project = ui.snapshot.as_ref()?.project.clone();
                let abs = scan_abs(ui, &project, &position);
                (!project.source_videos.is_empty()).then(|| project.locate(abs))
            }) else {
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
    window.on_delete_match_event({
        let bus = bus.clone();
        move |id| {
            if let Some(id) = parse_id(&id) {
                bus.borrow().send(Command::DeleteMatchEvent(id));
            }
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

/// The Match panel's rows, and what its actions are gated on. The live score
/// and clock aren't here: they follow the scan, so the tick renders them.
fn show_match(w: &AppWindow, project: &Project) {
    let rows: Vec<MatchRow> = match_panel::match_rows(project)
        .into_iter()
        .map(|row| MatchRow {
            id: row.id.to_string().into(),
            time: row.time.into(),
            label: row.label.into(),
            role_less: row.role_less,
        })
        .collect();
    w.set_match_rows(ModelRc::new(VecModel::from(rows)));
    w.set_match_configured(project.scoreboard.is_some());
    w.set_match_at_cap(project.start_stops_at_cap());
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

    window.on_place_self_view(|content, cam_aspect| {
        let picture = layout::Rect {
            x: content.x.into(),
            y: content.y.into(),
            w: content.width.into(),
            h: content.height.into(),
        };
        // Nothing to place on before the first layout, or with no picture.
        if !(picture.w > 0.0 && picture.h > 0.0 && cam_aspect > 0.0) {
            return PictureRect::default();
        }
        let r = layout::pip_rect_over_picture(picture, cam_aspect.into());
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
            // Once a drag, and only a drag: a click never gets this far.
            if first && zoom_input::drawing_hint(zoom, w.get_recording()) {
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
                // The bus cancels and clears its queue on an open, and its
                // own event follows; these three are the window's own.
                ui.transcription = Transcription::default();
            });
            set_zoom(w, Zoom::IDENTITY);
            w.set_selected_clip(SharedString::new());
            w.set_tag_filter(SharedString::new());
            // Another project's run doesn't belong in this one's sheet.
            w.set_export_run(ModelRc::default());
            w.set_export_finish(SharedString::new());
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
            ui.source_index = source_index;
            ui.target_abs = target_abs;
        }),
        Event::Playing(playing) => w.set_playing(playing),
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
            }
        }
        // −60…0 dBFS across the bar (R11).
        Event::Level(peak_db) => {
            // `max` then `min`, not `clamp`, which passes a NaN through: here
            // it reads as 0.
            #[allow(clippy::manual_clamp)]
            let fraction = ((peak_db + 60.0) / 60.0).max(0.0).min(1.0);
            w.set_level(fraction as f32);
            w.set_level_seen(true);
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
        // Never the modal dialog: it would swallow a recording's transport
        // keys.
        Event::Error(e) if e.is_notice() => {
            eprintln!("ui: notice: {e}");
            show_notice(w, sentence(&e.to_string()));
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
    UI.with_borrow_mut(|ui| {
        // Rebuilt here and nowhere else: a source add, move, remove or
        // relink moves the offsets a context froze (spec S2), and every one
        // of them arrives as one of these.
        ui.scoreboard = ScoreboardContext::for_project(&snapshot.project);
        ui.snapshot = Some(snapshot);
    });
    show_clip(w);
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

/// The self-view drew a frame (`video.rs`).
fn self_view_arrived() {
    UI.with_borrow_mut(|ui| ui.self_view_at = Some(Instant::now()));
}

/// The 30 Hz readout and scrubber update (spec D8): the scrubber's own value
/// while it's dragged, else the outstanding seek's target, else the player's
/// position on the current source. Also the recording's elapsed time (R11),
/// the notice's expiry, the drawings' (Phase 6 D5, which reuses this timer
/// rather than adding one), and whether the self-view is still arriving.
fn tick(w: &AppWindow, position: &PositionHandle, preview: &PreviewPosition) {
    let content = content_size(w);
    UI.with_borrow_mut(|ui| {
        w.set_self_view_shown(
            w.get_recording()
                && ui
                    .self_view_at
                    .is_some_and(|at| at.elapsed() < SELF_VIEW_QUIET),
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
        let current = if w.get_scrubbing() {
            f64::from(w.get_position_seconds())
        } else {
            let abs = match ui.preview_duration.is_some() {
                true => preview.seconds(),
                false => scan_abs(ui, &project, position),
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
        let now = match w.get_playing() {
            true => format_hms(current),
            false => format_hms_tenths(current),
        };
        w.set_readout(format!("{now} / {}", format_hms(total)).into());
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
