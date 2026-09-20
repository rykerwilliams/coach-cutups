//! Coach Cuts: the window.
//!
//! ```text
//! cargo run -p video-coach-app [-- <project folder>]
//! ```
//!
//! With a folder, opens (or creates) the project there; otherwise reopens the
//! last project. The UI thread owns only the window: the bus thread owns the
//! project and the player, takes [`Command`]s and answers with [`Event`]s,
//! which are handed to the UI thread with `upgrade_in_event_loop`.

mod pickers;
mod video;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, DataTransfer, ModelRc, SharedString, VecModel};
use uuid::Uuid;

use video_coach_app::bus::{
    Bus, BusHandle, CaptureKind, Command, Event, ExportStatus, RecordingStatus, Snapshot, UserError,
};
use video_coach_app::drawing::{path_commands, InProgress};
use video_coach_app::format::{format_hms, sentence};
use video_coach_app::zoom_input::{self, DragPan, Viewport};
use video_coach_core::project::{Clip, Project};
use video_coach_core::stroke::Stroke;
use video_coach_core::tag::{normalize_tags, tag_suggestions, tag_summaries, take_suggestion};
use video_coach_core::undo::ClipEdit;
use video_coach_core::zoom::{Zoom, SNAP_NOTCHES};
use video_coach_media::{list_devices, now_ns, Devices, PositionHandle, PreviewPosition, SinkKind};

use pickers::{Pick, Pickers};

slint::include_modules!();

