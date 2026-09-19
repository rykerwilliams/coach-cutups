//! The app's own state file, `$XDG_CONFIG_HOME/coach-cuts/state.json`: the
//! last successfully opened project folder (spec D6). Never stored in a
//! project.
//!
//! Losing this file only costs the user a re-open, so every failure here is
//! logged and otherwise ignored.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const APP_DIR: &str = "coach-cuts";
const FILE: &str = "state.json";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    last_project: Option<PathBuf>,
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
        let path = self.path.as_ref()?;
        let text = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str::<State>(&text) {
            Ok(state) => state.last_project,
            Err(e) => {
                eprintln!("bus: ignoring unreadable {}: {e}", path.display());
                None
            }
        }
    }

    /// Remembers `folder`, or forgets the last project with `None`.
    pub fn set_last_project(&self, folder: Option<&Path>) {
        let Some(path) = &self.path else {
            return;
        };
        if let Err(e) = write(path, folder) {
            eprintln!("bus: could not write {}: {e}", path.display());
        }
    }
}

fn write(path: &Path, folder: Option<&Path>) -> std::io::Result<()> {
    let state = State {
        last_project: folder.map(Path::to_path_buf),
    };
    // Fails only for a non-UTF-8 path, which then simply isn't remembered.
    let text = serde_json::to_string_pretty(&state).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The XDG base-directory rule: `$XDG_CONFIG_HOME` if set to an absolute
/// path, else `$HOME/.config`.
fn config_dir(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    xdg.map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".config"))
        })
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

    #[test]
    fn corrupt_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateFile::in_config_dir(dir.path());
        std::fs::create_dir_all(dir.path().join(APP_DIR)).unwrap();
        std::fs::write(dir.path().join(APP_DIR).join(FILE), "{not json").unwrap();
        assert_eq!(state.last_project(), None);
    }
}
