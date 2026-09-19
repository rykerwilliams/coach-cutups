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

mod video;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::ComponentHandle;

use video_coach_app::bus::{Bus, Command, Event};
use video_coach_media::SinkKind;

slint::include_modules!();

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
    let bus = Rc::new(RefCell::new(bus));
    video::install(&window, bus.clone());

    bus.borrow().send(match std::env::args_os().nth(1) {
        Some(folder) => Command::OpenProject(PathBuf::from(folder)),
        None => Command::RestoreLastProject,
    });

    window.on_toggle_play({
        let bus = bus.clone();
        move || bus.borrow().send(Command::TogglePlay)
    });
    window.on_skip({
        let bus = bus.clone();
        move |delta| {
            bus.borrow().send(Command::Skip {
                delta: delta.into(),
            })
        }
    });

    window.run().expect("run the window");
    // Normally already done by the renderer's teardown; idempotent.
    bus.borrow_mut().shutdown();
}

/// Applies a bus event on the UI thread. The window shows only video so far;
/// the rest is logged.
fn on_event(_window: &AppWindow, event: Event) {
    match event {
        Event::ProjectOpened(project) => eprintln!(
            "ui: opened project {:?} with {} source(s)",
            project.name,
            project.source_videos.len()
        ),
        Event::Error(e) => eprintln!("ui: error: {e}"),
        _ => {}
    }
}