/// How often the readout and scrubber follow the player (spec D8).
const TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);
/// How long a notice stays up.
const NOTICE: Duration = Duration::from_secs(6);
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
    /// The drawings on screen, each with the `now_ns()` moment it auto-clears
    /// (Phase 6 spec D3) — the pen-up the logged rule counts from, on the
    /// same clock. Live, "now" only moves forward and a finished stroke is
    /// always fully drawn, so this is the whole of the replay rule for the
    /// live case; `visible_strokes` is for saved clips.
    live_strokes: Vec<(Stroke, Option<u64>)>,
    /// The drawing under the pen, if the coach is mid-stroke.
    drawing: Option<InProgress>,
    /// The content rect the window's `live-paths` were built for: their
    /// commands are in its pixels, so a resize has to rebuild them.
    paths_rect: (f64, f64),
    /// When the notice line clears, if one is up.
    notice_until: Option<Instant>,
    /// The previewed clip's duration while a preview is open. The transport
    /// then runs over the clip rather than the concat timeline (spec P6).
    preview_duration: Option<f64>,
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
            live_strokes: Vec::new(),
            drawing: None,
            paths_rect: (0.0, 0.0),
            notice_until: None,
            preview_duration: None,
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

    let window = AppWindow::new().expect("create the window");

    let weak = window.as_weak();
    let bus = Bus::spawn(
        SinkKind::Gl,
        CaptureKind::Devices,
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
            let pick = Pick::Video {
                title: "Add Source Video",
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
    wire_export(window, bus, pickers);
    wire_preview(window, bus);
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
            if let Some(id) = parse_clip_id(&id) {
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

/// Export (Phase 5 X5): the clip menu's "Export video…" asks where, with the
/// clip's name suggested in the project folder; the transport's Cancel stops
/// it.
fn wire_export(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>, pickers: Pickers) {
    window.on_export_clip({
        let (weak, bus) = (window.as_weak(), bus.clone());
        move |id| {
            let (Some(w), Some(id)) = (weak.upgrade(), parse_clip_id(&id)) else {
                return;
            };
            let suggested = UI.with_borrow(|ui| {
                let s = ui.snapshot.as_ref()?;
                let clip = s.project.clips.iter().find(|c| c.id == id)?;
                Some((s.folder.clone(), export_file_name(&clip.name)))
            });
            let Some((folder, file_name)) = suggested else {
                return;
            };
            let (weak, bus) = (weak.clone(), bus.clone());
            pickers.open(&w, Pick::Export { folder, file_name }, move |mut path| {
                // The portal adds no extension to a typed name. The picker
                // confirmed an overwrite of the name as typed, not of this
                // one, so an existing file is refused.
                if path.extension().is_none() {
                    path.set_extension("mp4");
                    if path.exists() {
                        if let Some(w) = weak.upgrade() {
                            let name = path.file_name().unwrap_or_default().to_string_lossy();
                            let why = format!("{name} already exists");
                            show_error(&w, &UserError::CantExport(why).to_string());
                        }
                        return;
                    }
                }
                bus.borrow().send(Command::ExportClip { id, path });
            });
        }
    });
    window.on_cancel_export({
        let bus = bus.clone();
        move || bus.borrow().send(Command::CancelExport)
    });
}

/// Preview (Phase 7 P6): the inspector's button and the clip menu's "Preview
/// clip" open one, Close and Esc shut it. Opening is explicit — Space goes on
/// meaning "play the game video" until one is open (P5).
fn wire_preview(window: &AppWindow, bus: &Rc<RefCell<BusHandle>>) {
    window.on_open_preview({
        let bus = bus.clone();
        move |id| {
            if let Some(id) = parse_clip_id(&id) {
                bus.borrow().send(Command::OpenPreview(id));
            }
        }
    });
    window.on_close_preview({
        let bus = bus.clone();
        move || bus.borrow().send(Command::ClosePreview)
    });
}

/// `<clip name>.mp4`, with any `/` (which a file name can't hold) replaced.
fn export_file_name(clip_name: &str) -> String {
    let name = clip_name.trim();
    let name = if name.is_empty() { "Untitled" } else { name };
    format!("{}.mp4", name.replace('/', "-"))
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
            };
            let id = parse_clip_id(&id);
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
            if let Some(id) = parse_clip_id(&id) {
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

/// A clip id from the UI, which always sends valid ones: a bad one is
/// logged, as a bug.
fn parse_clip_id(id: &str) -> Option<Uuid> {
    Uuid::parse_str(id)
        .inspect_err(|_| eprintln!("ui: not a clip id: {id:?}"))
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
            let delta = UI.with_borrow_mut(|ui| ui.drag.as_mut().and_then(|d| d.moved(x, y)));
            if let Some((dx, dy)) = delta {
                update_zoom(&w, &bus, |zoom, vp| zoom_input::panned(zoom, vp, dx, dy));
            }
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
            let start = InProgress::start(now_ns(), x, y);
            // A press already draws its dot.
            w.set_drawing_path(start.commands().into());
            UI.with_borrow_mut(|ui| ui.drawing = Some(start));
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
    let paths: Vec<SharedString> = ui
        .live_strokes
        .iter()
        .map(|(s, _)| path_commands(&s.points, rect.0, rect.1).into())
        .collect();
    ui.paths_rect = rect;
    w.set_live_paths(ModelRc::new(VecModel::from(paths)));
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
            });
            set_zoom(w, Zoom::IDENTITY);
            w.set_selected_clip(SharedString::new());
            w.set_tag_filter(SharedString::new());
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
        Event::Export(status) => match status {
            ExportStatus::Running(percent) => w.set_export_progress(percent.into()),
            ExportStatus::Done(path) => {
                w.set_export_progress(-1);
                let name = path.file_name().unwrap_or(path.as_os_str());
                show_notice(w, format!("Exported to {}", name.to_string_lossy()));
            }
            ExportStatus::Cancelled => w.set_export_progress(-1),
            ExportStatus::Failed(e) => {
                w.set_export_progress(-1);
                show_error(w, &format!("export failed: {e}"));
            }
        },
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
    UI.with_borrow_mut(|ui| ui.snapshot = Some(snapshot));
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
        w.set_clip_show_pip(clip.is_some_and(|c| c.show_pip));
    });
}

/// The selected clip's id; `None` for no selection.
fn selected_id(w: &AppWindow) -> Option<Uuid> {
    Uuid::parse_str(&w.get_selected_clip()).ok()
}

/// The 30 Hz readout and scrubber update (spec D8): the scrubber's own value
/// while it's dragged, else the outstanding seek's target, else the player's
/// position on the current source. Also the recording's elapsed time (R11),
/// the notice's expiry, and the drawings' (Phase 6 D5, which reuses this
/// timer rather than adding one).
fn tick(w: &AppWindow, position: &PositionHandle, preview: &PreviewPosition) {
    let content = content_size(w);
    UI.with_borrow_mut(|ui| {
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
            let abs = match (ui.preview_duration.is_some(), ui.target_abs) {
                (true, _) => preview.seconds(),
                (false, Some(target)) => target,
                (false, None) if project.source_videos.is_empty() => 0.0,
                (false, None) => {
                    if let Some(secs) = position.query_position() {
                        ui.last_secs = secs;
                    }
                    project.abs_seconds(ui.source_index, ui.last_secs)
                }
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
        w.set_readout(format!("{} / {}", format_hms(current), format_hms(total)).into());
    });
}
