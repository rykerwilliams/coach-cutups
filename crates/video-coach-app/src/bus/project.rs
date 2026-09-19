//! Project lifecycle (spec D6): read first, then commit folder and project
//! together. macOS set the folder before reading, so after a refused open the
//! next autosave wrote the old project over the file it had just refused.

use std::path::PathBuf;
use std::sync::Arc;

use video_coach_core::project::Project;
use video_coach_core::store::{self, StoreError};

use super::{Bus, Event, Open, UserError};

impl Bus {
    /// Opens `folder`, creating a project if it has no `project.json`. Any
    /// other failure changes nothing — not the file, not the open project.
    pub(super) fn open_project(&mut self, folder: PathBuf) {
        let folder = match std::path::absolute(&folder) {
            Ok(folder) => folder,
            Err(e) => return self.emit(Event::Error(UserError::Io(e.to_string()))),
        };
        match store::read(&folder) {
            Ok(project) => self.commit(folder, project),
            Err(StoreError::MissingProjectJson(_)) => {
                let name = folder
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".into());
                let mut project = Project::new(name);
                match store::write(&folder, &mut project) {
                    Ok(()) => self.commit(folder, project),
                    Err(e) => self.emit(Event::Error(e.into())),
                }
            }
            Err(e) => self.emit(Event::Error(e.into())),
        }
    }

    /// Reopens the remembered project. Opens an **existing** project only:
    /// creating one would make `store::write` recreate a deleted or unmounted
    /// folder. On any failure the path is forgotten and the UI stays in its
    /// no-project state.
    pub(super) fn restore_last_project(&mut self) {
        let Some(folder) = self.state.last_project() else {
            return;
        };
        match store::read(&folder) {
            Ok(project) => self.commit(folder, project),
            Err(e) => {
                eprintln!("bus: not restoring last project {}: {e}", folder.display());
                self.state.set_last_project(None);
            }
        }
    }

    pub(super) fn rename_project(&mut self, name: String) {
        let name = name.trim();
        let Some(open) = &mut self.open else {
            return;
        };
        if name.is_empty() || name == open.project.name {
            return;
        }
        open.project.name = name.to_owned();
        self.project_changed();
    }

    /// Makes `project` in `folder` the open project and resets everything
    /// tied to the previous one.
    fn commit(&mut self, folder: PathBuf, project: Project) {
        // The folder exists now (it was read or just written). Canonical, so
        // relative source paths computed against it resolve the way the
        // kernel resolves `..`.
        let folder = folder.canonicalize().unwrap_or(folder);
        self.state.set_last_project(Some(&folder));

        self.reset_slot();
        self.set_playing(false);
        self.player.set_volume(project.preferences.scan_volume);
        self.current = 0;
        self.loaded = false;
        let snapshot = Arc::new(project.clone());
        self.open = Some(Open { folder, project });
        self.emit(Event::ProjectOpened(snapshot));
        self.check_missing();
        self.ensure_loaded(0.0);
    }

    /// Saves the open project after a mutation and publishes the snapshot. A
    /// failed save is reported; the in-memory change stands, and the next
    /// successful save carries it.
    pub(super) fn project_changed(&mut self) {
        let Some(open) = &mut self.open else {
            return;
        };
        let saved = store::write(&open.folder, &mut open.project);
        let snapshot = Arc::new(open.project.clone());
        if let Err(e) = saved {
            self.emit(Event::Error(e.into()));
        }
        self.emit(Event::ProjectChanged(snapshot));
    }
}
