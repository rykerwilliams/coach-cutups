//! Project lifecycle (spec D6): read first, then commit folder and project
//! together. macOS set the folder before reading, so after a refused open the
//! next autosave wrote the old project over the file it had just refused.
//!
//! The project folder's own assets live here too: the avatar image, which is
//! copied in, named in `project.json`, and deleted with the project's copy
//! alone (avatar spec A2).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use video_coach_core::project::Project;
use video_coach_core::store::{self, StoreError};
use video_coach_media::decode_still;

use super::{Bus, Event, Open, Snapshot, UserError};

/// Deletes the project's copy of an avatar image. A copy that has already
/// gone is not news: the field is what says there is one, and it is on its
/// way out either way.
fn remove_avatar_file(folder: &Path, name: &str) {
    if let Err(e) = std::fs::remove_file(folder.join(name)) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("bus: could not delete the project's {name}: {e}");
        }
    }
}

impl Bus {
    /// Opens `folder`, creating a project if it exists but has no
    /// `project.json`. Any other failure changes nothing — not the file, not
    /// the open project. A folder that doesn't exist is an error: saving
    /// never creates one.
    pub(super) fn open_project(&mut self, folder: PathBuf) {
        let folder = match std::path::absolute(&folder) {
            Ok(folder) => folder,
            Err(e) => return self.emit(Event::Error(UserError::Io(e.to_string()))),
        };
        if !folder.is_dir() {
            return self.emit(Event::Error(UserError::Io(format!(
                "folder not found: {}",
                folder.display()
            ))));
        }
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
        let Some(folder) = self.files.last_project() else {
            return;
        };
        match store::read(&folder) {
            Ok(project) => self.commit(folder, project),
            Err(e) => {
                eprintln!("bus: not restoring last project {}: {e}", folder.display());
                self.files.set_last_project(None);
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
        self.files.set_last_project(Some(&folder));

        // Nothing of the previous project survives: not its requests, its
        // skip burst, nor its frame.
        self.unload();
        // Undo is in-memory only, so the trash it held is unreachable now
        // (Phase 3 spec C4). That includes the previous project's: its
        // history is being cleared, so its trash can never be restored.
        self.history.clear();
        if let Some(open) = &self.open {
            super::clips::empty_trash(&open.folder);
        }
        super::clips::empty_trash(&folder);
        self.player.set_volume(project.preferences.scan_volume);
        self.current = 0;
        self.open = Some(Open { folder, project });
        self.refresh_missing();
        if let Some(snapshot) = self.snapshot() {
            self.emit(Event::ProjectOpened(snapshot));
        }
        // The transcription queue doesn't survive an open either, for the
        // same reason the history doesn't: it holds ids of another project's
        // clips, and a job left running would write its words into a project
        // that is no longer open (Phase 10 spec S5). **After** the
        // `ProjectOpened` it belongs to, so the UI hears it as this project's
        // state rather than the last one's.
        self.reset_transcription();
        self.ensure_loaded(0.0);
    }

    /// Makes `path` the project's avatar (spec A2): decode, copy, save.
    ///
    /// Decoding first is the whole validation — pixels out of
    /// [`decode_still`] are proof the export will get pixels too — and it
    /// happens **before** anything is copied, so a refused pick leaves the
    /// project exactly as it was. The copy goes through a temp file and a
    /// rename in the same directory, as `store::write` writes `project.json`:
    /// a copy cut short leaves no avatar.
    pub(super) fn set_avatar(&mut self, path: PathBuf) {
        let Some(open) = &self.open else {
            return eprintln!("bus: SetAvatar with no project open");
        };
        if let Err(e) = decode_still(&path) {
            return self.emit(Event::Error(UserError::Avatar(e)));
        }
        // The stored name keeps the original extension, so the file opens in
        // a file manager as what it is; the decoder sniffs regardless, which
        // is why a file without one needs no guess.
        let name = match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => format!("avatar.{}", ext.to_ascii_lowercase()),
            None => "avatar".to_owned(),
        };
        let folder = open.folder.clone();
        let temp = folder.join(".avatar.tmp");
        if let Err(e) =
            std::fs::copy(&path, &temp).and_then(|_| std::fs::rename(&temp, folder.join(&name)))
        {
            let _ = std::fs::remove_file(&temp);
            return self.emit(Event::Error(UserError::Io(format!(
                "the image couldn't be copied into the project: {e}"
            ))));
        }
        let Some(open) = &mut self.open else { return };
        // Exactly the file the project named, never a `avatar.*` glob over a
        // folder the coach can also put files in.
        if let Some(old) = open.project.avatar.take().filter(|old| *old != name) {
            remove_avatar_file(&folder, &old);
        }
        open.project.avatar = Some(name);
        self.project_changed();
    }

    /// Drops the avatar: the field, then the project's **copy** of the image.
    /// The coach's original is theirs.
    pub(super) fn clear_avatar(&mut self) {
        let Some(open) = &mut self.open else {
            return eprintln!("bus: ClearAvatar with no project open");
        };
        let Some(old) = open.project.avatar.take() else {
            return;
        };
        let folder = open.folder.clone();
        remove_avatar_file(&folder, &old);
        self.project_changed();
    }

    /// Saves the open project after a mutation and publishes the snapshot. A
    /// failed save is reported; the in-memory change stands, and the next
    /// successful save carries it.
    pub(super) fn project_changed(&mut self) {
        self.save();
        self.publish_project();
    }

    /// [`Bus::project_changed`]'s save, for a caller with more to do before
    /// it publishes. Returns whether it succeeded.
    pub(super) fn save(&mut self) -> bool {
        let Some(open) = &mut self.open else {
            return false;
        };
        match store::write(&open.folder, &mut open.project) {
            Ok(()) => true,
            Err(e) => {
                self.emit(Event::Error(e.into()));
                false
            }
        }
    }

    /// Publishes the open project, with the cached missing flags.
    pub(super) fn publish_project(&self) {
        if let Some(snapshot) = self.snapshot() {
            self.emit(Event::ProjectChanged(snapshot));
        }
    }

    fn snapshot(&self) -> Option<Snapshot> {
        let open = self.open.as_ref()?;
        Some(Snapshot {
            project: Arc::new(open.project.clone()),
            folder: open.folder.clone(),
            missing: self.missing.clone(),
        })
    }
}
