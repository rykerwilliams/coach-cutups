//! The source list (spec D7): probe, then gate, then remap. The pure parts —
//! remap, permute, the aspect gate — live in core; this is the I/O around
//! them and the player bookkeeping.
//!
//! Position survives list changes: `current` goes through the same remap as
//! clips and match events, and the player reloads only when the current
//! source itself was removed or relinked. Otherwise only concat offsets move,
//! and a request in flight or pending still lands where it was headed: it
//! names a file, not a concat time. Only the skip coordinator, whose target
//! *is* a concat time, is reset on every change.

use std::path::{Component, Path, PathBuf};

use gstreamer as gst;
use video_coach_core::project::{Project, SourceRef};
use video_coach_media::{probe, Origin};

use super::{Bus, Event, UserError};

/// How far before a source's end a load may land (spec D8's clamp), so a
/// reload never starts at end of stream.
pub(super) const END_MARGIN: f64 = 0.05;

impl Bus {
    pub(super) fn add_source(&mut self, path: PathBuf) {
        let Some(open) = &self.open else {
            return eprintln!("bus: AddSource with no project open");
        };
        match probed_source(&open.folder, &open.project, &path, None) {
            Ok(source) => {
                if let Some(open) = &mut self.open {
                    open.project.source_videos.push(source);
                }
                self.reset_skip();
                self.project_changed();
                self.check_missing();
                // Loads only if nothing was loaded, i.e. the first source.
                self.ensure_loaded(0.0);
            }
            Err(e) => self.emit(Event::Error(e)),
        }
    }

    pub(super) fn remove_source(&mut self, index: usize) {
        let Some(open) = &mut self.open else {
            return;
        };
        if index >= open.project.source_videos.len() {
            return eprintln!("bus: RemoveSource({index}) out of range");
        }
        let removed = open.project.remove_source(index, self.current);
        let remaining = open.project.source_videos.len();
        match removed {
            Err(e) => return self.emit(Event::Error(e.into())),
            Ok(Some(current)) => self.current = current,
            Ok(None) => {
                // The current source is gone: reload the one that took its
                // place (or the new last one) from its start. With none
                // left, `ensure_loaded` unloads.
                self.current = index.min(remaining.saturating_sub(1));
                self.loaded = false;
                self.reset_slot();
            }
        }
        self.reset_skip();
        self.project_changed();
        self.check_missing();
        self.ensure_loaded(0.0);
    }

    pub(super) fn move_source(&mut self, from: usize, to: usize) {
        let Some(open) = &mut self.open else {
            return;
        };
        let len = open.project.source_videos.len();
        if from >= len || to >= len {
            return eprintln!("bus: MoveSource {{ {from} -> {to} }} out of range");
        }
        if from == to {
            return;
        }
        self.current = open.project.move_source(from, to, self.current);
        self.reset_skip();
        self.project_changed();
        self.check_missing();
        self.ensure_loaded(0.0);
    }

    pub(super) fn relink_source(&mut self, index: usize, path: PathBuf) {
        let Some(open) = &self.open else {
            return;
        };
        if index >= open.project.source_videos.len() {
            return eprintln!("bus: RelinkSource({index}) out of range");
        }
        let source = match probed_source(&open.folder, &open.project, &path, Some(index)) {
            Ok(source) => source,
            Err(e) => return self.emit(Event::Error(e)),
        };
        // Taken before the swap: the time the player is at (or heading to)
        // in the old file.
        let resume = self.current_secs();
        if let Some(open) = &mut self.open {
            open.project.source_videos[index] = source;
        }
        if index == self.current {
            self.loaded = false;
            self.reset_slot();
        }
        self.reset_skip();
        self.project_changed();
        self.check_missing();
        self.ensure_loaded(resume);
    }

    /// Re-checks every source path and publishes the result.
    pub(super) fn check_missing(&mut self) {
        let Some(open) = &self.open else {
            return;
        };
        self.missing = open
            .project
            .source_videos
            .iter()
            .map(|s| !open.folder.join(&s.relative_path).exists())
            .collect();
        self.emit(Event::Missing(self.missing.clone()));
    }

    pub(super) fn any_missing(&self) -> bool {
        self.missing.iter().any(|&m| m)
    }

    /// Loads `current` at `secs` unless the player already holds it, and
    /// unloads the player when there is nothing to hold — no sources, or a
    /// missing current source — so no stale frame stays up. Publishes the
    /// position in every case.
    pub(super) fn ensure_loaded(&mut self, secs: f64) {
        let loadable = self.open.as_ref().is_some_and(|open| {
            self.current < open.project.source_videos.len()
                && !self.missing.get(self.current).copied().unwrap_or(true)
        });
        if !loadable {
            self.unload();
        } else if !self.loaded && self.load(self.current, secs, true, Origin::System) {
            return;
        }
        self.publish_position();
    }

