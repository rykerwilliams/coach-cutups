//! The project document.
//!
//! A project is a folder: `project.json` plus a `recordings/` subdirectory of
//! commentary `.mkv` files. Sources are referenced, never copied.
//!
//! **Field-level `#[serde(default)]` is a hazard**: it resolves to
//! `Default::default()`, which is `0.0` for `f64` and `false` for `bool`, so
//! applying it per field would silently mute every volume and turn PiP off.
//! `Preferences` puts `default` on the container instead, which fills from its
//! own `Default` impl. And only
//! genuinely optional keys get a default at all: defaulting `clips` would let a
//! truncated `project.json` load as an empty project, after which the next save
//! destroys the user's work.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::CommentaryEvent;
use crate::recording::PendingClip;
use crate::scoreboard_config::{MatchEventRecord, ScoreboardConfig};

/// Export frame size. `source` is deliberately absent — it was ill-defined
/// (undefined for a compilation mixing sources of different dimensions) and
/// partly broken in the macOS original.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Resolution {
    R720,
    #[default]
    R1080,
    R2160,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Quality {
    Low,
    #[default]
    Medium,
    High,
}

/// User preferences, persisted with the project.
///
/// `default` is on the **container**, so a missing key is filled from the
/// hand-written `Default` impl below — one copy of the defaults, not two.
/// (Field-level `#[serde(default)]` is the hazard: it resolves to
/// `Default::default()`, i.e. `0.0` and `false`.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    pub scan_volume: f64,
    pub preview_source_volume: f64,
    pub preview_commentary_volume: f64,
    pub last_export_resolution: Resolution,
    pub last_export_quality: Quality,
    /// Stable identifier for the preferred camera: its PipeWire `node.name`.
    /// A hint: if the device is absent at launch the app falls back to the
    /// default **without clearing this**, so the preference is restored if the
    /// device reappears.
    pub preferred_camera_id: Option<String>,
    /// Same semantics as `preferred_camera_id`.
    pub preferred_mic_id: Option<String>,
    pub pip_for_new_recordings: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            scan_volume: 1.0,
            preview_source_volume: 1.0,
            preview_commentary_volume: 1.0,
            last_export_resolution: Resolution::R1080,
            last_export_quality: Quality::Medium,
            preferred_camera_id: None,
            preferred_mic_id: None,
            pip_for_new_recordings: true,
        }
    }
}

/// A referenced source video.
///
/// `duration_seconds` is **the** duration authority. Phase 2's probe writes it
/// back on add and relink; everything else reads it. Two duration sources would
/// let the preview clock and the export clock disagree at EOF for the same
/// clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    /// Relative to the project folder, POSIX `/` separators. May traverse
    /// `..`. Breaks if the user moves the file; Phase 2 owns relink.
    pub relative_path: String,
    pub display_name: String,
    pub duration_seconds: f64,
    /// Width / height after pixel aspect ratio, stored at probe time.
    ///
    /// Read **only** by the aspect gate ([`Project::check_aspect`]); rendering
    /// uses the live caps. Required rather than defaulted: a `0.0` default
    /// would fail every gate comparison, and no file without it was ever
    /// written outside tests (the field amends the unshipped v7).
    pub display_aspect: f64,
}

/// [`Project::remove_source`] refused because a clip or match event still
/// points at the source. Silently retargeting them would produce subtly wrong
/// playback, so the user must delete them first.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("source {index} is still used by a clip or match event")]
pub struct SourceReferenced {
    pub index: usize,
}

/// [`Project::check_aspect`] refused: every source in a project shares one
/// display aspect, and `attempted` differs from the project's `existing` one.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq)]
#[error("aspect {attempted:.4} does not match the project's {existing:.4}")]
pub struct AspectMismatch {
    pub existing: f64,
    pub attempted: f64,
}

/// One tagged moment with its commentary recording.
///
/// A clip **is** a recording: `recording_filename` is not optional, so clips
/// only come into existence once capture has produced a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: Uuid,
    pub name: String,
    pub notes: String,
    pub tags: Vec<String>,

    pub source_index: usize,
    pub start_source_seconds: f64,
    pub recording_duration: f64,

    /// `<uuid>.mkv`, relative to the project's `recordings/` directory.
    pub recording_filename: String,

    pub events: Vec<CommentaryEvent>,
    pub show_pip: bool,
    pub sort_index: i64,

    /// RFC3339, opaque. Nothing reads it — ordering is by `sort_index` — so it
    /// is stored as a string rather than justifying a date dependency in a
    /// crate that otherwise needs none.
    pub created_at: String,

    #[serde(default)]
    pub transcript: String,
}

