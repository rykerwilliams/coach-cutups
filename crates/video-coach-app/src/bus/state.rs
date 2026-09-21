//! The app's own state file, `$XDG_CONFIG_HOME/coach-cuts/state.json`: the
//! last successfully opened project folder (spec D6) and which speech model
//! transcription runs (Phase 10 S3). **Neither is a project's.** The model
//! describes how fast this machine is, not the match, and `Preferences` lives
//! in `project.json`, where a new field is a format change that
//! [`store::read`](video_coach_core::store::read)'s exact-version guard would
//! make every existing project unreadable for.
//!
//! Losing this file only costs the user a re-open and a re-pick, so every
//! failure here is logged and otherwise ignored.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use video_coach_media::WhisperModel;

/// The app's own directory under whichever XDG base directory is in play.
pub(super) const APP_DIR: &str = "coach-cuts";
const FILE: &str = "state.json";

/// **Every field defaults**, and a file written by a later version keeps the
/// fields this one doesn't know only insofar as it rewrites the whole
/// document — it doesn't. A lost field costs a re-open or a re-pick.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    #[serde(default)]
    last_project: Option<PathBuf>,
    /// [`WhisperModel::label`], not the enum: the file is hand-readable, and
    /// a label this version doesn't know reads as the default rather than
    /// throwing the whole document away.
    #[serde(default)]
    whisper_model: Option<String>,
}

/// Where the state file lives. `None` when there is no config directory at
/// all (no `$XDG_CONFIG_HOME` and no `$HOME`), in which case nothing is
/// remembered.
#[derive(Debug, Clone)]
pub struct StateFile {
    path: Option<PathBuf>,
}

impl StateFile {
    /// `$XDG_CONFIG_HOME/coach-cuts/state.json`, falling back to
    /// `~/.config/coach-cuts/state.json`.
    pub fn default_location() -> Self {
        StateFile {
            path: config_dir(
                std::env::var_os("XDG_CONFIG_HOME"),
                std::env::var_os("HOME"),
            )
            .map(|dir| dir.join(APP_DIR).join(FILE)),
        }
    }

    /// The state file under `config_dir` instead of the user's, for tests.
    pub fn in_config_dir(config_dir: &Path) -> Self {
        StateFile {
            path: Some(config_dir.join(APP_DIR).join(FILE)),
        }
    }

    /// The remembered project folder, if any. An unreadable file reads as
    /// none.
    pub fn last_project(&self) -> Option<PathBuf> {
        self.read().last_project
    }

    /// Remembers `folder`, or forgets the last project with `None`.
    pub fn set_last_project(&self, folder: Option<&Path>) {
        let mut state = self.read();
        state.last_project = folder.map(Path::to_path_buf);
        self.save(&state);
    }

    /// Which speech model transcription runs (Phase 10 S3). A file that
    /// doesn't say, or says something this version doesn't know, reads as the
    /// default.
    pub fn whisper_model(&self) -> WhisperModel {
        self.read()
            .whisper_model
            .as_deref()
            .and_then(WhisperModel::from_label)
            .unwrap_or_default()
    }

    /// Remembers `model` for every project on this machine.
    pub fn set_whisper_model(&self, model: WhisperModel) {
        let mut state = self.read();
        state.whisper_model = Some(model.label().to_owned());
        self.save(&state);
    }

    /// The file as it stands, defaulted where it is absent or unreadable.
    ///
    /// **Every write reads first**, so a field one setter doesn't know about
    /// survives the other's write: the document is rewritten whole.
    fn read(&self) -> State {
        let Some(path) = self.path.as_ref() else {
            return State::default();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return State::default();
        };
        serde_json::from_str::<State>(&text).unwrap_or_else(|e| {
            eprintln!("bus: ignoring unreadable {}: {e}", path.display());
            State::default()
        })
    }

    fn save(&self, state: &State) {
        let Some(path) = &self.path else {
            return;
        };
        if let Err(e) = write(path, state) {
            eprintln!("bus: could not write {}: {e}", path.display());
        }
    }
}

fn write(path: &Path, state: &State) -> std::io::Result<()> {
    // Fails only for a non-UTF-8 path, which then simply isn't remembered.
    let text = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The XDG base-directory rule: the `$XDG_*_HOME` variable if it is set to an
/// absolute path, else `$HOME/<fallback>`.
fn base_dir(xdg: Option<OsString>, home: Option<OsString>, fallback: &str) -> Option<PathBuf> {
    xdg.map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(fallback))
        })
}

