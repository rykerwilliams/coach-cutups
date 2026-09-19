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
    /// Stable identifier for the preferred camera (PipeWire node name or a
    /// `/dev/v4l/by-id` path). A hint: if the device is absent at launch the
    /// app falls back to the default **without clearing this**, so the
    /// preference is restored if the device reappears.
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
}
