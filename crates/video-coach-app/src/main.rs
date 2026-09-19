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
use std::time::Duration;

use slint::{ComponentHandle, DataTransfer, ModelRc, SharedString, VecModel};

use video_coach_app::bus::{
    Bus, BusHandle, CaptureKind, Command, Event, RecordingStatus, Snapshot,
};
use video_coach_app::format::{format_hms, sentence};
use video_coach_app::zoom_input::{self, DragPan, Viewport};
use video_coach_core::zoom::{Zoom, SNAP_NOTCHES};
use video_coach_media::{list_devices, now_ns, Devices, PositionHandle, SinkKind};

use pickers::{Pick, Pickers};

slint::include_modules!();

/// How often the readout and scrubber follow the player (spec D8).
const TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);

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
    /// The recording, from the bus's latest `Recording` event: R starts one
    /// only from Idle.
    recording: RecordingStatus,
    /// What each row of the Devices popover's lists stores as the
    /// preference (`None` is the system default).
    camera_choices: Vec<Option<String>>,
    mic_choices: Vec<Option<String>>,
    /// Where zoom changes go: [`set_zoom`] is called from bus events too,
    /// which have no other way to reach the bus.
    bus: Option<Rc<RefCell<BusHandle>>>,
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
            recording: RecordingStatus::Idle,
            camera_choices: Vec::new(),
            mic_choices: Vec::new(),
            bus: None,
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
    let bus = Rc::new(RefCell::new(bus));
    UI.with_borrow_mut(|ui| ui.bus = Some(bus.clone()));
    video::install(&window, bus.clone());
    wire_callbacks(&window, &bus);
    wire_zoom(&window);

    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, TICK, {
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                tick(&w, &position);
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
        let (weak, pickers, send) = (window.as_weak(), pickers, send(bus));
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
    window.on_toggle_recording({
        let send = send(bus);
        move || {
            let (status, zoom) = UI.with_borrow(|ui| (ui.recording, ui.zoom));
            send(match status {
                RecordingStatus::Idle => Command::StartRecording { zoom },
                _ => Command::StopRecording,
            })
        }
    });
    window.on_stop_recording({
        let send = send(bus);
        move || send(Command::StopRecording)
    });
    wire_devices(window, bus);
    // Drag-to-reorder carries the dragged row's index.
    window.on_source_payload(|index| DataTransfer::from(SharedString::from(index.to_string())));
    window.on_payload_source(|data| {
        data.plain_text()
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(-1)
    });
}

/// The Devices popover (R2): listed on a short-lived thread each time it
/// opens, since the first enumeration takes ~250 ms; picking a row saves the
/// pair of choices.
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
    let choose = |bus: &Rc<RefCell<BusHandle>>, camera: bool| {
        let bus = bus.clone();
        move |index: i32| {
            let command = UI.with_borrow(|ui| {
                let prefs = &ui.snapshot.as_ref()?.project.preferences;
                let (mut chosen_camera, mut chosen_mic) = (
                    prefs.preferred_camera_id.clone(),
                    prefs.preferred_mic_id.clone(),
                );
                let (choices, slot) = if camera {
                    (&ui.camera_choices, &mut chosen_camera)
                } else {
                    (&ui.mic_choices, &mut chosen_mic)
                };
                *slot = choices.get(usize::try_from(index).ok()?)?.clone();
                Some(Command::SetDevices {
                    camera: chosen_camera,
                    mic: chosen_mic,
                })
            });
            if let Some(command) = command {
                bus.borrow().send(command);
            }
        }
    };
    window.on_choose_camera(choose(bus, true));
    window.on_choose_mic(choose(bus, false));
}

