//! File pickers: the desktop's own dialogs through `rfd`'s XDG portal backend,
//! awaited on the UI thread's event loop (`slint::spawn_local`), parented to
//! the window. Never the blocking dialog: it would stall the event loop and
//! with it the video.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use rfd::AsyncFileDialog;
use slint::ComponentHandle;

use crate::AppWindow;

/// What a picker looks for.
#[derive(Clone, Copy)]
pub enum Pick {
    ProjectFolder,
    /// A video file; `title` names what it's for.
    Video {
        title: &'static str,
    },
}

/// Video extensions offered by default. Both cases: a portal's glob match
/// may be case-sensitive, and cameras write `.MP4`.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "MP4", "mov", "MOV", "m4v", "M4V", "mkv", "MKV", "webm", "WEBM", "avi", "AVI", "mts",
    "MTS",
];

/// Opens one picker at a time, answering with the chosen path.
#[derive(Clone, Default)]
pub struct Pickers {
    /// A picker is up: further requests are ignored rather than stacked.
    busy: Rc<Cell<bool>>,
}

impl Pickers {
    /// Shows the picker and, if the user chooses something, calls `then` with
    /// it on the UI thread. Returns at once.
    pub fn open(&self, window: &AppWindow, pick: Pick, then: impl FnOnce(PathBuf) + 'static) {
        if self.busy.replace(true) {
            return;
        }
        let dialog = AsyncFileDialog::new().set_parent(&window.window().window_handle());
        let busy = self.busy.clone();
        let spawned = slint::spawn_local(async move {
            let chosen = match pick {
                Pick::ProjectFolder => dialog.set_title("Open Project Folder").pick_folder().await,
                Pick::Video { title } => {
                    dialog
                        .set_title(title)
                        .add_filter("Video", VIDEO_EXTENSIONS)
                        .add_filter("All files", &["*"])
                        .pick_file()
                        .await
                }
            };
            busy.set(false);
            if let Some(chosen) = chosen {
                then(chosen.path().to_path_buf());
            }
        });
        if let Err(e) = spawned {
            eprintln!("ui: could not show the file picker: {e}");
            self.busy.set(false);
        }
    }
}
