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
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, DataTransfer, ModelRc, SharedString, VecModel};

use video_coach_app::bus::{Bus, BusHandle, Command, Event};
use video_coach_app::format::{format_hms, sentence};
use video_coach_core::project::Project;
use video_coach_media::{PositionHandle, SinkKind};

use pickers::{Pick, Pickers};

slint::include_modules!();

/// How often the readout and scrubber follow the player (spec D8).
const TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);

/// What the UI thread knows of the bus's state, from its events.
#[derive(Default)]
struct UiState {
    project: Option<Arc<Project>>,
    /// One entry per source, from the last `Missing` event.
    missing: Vec<bool>,
    /// The source the player holds (or is loading).
    source_index: usize,
    /// While a seek is outstanding, where it's headed, concat seconds.
    target_abs: Option<f64>,
    /// The last successful position query, source seconds. Kept when a query
    /// fails (mid-load, nothing loaded).
    last_secs: f64,
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
        Box::new(move |event| {
            let _ = weak.upgrade_in_event_loop(move |w| on_event(&w, event));
        }),
    );
    let position = bus.position_handle().clone();
    let bus = Rc::new(RefCell::new(bus));
    video::install(&window, bus.clone());
    wire_callbacks(&window, &bus);

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

/// Turns the window's callbacks into bus commands. Values that aren't finite
/// are dropped here, before they become commands (BACKLOG #28).
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
        let send = send(bus);
        move |name| send(Command::RenameProject(name.into()))
    });
    window.on_toggle_play({
        let send = send(bus);
        move || send(Command::TogglePlay)
    });
    window.on_skip({
        let send = send(bus);
        move |delta| {
            if let Some(delta) = finite(delta) {
                send(Command::Skip { delta });
            }
        }
    });
    window.on_scrub_move({
        let send = send(bus);
        move |abs| {
            if let Some(abs) = finite(abs) {
                send(Command::ScrubMove { abs });
            }
        }
    });
    window.on_scrub_release({
        let send = send(bus);
        move |abs| {
            if let Some(abs) = finite(abs) {
                send(Command::ScrubRelease { abs });
            }
        }
    });
    window.on_volume_changed({
        let send = send(bus);
        move |value| {
            if let Some(value) = finite(value) {
                send(Command::SetVolume {
                    value,
                    commit: false,
                });
            }
        }
    });
    window.on_volume_released({
        let send = send(bus);
        move |value| {
            if let Some(value) = finite(value) {
                send(Command::SetVolume {
                    value,
                    commit: true,
                });
            }
        }
    });
    // Zoom keys (`zoom-reset`, `zoom-step`) are wired in Task 7.

    // Drag-to-reorder carries the dragged row's index.
    window.on_source_payload(|index| DataTransfer::from(SharedString::from(index.to_string())));
    window.on_payload_source(|data| {
        data.plain_text()
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(-1)
    });
}

fn finite(value: f32) -> Option<f64> {
    let value = f64::from(value);
    value.is_finite().then_some(value)
}

/// Applies a bus event on the UI thread.
fn on_event(w: &AppWindow, event: Event) {
    match event {
        Event::ProjectOpened(project) => {
            UI.with_borrow_mut(|ui| {
                ui.source_index = 0;
                ui.target_abs = None;
                ui.last_secs = 0.0;
            });
            w.set_volume(project.preferences.scan_volume as f32);
            w.set_project_name(project.name.as_str().into());
            show_project(w, project);
        }
        Event::ProjectChanged(project) => {
            // Never overwrite a name that's being typed.
            if !w.get_name_editing() {
                w.set_project_name(project.name.as_str().into());
            }
            show_project(w, project);
        }
        Event::Position {
            source_index,
            target_abs,
        } => UI.with_borrow_mut(|ui| {
            ui.source_index = source_index;
            ui.target_abs = target_abs;
        }),
        Event::Playing(playing) => w.set_playing(playing),
        Event::Missing(missing) => {
            UI.with_borrow_mut(|ui| ui.missing = missing);
            show_sources(w);
        }
        Event::Error(e) => {
            eprintln!("ui: error: {e}");
            w.set_error_message(sentence(&e.to_string()).into());
        }
    }
}

fn show_project(w: &AppWindow, project: Arc<Project>) {
    w.set_has_project(true);
    w.set_total_seconds(project.total_source_duration() as f32);
    UI.with_borrow_mut(|ui| ui.project = Some(project));
    show_sources(w);
}

/// The Sources list, the missing-source card and whether playback is
/// possible, from the latest snapshot and `Missing` event.
fn show_sources(w: &AppWindow) {
    UI.with_borrow(|ui| {
        let Some(project) = &ui.project else { return };
        let missing = |i: usize| ui.missing.get(i).copied().unwrap_or(false);
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
        w.set_missing_index(first_missing.map_or(-1, |i| i as i32));
        w.set_missing_name(
            first_missing
                .map(|i| project.source_videos[i].display_name.as_str())
                .unwrap_or_default()
                .into(),
        );
        w.set_can_play(!rows.is_empty() && first_missing.is_none());
        w.set_sources(ModelRc::new(VecModel::from(rows)));
    });
}

/// The 30 Hz readout and scrubber update (spec D8): the scrubber's own value
/// while it's dragged, else the outstanding seek's target, else the player's
/// position on the current source.
fn tick(w: &AppWindow, position: &PositionHandle) {
    UI.with_borrow_mut(|ui| {
        let Some(project) = &ui.project else { return };
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