/// Fills the Devices popover's lists: "System default" first, then what was
/// found, with the project's choice checked. A chosen device that isn't
/// connected keeps a row of its own, since the choice is kept (R2).
fn show_devices(w: &AppWindow, devices: Devices) {
    UI.with_borrow_mut(|ui| {
        let Some(prefs) = ui.snapshot.as_ref().map(|s| &s.project.preferences) else {
            return;
        };
        let cameras = devices
            .cameras
            .iter()
            .map(|c| (c.node_name.as_str(), c.label.as_str()));
        let (rows, choices) = device_rows(cameras, prefs.preferred_camera_id.as_deref(), "camera");
        w.set_cameras(ModelRc::new(VecModel::from(rows)));
        ui.camera_choices = choices;
        let mics = devices
            .mics
            .iter()
            .map(|m| (m.node_name.as_str(), m.label.as_str()));
        let (rows, choices) = device_rows(mics, prefs.preferred_mic_id.as_deref(), "microphone");
        w.set_mics(ModelRc::new(VecModel::from(rows)));
        ui.mic_choices = choices;
    });
}

/// One list's rows and the preference each stands for, from `(node_name,
/// label)` pairs and the current choice.
fn device_rows<'a>(
    found: impl Iterator<Item = (&'a str, &'a str)>,
    chosen: Option<&str>,
    what: &str,
) -> (Vec<DeviceRow>, Vec<Option<String>>) {
    let mut rows = vec![DeviceRow {
        label: "System default".into(),
        chosen: chosen.is_none(),
    }];
    let mut choices = vec![None];
    let mut listed = false;
    for (node_name, label) in found {
        let is_chosen = chosen == Some(node_name);
        listed |= is_chosen;
        rows.push(DeviceRow {
            label: label.into(),
            chosen: is_chosen,
        });
        choices.push(Some(node_name.to_owned()));
    }
    if let (Some(chosen), false) = (chosen, listed) {
        rows.push(DeviceRow {
            label: format!("The chosen {what} (not connected)").into(),
            chosen: true,
        });
        choices.push(Some(chosen.to_owned()));
    }
    (rows, choices)
}

/// A callback taking one Slint `float` that sends `command(value)`.
fn cmd(bus: &Rc<RefCell<BusHandle>>, command: impl Fn(f64) -> Command + 'static) -> impl Fn(f32) {
    let bus = bus.clone();
    move |value| bus.borrow().send(command(value.into()))
}

/// Zoom and pan (spec D9). The state lives in [`UiState`]; the window gets
/// a copy to draw from. Everything is recomputed from input: no snapping, no
/// throttle, always clamped (by `zoom_input`, through core's `Zoom`).
fn wire_zoom(window: &AppWindow) {
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
        let weak = window.as_weak();
        move |x, y, dx, dy, ctrl, shift| {
            let (Some(w), Some(x), Some(y), Some(dx), Some(dy)) =
                (weak.upgrade(), finite(x), finite(y), finite(dx), finite(dy))
            else {
                return;
            };
            update_zoom(&w, |zoom, vp| {
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
        let weak = window.as_weak();
        move |x, y| {
            let (Some(w), Some(x), Some(y)) = (weak.upgrade(), finite(x), finite(y)) else {
                return;
            };
            let delta = UI.with_borrow_mut(|ui| ui.drag.as_mut().and_then(|d| d.moved(x, y)));
            if let Some((dx, dy)) = delta {
                update_zoom(&w, |zoom, vp| zoom_input::panned(zoom, vp, dx, dy));
            }
        }
    });
    window.on_zoom_reset({
        let weak = window.as_weak();
        move || {
            if let Some(w) = weak.upgrade() {
                set_zoom(&w, Zoom::IDENTITY);
            }
        }
    });
    window.on_zoom_step({
        let weak = window.as_weak();
        move |delta, over_player, x, y| {
            let (Some(w), Some(delta)) = (weak.upgrade(), finite(delta)) else {
                return;
            };
            let pointer = match (over_player, finite(x), finite(y)) {
                (true, Some(x), Some(y)) => Some((x, y)),
                _ => None,
            };
            update_zoom(&w, |zoom, vp| zoom_input::stepped(zoom, vp, pointer, delta));
        }
    });
}

/// Applies `change` to the zoom, if the player has a size yet.
fn update_zoom(w: &AppWindow, change: impl FnOnce(Zoom, &Viewport) -> Zoom) {
    let Some(vp) = Viewport::new(
        w.get_frame_width().into(),
        w.get_frame_height().into(),
        w.get_player_width().into(),
        w.get_player_height().into(),
    ) else {
        return;
    };
    let zoom = UI.with_borrow(|ui| ui.zoom);
    set_zoom(w, change(zoom, &vp));
}

/// The one place the zoom changes. Every change goes to the bus, which logs
/// it while recording and ignores it otherwise.
fn set_zoom(w: &AppWindow, zoom: Zoom) {
    let host_ns = now_ns();
    UI.with_borrow_mut(|ui| {
        ui.zoom = zoom;
        if let Some(bus) = &ui.bus {
            bus.borrow().send(Command::Zoom { host_ns, zoom });
        }
    });
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
            w.set_volume(snapshot.project.preferences.scan_volume as f32);
            w.set_project_name(snapshot.project.name.as_str().into());
            show_project(w, snapshot);
        }
        Event::ProjectChanged(snapshot) => {
            // Never overwrite a name that's being typed.
            if !w.get_name_editing() {
                w.set_project_name(snapshot.project.name.as_str().into());
            }
            show_project(w, snapshot);
        }
        Event::Position {
            source_index,
            target_abs,
        } => UI.with_borrow_mut(|ui| {
            ui.source_index = source_index;
            ui.target_abs = target_abs;
        }),
        Event::Playing(playing) => w.set_playing(playing),
        Event::Recording(status) => {
            UI.with_borrow_mut(|ui| ui.recording = status);
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
            let fraction = ((peak_db + 60.0) / 60.0).clamp(0.0, 1.0);
            w.set_level(if fraction.is_nan() {
                0.0
            } else {
                fraction as f32
            });
            w.set_level_seen(true);
        }
        Event::Error(e) => {
            eprintln!("ui: error: {e}");
            // The first error stays up: one failure can report several, and
            // the first says what went wrong.
            if w.get_error_message().is_empty() {
                w.set_error_message(sentence(&e.to_string()).into());
            }
        }
    }
}

