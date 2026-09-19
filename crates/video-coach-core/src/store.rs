//! Reading and writing `project.json`.
//!
//! The version guard runs against the **raw JSON**, before deserialization, so
//! a macOS-era file produces a clear "made by the macOS version" error rather
//! than a confusing field-level decode failure.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::project::Project;

/// The format version this build writes and is the minimum it reads.
///
/// Numbering continues from the Swift lineage (v1–v6) rather than resetting.
/// Resetting would have made a Swift v1 file — which has no `formatVersion`
/// key at all and so decodes as 1 — indistinguishable from a current file,
/// and the guard would then need to sniff field shapes. One comparison cannot
/// have holes.
pub const CURRENT_FORMAT_VERSION: u32 = 7;

pub const PROJECT_FILENAME: &str = "project.json";
pub const RECORDINGS_DIRNAME: &str = "recordings";

#[derive(thiserror::Error, Debug)]
pub enum StoreError {
    /// No `project.json` in the folder.
    ///
    /// A named variant rather than a folded `Io`, because the "empty folder ⇒
    /// create a project" versus "unreadable `project.json` ⇒ refuse, do not
    /// overwrite" distinction is defined by it.
    #[error("no {PROJECT_FILENAME} in {0}")]
    MissingProjectJson(PathBuf),

    #[error(
        "this project was created by the macOS version of Coach Cuts \
         (format v{found}) and cannot be opened; v{minimum} or later is required"
    )]
    LegacyProject { found: u32, minimum: u32 },

    #[error("project format v{found} is newer than this build supports (v{supported})")]
    TooNew { found: u32, supported: u32 },

    #[error("{PROJECT_FILENAME} is unreadable: {0}")]
    Malformed(String),

    #[error("could not write {PROJECT_FILENAME}: {0}")]
    NotSerializable(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Resolve `formatVersion` from raw JSON.
///
/// Three cases, not two. The obvious `as_u64().unwrap_or(1)` maps a JSON float
/// like `7.0` — which plenty of generic JSON writers emit for integral numbers
/// — to 1, and then tells the user their valid current project was made by the
/// macOS app. A confidently wrong error is worse than a vague one.
fn format_version_of(value: &serde_json::Value) -> Result<u32, StoreError> {
    match value.get("formatVersion") {
        // Absent: predates the field, so it is Swift v1.
        None => Ok(1),
        Some(v) => {
            if let Some(n) = v.as_u64() {
                u32::try_from(n)
                    .map_err(|_| StoreError::Malformed(format!("formatVersion out of range: {n}")))
            } else if let Some(f) = v.as_f64() {
                // Integral float, e.g. 7.0.
                if f.fract() == 0.0 && f >= 0.0 && f <= u32::MAX as f64 {
                    Ok(f as u32)
                } else {
                    Err(StoreError::Malformed(format!(
                        "formatVersion is not an integer: {f}"
                    )))
                }
            } else {
                Err(StoreError::Malformed(format!(
                    "formatVersion is not a number: {v}"
                )))
            }
        }
    }
}

/// Read the project in `project_dir`.
pub fn read(project_dir: &Path) -> Result<Project, StoreError> {
    let path = project_dir.join(PROJECT_FILENAME);
    // Map NotFound on the read itself rather than testing `exists()` first:
    // one syscall, no TOCTOU window, and the distinction is made in one place.
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StoreError::MissingProjectJson(project_dir.to_path_buf()))
        }
        Err(e) => return Err(StoreError::Io(e)),
    };

    let mut value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| StoreError::Malformed(e.to_string()))?;

    // Guard the root shape before looking for a version. `Value::get` returns
    // None for a non-object, which the absent-key rule would read as v1 — and
    // then tell the user their `[]` or `"hello"` was made by the macOS app.
    // That is precisely the confidently-wrong error this function exists to
    // avoid.
    if !value.is_object() {
        return Err(StoreError::Malformed(format!(
            "{PROJECT_FILENAME} root is not an object"
        )));
    }

    let found = format_version_of(&value)?;
    if found < CURRENT_FORMAT_VERSION {
        return Err(StoreError::LegacyProject {
            found,
            minimum: CURRENT_FORMAT_VERSION,
        });
    }
    if found > CURRENT_FORMAT_VERSION {
        return Err(StoreError::TooNew {
            found,
            supported: CURRENT_FORMAT_VERSION,
        });
    }

    // Normalize the version we just validated back into the document. It may
    // have arrived as an integral float (`7.0`), which is a legitimate JSON
    // spelling that a `u32` field cannot deserialize from directly. The root
    // was checked above, so the object access cannot fail.
    value
        .as_object_mut()
        .expect("root shape checked above")
        .insert("formatVersion".into(), serde_json::Value::from(found));

    Project::deserialize(&value).map_err(|e| StoreError::Malformed(e.to_string()))
}

/// Write the project into `project_dir`, creating `recordings/` if needed.
///
/// Stamps `format_version` to current, and writes atomically — an interrupted
/// save must not leave a truncated `project.json`, since that is the file the
/// refuse-to-overwrite rule keys on.
pub fn write(project_dir: &Path, project: &mut Project) -> Result<(), StoreError> {
    project.format_version = CURRENT_FORMAT_VERSION;

    std::fs::create_dir_all(project_dir.join(RECORDINGS_DIRNAME))?;

    let mut text =
        serde_json::to_string_pretty(project).map_err(|e| StoreError::Malformed(e.to_string()))?;
    text.push('\n');

    // Same directory, so the rename is atomic (a cross-filesystem rename
    // would not be).
    let tmp = project_dir.join(".project.json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, project_dir.join(PROJECT_FILENAME))?;
    Ok(())
}