    /// Requests `secs` of source `index` (clamped inside it), loading it if
    /// the player holds another file. Returns whether the request was issued.
    pub(super) fn load(&mut self, index: usize, secs: f64, accurate: bool, origin: Origin) -> bool {
        let Some(open) = &self.open else {
            return false;
        };
        let source = &open.project.source_videos[index];
        let path = open.folder.join(&source.relative_path);
        let uri = match gst::glib::filename_to_uri(&path, None) {
            Ok(uri) => uri,
            Err(e) => {
                eprintln!("bus: no URI for {}: {e}", path.display());
                return false;
            }
        };
        let secs = clamp_in_source(secs, source.duration_seconds);
        self.current = index;
        self.loaded = true;
        self.request(&uri, secs, accurate, origin);
        true
    }

    /// Where the player is, or is heading, in `current`, in source seconds.
    pub(super) fn current_secs(&self) -> f64 {
        if !self.loaded {
            return 0.0;
        }
        self.target_secs
            .or_else(|| self.position.query_position())
            .unwrap_or(0.0)
    }
}

/// Probes `path` and gates it against `project`, yielding the `SourceRef` to
/// store. `excluding` is the source being relinked.
fn probed_source(
    folder: &Path,
    project: &Project,
    path: &Path,
    excluding: Option<usize>,
) -> Result<SourceRef, UserError> {
    let probe = probe(path)?;
    project.check_aspect(probe.display_aspect, excluding)?;
    let io = |e: std::io::Error| UserError::Io(format!("{}: {e}", path.display()));
    let canonical = path.canonicalize().map_err(io)?;
    let relative_path = relative_path(&canonical, folder).ok_or_else(|| {
        UserError::Io(format!(
            "{} can't be stored relative to the project folder (non-UTF-8 path?)",
            path.display()
        ))
    })?;
    let display_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| relative_path.clone());
    Ok(SourceRef {
        relative_path,
        display_name,
        duration_seconds: probe.duration_seconds,
        display_aspect: probe.display_aspect,
    })
}

/// `path` relative to `base`, both absolute and canonical, as POSIX `/`
/// components that may start with `..`. `None` if either isn't absolute or a
/// component isn't UTF-8.
///
/// Computed from canonical paths because the kernel resolves `..` physically:
/// from a symlinked project folder, `..` leaves the link's target, not the
/// link.
fn relative_path(path: &Path, base: &Path) -> Option<String> {
    if !path.is_absolute() || !base.is_absolute() {
        return None;
    }
    let path: Vec<Component> = path.components().collect();
    let base: Vec<Component> = base.components().collect();
    let common = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; base.len() - common];
    for c in &path[common..] {
        parts.push(c.as_os_str().to_str()?);
    }
    Some(parts.join("/"))
}

/// `secs` inside a source of `duration`, short of its end by [`END_MARGIN`].
/// NaN-safe: `f64::clamp` would panic on a NaN bound from a bad duration.
fn clamp_in_source(secs: f64, duration: f64) -> f64 {
    if !secs.is_finite() {
        return 0.0;
    }
    #[allow(clippy::manual_clamp, reason = "the bound may be NaN")]
    secs.min(duration - END_MARGIN).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_inside_the_folder() {
        assert_eq!(
            relative_path(Path::new("/p/game/v/a.mp4"), Path::new("/p/game")).as_deref(),
            Some("v/a.mp4")
        );
    }

    #[test]
    fn relative_path_climbs_out_of_the_folder() {
        assert_eq!(
            relative_path(Path::new("/media/cam/a.mp4"), Path::new("/p/game")).as_deref(),
            Some("../../media/cam/a.mp4")
        );
        assert_eq!(
            relative_path(Path::new("/p/a.mp4"), Path::new("/p/game")).as_deref(),
            Some("../a.mp4")
        );
    }

    #[test]
    fn relative_path_round_trips_through_join() {
        let base = Path::new("/home/u/games/2026");
        for p in ["/home/u/videos/x.mp4", "/home/u/games/2026/x.mp4", "/x.mp4"] {
            let rel = relative_path(Path::new(p), base).unwrap();
            let joined: PathBuf =
                base.join(&rel)
                    .components()
                    .fold(PathBuf::new(), |mut acc, c| {
                        match c {
                            Component::ParentDir => {
                                acc.pop();
                            }
                            c => acc.push(c),
                        }
                        acc
                    });
            assert_eq!(joined, Path::new(p), "{rel}");
        }
    }

    #[test]
    fn relative_path_needs_absolute_paths() {
        assert_eq!(relative_path(Path::new("a.mp4"), Path::new("/p")), None);
        assert_eq!(relative_path(Path::new("/a.mp4"), Path::new("p")), None);
    }

    #[test]
    fn clamp_in_source_keeps_short_of_the_end() {
        assert_eq!(clamp_in_source(5.0, 10.0), 5.0);
        assert_eq!(clamp_in_source(10.0, 10.0), 10.0 - END_MARGIN);
        assert_eq!(clamp_in_source(-1.0, 10.0), 0.0);
        assert_eq!(clamp_in_source(f64::NAN, 10.0), 0.0);
        assert_eq!(clamp_in_source(1.0, f64::NAN), 1.0);
        assert_eq!(clamp_in_source(1.0, 0.0), 0.0);
    }
}