/// The project document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub format_version: u32,
    pub name: String,
    pub source_videos: Vec<SourceRef>,
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub preferences: Preferences,
    #[serde(default)]
    pub scoreboard: Option<ScoreboardConfig>,
    #[serde(default)]
    pub match_events: Vec<MatchEventRecord>,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Project {
            format_version: crate::store::CURRENT_FORMAT_VERSION,
            name: name.into(),
            source_videos: Vec::new(),
            clips: Vec::new(),
            preferences: Preferences::default(),
            scoreboard: None,
            match_events: Vec::new(),
        }
    }

    /// Sum of every source's duration.
    pub fn total_source_duration(&self) -> f64 {
        self.source_videos.iter().map(|s| s.duration_seconds).sum()
    }

    /// Where source `source_index` starts on the virtual-concat timeline.
    ///
    /// Clamped to the source count, so an index past the end returns the total
    /// duration rather than panicking.
    pub fn cumulative_offset(&self, source_index: usize) -> f64 {
        let end = source_index.min(self.source_videos.len());
        self.source_videos[..end]
            .iter()
            .map(|s| s.duration_seconds)
            .sum()
    }

    /// Absolute time on the virtual-concat timeline.
    ///
    /// The match clock runs on this timeline, which is why it lives with the
    /// format rather than with the player.
    pub fn abs_seconds(&self, source_index: usize, source_seconds: f64) -> f64 {
        self.cumulative_offset(source_index) + source_seconds
    }

    /// Concat time → `(source_index, source_seconds)`. The inverse is
    /// [`Project::abs_seconds`].
    ///
    /// Ported from macOS `Workspace.sourceTime(at:)`:
    ///
    /// - the first source with `abs < cumulative + duration` contains it;
    /// - an instant exactly on a boundary belongs to the **next** source, at 0;
    /// - past the end, it clamps to `(last, last_duration)`;
    /// - with no sources, it is `(0, 0)`.
    ///
    /// Uses the stored durations (the duration authority), so a zero-length
    /// source is never located into.
    pub fn locate(&self, abs_seconds: f64) -> (usize, f64) {
        let Some(last) = self.source_videos.len().checked_sub(1) else {
            return (0, 0.0);
        };
        let mut cumulative = 0.0;
        for (i, src) in self.source_videos.iter().enumerate() {
            let next = cumulative + src.duration_seconds;
            if abs_seconds < next {
                // `f64::max` returns the non-NaN operand, so this never goes
                // negative for an `abs_seconds` before the start.
                return (i, (abs_seconds - cumulative).max(0.0));
            }
            cumulative = next;
        }
        (last, self.source_videos[last].duration_seconds)
    }

    /// True if any clip or match event points at source `index`.
    ///
    /// The UI disables a source's remove button on this; [`remove_source`]
    /// re-checks it. macOS counted clips only, so a match event could be left
    /// pointing at the wrong file.
    ///
    /// [`remove_source`]: Project::remove_source
    pub fn source_is_referenced(&self, index: usize) -> bool {
        self.clips.iter().any(|c| c.source_index == index)
            || self.match_events.iter().any(|m| m.source_index == index)
    }

    /// Remove source `index`, keeping every clip and match event on its own
    /// physical file.
    ///
    /// Refuses while the source is referenced. On success every higher
    /// `source_index` — in clips **and** match events (macOS remapped clips
    /// only) — drops by one, and `current` (the player's source) goes through
    /// the same remap: `None` means the current source was the one removed.
    ///
    /// # Panics
    ///
    /// If `index` is out of range, like `Vec::remove`.
    pub fn remove_source(
        &mut self,
        index: usize,
        current: usize,
    ) -> Result<Option<usize>, SourceReferenced> {
        if self.source_is_referenced(index) {
            return Err(SourceReferenced { index });
        }
        self.source_videos.remove(index);
        let shift = |i: &mut usize| {
            if *i > index {
                *i -= 1;
            }
        };
        self.clips
            .iter_mut()
            .for_each(|c| shift(&mut c.source_index));
        self.match_events
            .iter_mut()
            .for_each(|m| shift(&mut m.source_index));
        Ok(match current.cmp(&index) {
            std::cmp::Ordering::Less => Some(current),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(current - 1),
        })
    }

    /// Move source `from` to position `to` (the `Vec::remove` + `Vec::insert`
    /// convention) and remap clips, match events and `current` through the same
    /// permutation, returning the new `current`.
    ///
    /// A move is always a valid permutation, so there is nothing to refuse.
    /// macOS remapped clips only.
    ///
    /// # Panics
    ///
    /// If `from` or `to` is out of range.
    pub fn move_source(&mut self, from: usize, to: usize, current: usize) -> usize {
        let src = self.source_videos.remove(from);
        self.source_videos.insert(to, src);
        let remap = |i: usize| {
            if i == from {
                to
            } else if from < i && i <= to {
                i - 1
            } else if to <= i && i < from {
                i + 1
            } else {
                i
            }
        };
        for c in &mut self.clips {
            c.source_index = remap(c.source_index);
        }
        for m in &mut self.match_events {
            m.source_index = remap(m.source_index);
        }
        remap(current)
    }

    /// Gate a candidate source's display aspect against the project's.
    ///
    /// The reference is the stored aspect of the first source other than
    /// `excluding` (the one being relinked; `None` on add). With no such source
    /// there is no gate, so a sole source can be relinked to a new aspect —
    /// the intent of macOS's relink gate, which in practice never ran. Stored
    /// aspects are used, so the gate works while other sources are missing.
    ///
    /// Rule (macOS `aspectsMatch`): both aspects > 0 and
    /// `|a − b| / max(a, b) < 0.005`. The 0.5% absorbs phone footage that lands
    /// a pixel off (1920×1078) without admitting a genuinely different aspect.
    /// A NaN aspect fails the `> 0` test and so mismatches.
    pub fn check_aspect(
        &self,
        candidate: f64,
        excluding: Option<usize>,
    ) -> Result<(), AspectMismatch> {
        let reference = self
            .source_videos
            .iter()
            .enumerate()
            .find(|&(i, _)| Some(i) != excluding);
        let Some((_, reference)) = reference else {
            return Ok(());
        };
        let (a, b) = (reference.display_aspect, candidate);
        if a > 0.0 && b > 0.0 && (a - b).abs() / a.max(b) < 0.005 {
            Ok(())
        } else {
            Err(AspectMismatch {
                existing: a,
                attempted: b,
            })
        }
    }

    /// Appends the clip a finished recording produced and returns it.
    ///
    /// The name is macOS's `defaultClipName`: the 1-based source number and
    /// the floored start, `"2-01:02:05"`. `sort_index` is one past the largest
    /// existing one; macOS used the clip count, which repeats an index after a
    /// delete. `created_at` is passed in because core has no clock.
    pub fn add_recorded_clip(
        &mut self,
        pending: PendingClip,
        duration: f64,
        events: Vec<CommentaryEvent>,
        created_at: String,
    ) -> &Clip {
        // `as` truncates toward zero and saturates, so a negative or NaN start
        // names as 00:00:00, like macOS's `max(0, ...)`.
        let total = pending.start_source_seconds as u64;
        let name = format!(
            "{}-{:02}:{:02}:{:02}",
            pending.source_index + 1,
            total / 3600,
            total % 3600 / 60,
            total % 60
        );
        let sort_index = self
            .clips
            .iter()
            .map(|c| c.sort_index)
            .max()
            .map_or(0, |m| m + 1);
        self.clips.push(Clip {
            id: pending.id,
            name,
            notes: String::new(),
            tags: Vec::new(),
            source_index: pending.source_index,
            start_source_seconds: pending.start_source_seconds,
            recording_duration: duration,
            recording_filename: format!("{}.mkv", pending.id),
            events,
            show_pip: self.preferences.pip_for_new_recordings,
            sort_index,
            created_at,
            transcript: String::new(),
        });
        self.clips.last().expect("just pushed")
    }
}