/// `$XDG_CONFIG_HOME`, else `~/.config`: where this file lives.
fn config_dir(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    base_dir(xdg, home, ".config")
}

/// `$XDG_CACHE_HOME`, else `~/.cache`: where the whisper models are looked
/// for (Phase 10 spec S3). Hundreds of megabytes of downloaded weights are a
/// cache, not configuration, and nothing is stored there by this app.
pub(super) fn cache_dir(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    base_dir(xdg, home, ".cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_wins_when_absolute() {
        assert_eq!(
            config_dir(Some("/x/cfg".into()), Some("/home/u".into())),
            Some(PathBuf::from("/x/cfg"))
        );
    }

    #[test]
    fn relative_or_empty_xdg_falls_back_to_home() {
        for xdg in ["", "rel/cfg"] {
            assert_eq!(
                config_dir(Some(xdg.into()), Some("/home/u".into())),
                Some(PathBuf::from("/home/u/.config")),
                "{xdg:?}"
            );
        }
        assert_eq!(
            config_dir(None, Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.config"))
        );
    }

    #[test]
    fn no_config_dir_without_xdg_or_home() {
        assert_eq!(config_dir(None, None), None);
    }

    /// The same rule, a different base directory: the model is a cache.
    #[test]
    fn the_cache_directory_follows_its_own_variable() {
        assert_eq!(
            cache_dir(Some("/x/cache".into()), Some("/home/u".into())),
            Some(PathBuf::from("/x/cache"))
        );
        assert_eq!(
            cache_dir(None, Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.cache"))
        );
        assert_eq!(cache_dir(None, None), None);
    }

    #[test]
    fn remembers_and_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateFile::in_config_dir(dir.path());
        assert_eq!(state.last_project(), None);
        state.set_last_project(Some(Path::new("/p/game")));
        assert_eq!(state.last_project(), Some(PathBuf::from("/p/game")));
        assert!(dir.path().join("coach-cuts/state.json").is_file());
        state.set_last_project(None);
        assert_eq!(state.last_project(), None);
    }

    /// The model is machine-wide and survives a restart, which is the whole
    /// point of it being here rather than in `project.json`.
    #[test]
    fn remembers_the_speech_model() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateFile::in_config_dir(dir.path());
        assert_eq!(state.whisper_model(), WhisperModel::default());
        state.set_whisper_model(WhisperModel::Base);
        // A second handle on the same file: what a relaunch sees.
        assert_eq!(
            StateFile::in_config_dir(dir.path()).whisper_model(),
            WhisperModel::Base
        );
    }

    /// **Neither setter may clobber the other's field.** Each write rewrites
    /// the whole document, so one that didn't read first would forget the
    /// project every time the model changed, and the model every time a
    /// project opened.
    #[test]
    fn the_two_settings_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateFile::in_config_dir(dir.path());
        state.set_last_project(Some(Path::new("/p/game")));
        state.set_whisper_model(WhisperModel::Base);
        assert_eq!(state.last_project(), Some(PathBuf::from("/p/game")));
        state.set_last_project(Some(Path::new("/p/other")));
        assert_eq!(state.whisper_model(), WhisperModel::Base);
    }

    /// A state file from before the picker, and one from a version that knows
    /// a model this one doesn't: both read as the default rather than as a
    /// failure.
    #[test]
    fn an_unknown_or_absent_model_reads_as_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateFile::in_config_dir(dir.path());
        std::fs::create_dir_all(dir.path().join(APP_DIR)).unwrap();
        let file = dir.path().join(APP_DIR).join(FILE);
        for text in [
            r#"{"lastProject":"/p/game"}"#,
            r#"{"lastProject":"/p/game","whisperModel":"medium.en"}"#,
        ] {
            std::fs::write(&file, text).unwrap();
            assert_eq!(state.whisper_model(), WhisperModel::default(), "{text}");
            assert_eq!(
                state.last_project(),
                Some(PathBuf::from("/p/game")),
                "{text}"
            );
        }
    }

    #[test]
    fn corrupt_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateFile::in_config_dir(dir.path());
        std::fs::create_dir_all(dir.path().join(APP_DIR)).unwrap();
        std::fs::write(dir.path().join(APP_DIR).join(FILE), "{not json").unwrap();
        assert_eq!(state.last_project(), None);
    }
}