/// The sidebar, the missing-source card and whether playback is possible,
/// from `snapshot`.
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
    w.set_total_seconds(project.total_source_duration() as f32);
    w.set_missing_index(first_missing.map_or(-1, |i| i as i32));
    w.set_missing_name(
        first_missing
            .map(|i| project.source_videos[i].display_name.as_str())
            .unwrap_or_default()
            .into(),
    );
    w.set_can_play(!rows.is_empty() && first_missing.is_none());
    w.set_sources(ModelRc::new(VecModel::from(rows)));
    let mut clips: Vec<_> = project.clips.iter().collect();
    clips.sort_by_key(|c| c.sort_index);
    let clips: Vec<ClipRow> = clips
        .into_iter()
        .map(|c| ClipRow {
            name: c.name.as_str().into(),
            duration: format_hms(c.recording_duration).into(),
        })
        .collect();
    w.set_clips(ModelRc::new(VecModel::from(clips)));
    UI.with_borrow_mut(|ui| ui.snapshot = Some(snapshot));
}

/// The 30 Hz readout and scrubber update (spec D8): the scrubber's own value
/// while it's dragged, else the outstanding seek's target, else the player's
/// position on the current source. Also the recording's elapsed time (R11).
fn tick(w: &AppWindow, position: &PositionHandle) {
    UI.with_borrow_mut(|ui| {
        if let RecordingStatus::Recording { t0_ns } = ui.recording {
            let elapsed = now_ns().saturating_sub(t0_ns) as f64 / 1e9;
            w.set_recording_elapsed(format_hms(elapsed).into());
        }
        let Some(project) = ui.snapshot.as_ref().map(|s| s.project.clone()) else {
            return;
        };
        let total = project.total_source_duration();
        let current = if w.get_scrubbing() {
            f64::from(w.get_position_seconds())
        } else {
            let abs = match ui.target_abs {
                Some(target) => target,
                None if project.source_videos.is_empty() => 0.0,
                None => {
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
        w.set_readout(format!("{} / {}", format_hms(current), format_hms(total)).into());
    });
}
