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
pub enum Pick {
    ProjectFolder,
    /// A video file; `title` names what it's for.
    Video {
        title: &'static str,
    },
    /// One or more video files at once: a game is often several camera files.
    Videos {
        title: &'static str,
    },
}

/// Video extensions offered by default. Both cases: a portal's glob match
/// may be case-sensitive, and cameras write `.MP4`.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "MP4", "mov", "MOV", "m4v", "M4V", "mkv", "MKV", "webm", "WEBM", "avi", "AVI", "mts",
    "MTS",
];

/// Opens one picker at a time, answering with each chosen path.
#[derive(Clone, Default)]
pub struct Pickers {
    /// A picker is up: further requests are ignored rather than stacked.
    busy: Rc<Cell<bool>>,
}

impl Pickers {
    /// Shows the picker and calls `then` on the UI thread with each path the
    /// user chose — at most once for a single pick, once per file for
    /// [`Pick::Videos`], in file-name order. Returns at once.
    ///
    /// File-name order because a portal returns a multiple selection in no
    /// promised order, and camera files are named by when they were shot, so
    /// sorting them is what puts a game's halves in sequence.
    pub fn open(&self, window: &AppWindow, pick: Pick, mut then: impl FnMut(PathBuf) + 'static) {
        if self.busy.replace(true) {
            return;
        }
        let dialog = AsyncFileDialog::new().set_parent(&window.window().window_handle());
        let busy = self.busy.clone();
        let videos = |dialog: AsyncFileDialog, title| {
            dialog
                .set_title(title)
                .add_filter("Video", VIDEO_EXTENSIONS)
                .add_filter("All files", &["*"])
        };
        let spawned = slint::spawn_local(async move {
            let chosen: Vec<_> = match pick {
                Pick::ProjectFolder => dialog
                    .set_title("Open Project Folder")
                    .pick_folder()
                    .await
                    .into_iter()
                    .collect(),
                Pick::Video { title } => videos(dialog, title)
                    .pick_file()
                    .await
                    .into_iter()
                    .collect(),
                Pick::Videos { title } => {
                    videos(dialog, title).pick_files().await.unwrap_or_default()
                }
            };
            busy.set(false);
            let mut paths: Vec<PathBuf> = chosen.iter().map(|c| c.path().to_path_buf()).collect();
            paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
            for path in paths {
                then(path);
            }
        });
        if let Err(e) = spawned {
            eprintln!("ui: could not show the file picker: {e}");
            self.busy.set(false);
        }
    }
}
