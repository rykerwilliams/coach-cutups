//! The project document.
//!
//! A project is a folder: `project.json` plus a `recordings/` subdirectory of
//! commentary `.mkv` files. Sources are referenced, never copied.
//!
//! **Field-level `#[serde(default)]` is a hazard on an `f64` or a `bool`**: it
//! resolves to `Default::default()`, which is `0.0` and `false`, so applying it
//! per field would silently mute every volume and turn PiP off.
//! `Preferences` puts `default` on the container instead, which fills from its
//! own `Default` impl. On an `Option` or a `Vec` a field-level default is
//! exactly right: `None` and empty are what an older file, written before the
//! field existed, means (spec F2). So a field added to an existing struct is an
//! `Option` or a `Vec` with a field-level default, and a new struct's fields
//! get none — or, since the reason is the rule, any type whose `Default` is
//! what an older file means, which is why `Inset` defaults to `Camera`. And only
//! genuinely optional keys get a default at all: defaulting `clips` would let a
//! truncated `project.json` load as an empty project, after which the next save
//! destroys the user's work.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::CommentaryEvent;
use crate::highlight::PlayerHighlight;
use crate::plan::ScoreboardMode;
use crate::recording::PendingClip;
use crate::scoreboard::{MatchEventRecord, ScoreboardConfig};
use crate::undo::ClipEdit;

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

/// What a clip's inset holds: the webcam it was recorded with, or the
/// project's avatar image.
///
/// It is a clip's own fact, recorded at capture, so a project can hold both
/// kinds and each entry of one export renders what it was made with. Not
/// derived from the recording's lack of a video track: a webcam take whose
/// camera died has none either, and deriving would draw the coach's face over
/// a take they recorded on camera — a *wrong* picture, not a missing one
/// (spec B2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Inset {
    #[default]
    Camera,
    Avatar,
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
    /// Which way the export sheet last carried the scoreboard, or `None` for
    /// its per-target default ([`crate::plan::default_scoreboard_mode`]). v11.
    pub last_export_scoreboard: Option<ScoreboardMode>,
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
            last_export_scoreboard: None,
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

/// [`Project::remove_source`] refused because a clip, a match event or a
/// player highlight still points at the source. Silently retargeting them
/// would produce subtly wrong playback, so the user must delete them first.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("source {index} is still used by a clip, a match event or a highlight")]
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
    /// v10. Which inset this clip was recorded with; `show_pip` still decides
    /// whether one is drawn at all. Read through [`Clip::shows_camera_pip`]
    /// and [`Clip::shows_avatar`], never on its own.
    #[serde(default)]
    pub inset: Inset,
    pub sort_index: i64,

    /// RFC3339, opaque. Nothing reads it — ordering is by `sort_index` — so it
    /// is stored as a string rather than justifying a date dependency in a
    /// crate that otherwise needs none.
    pub created_at: String,

    #[serde(default)]
    pub transcript: String,
}

impl Clip {
    /// Set one field, returning its previous value as the same variant: the
    /// one definition of a field change. Unchanged if the two are equal.
    pub fn set(&mut self, edit: ClipEdit) -> ClipEdit {
        match edit {
            ClipEdit::Name(v) => ClipEdit::Name(std::mem::replace(&mut self.name, v)),
            ClipEdit::Tags(v) => ClipEdit::Tags(std::mem::replace(&mut self.tags, v)),
            ClipEdit::Notes(v) => ClipEdit::Notes(std::mem::replace(&mut self.notes, v)),
            ClipEdit::ShowPip(v) => ClipEdit::ShowPip(std::mem::replace(&mut self.show_pip, v)),
            ClipEdit::Transcript(v) => {
                ClipEdit::Transcript(std::mem::replace(&mut self.transcript, v))
            }
        }
    }

    // The two halves of one decision, written together so they cannot drift:
    // `show_pip × inset` is interpreted here and nowhere else. Each has a
    // caller that wants it positively — the webcam PiP pad and the overlay's
    // avatar — and they are never both true.

    /// The webcam PiP pad carries this clip's recording.
    pub fn shows_camera_pip(&self) -> bool {
        self.show_pip && self.inset == Inset::Camera
    }

    /// The overlay draws the project's avatar for this clip.
    pub fn shows_avatar(&self) -> bool {
        self.show_pip && self.inset == Inset::Avatar
    }

    /// **Some** inset is drawn: what the text bar is fitted to, since it stops
    /// where the inset stands whichever kind it is (`layout::bar_rect`).
    ///
    /// It is `show_pip` alone, and says so rather than or-ing the two above:
    /// every [`Inset`] is drawn somewhere, so a clip that shows one shows one
    /// whatever it picked. Written as `shows_camera_pip() || shows_avatar()`
    /// this would read as a claim about the variants that it cannot make — a
    /// third kind nobody drew would still be `true` here, and the or-form would
    /// suggest it wasn't.
    pub fn shows_inset(&self) -> bool {
        self.show_pip
    }
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
    /// v9. Rings on the footage, not on a clip — see [`crate::highlight`].
    #[serde(default)]
    pub player_highlights: Vec<PlayerHighlight>,
    /// v10. The avatar image's **file name**, relative to the project folder
    /// — never a path. It *is* the mode: `Some` and takes record commentary
    /// only, with that picture for the inset; `None` and they record on
    /// camera, as they always did. There is no second flag to disagree with
    /// it (spec B1).
    #[serde(default)]
    pub avatar: Option<String>,
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
            player_highlights: Vec::new(),
            avatar: None,
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

    /// True if any clip, match event or player highlight points at source
    /// `index`.
    ///
    /// The UI disables a source's remove button on this; [`remove_source`]
    /// re-checks it. macOS counted clips only, so a match event could be left
    /// pointing at the wrong file.
    ///
    /// [`remove_source`]: Project::remove_source
    pub fn source_is_referenced(&self, index: usize) -> bool {
        self.clips.iter().any(|c| c.source_index == index)
            || self.match_events.iter().any(|m| m.source_index == index)
            || self
                .player_highlights
                .iter()
                .any(|h| h.source_index == index)
    }

    /// Remove source `index`, keeping every clip, match event and player
    /// highlight on its own physical file.
    ///
    /// Refuses while the source is referenced. On success every higher
    /// `source_index` — in clips, match events **and** highlights (macOS
    /// remapped clips only) — drops by one, and `current` (the player's
    /// source) goes through the same remap: `None` means the current source
    /// was the one removed.
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
        self.player_highlights
            .iter_mut()
            .for_each(|h| shift(&mut h.source_index));
        Ok(match current.cmp(&index) {
            std::cmp::Ordering::Less => Some(current),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(current - 1),
        })
    }

    /// Move source `from` to position `to` (the `Vec::remove` + `Vec::insert`
    /// convention) and remap clips, match events, player highlights and
    /// `current` through the same permutation, returning the new `current`.
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
        for h in &mut self.player_highlights {
            h.source_index = remap(h.source_index);
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
    /// the floored start, `"2-01:02:05"`. `sort_index` is the clip count,
    /// which is the next position because clips are kept in order (see
    /// [`Project::apply_clip_order`]). `created_at` is passed in because core
    /// has no clock.
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
        let sort_index = self.clips.len() as i64;
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
            inset: if self.avatar.is_some() {
                Inset::Avatar
            } else {
                Inset::Camera
            },
            sort_index,
            created_at,
            transcript: String::new(),
        });
        self.clips.last().expect("just pushed")
    }

    // ------------------------------------------------------------ clip order
    //
    // `clips` is kept in order with `sort_index == position` (Phase 3 spec
    // C3): `store::read` normalizes it and every mutation renumbers. Each order
    // operation is then a `Vec` operation, and export never meets a tie or a
    // gap.

    /// Set every `sort_index` to its position.
    pub(crate) fn renumber(&mut self) {
        for (i, c) in self.clips.iter_mut().enumerate() {
            c.sort_index = i as i64;
        }
    }

    /// The clip ids in list order.
    pub fn clip_order(&self) -> Vec<Uuid> {
        self.clips.iter().map(|c| c.id).collect()
    }

    /// The one order mutation, shared by move, sort, undo and redo.
    ///
    /// Clips named in `order` come first, in that order; ids that no longer
    /// exist (or repeat) are skipped. The rest follow in their current order,
    /// so an order captured before an add or delete still applies.
    pub fn apply_clip_order(&mut self, order: &[Uuid]) {
        let mut rest = std::mem::take(&mut self.clips);
        for id in order {
            if let Some(i) = rest.iter().position(|c| c.id == *id) {
                self.clips.push(rest.remove(i));
            }
        }
        self.clips.append(&mut rest);
        self.renumber();
    }

    /// The order after moving the clip at `from` to position `to` (the
    /// `Vec::remove` + `Vec::insert` convention).
    ///
    /// # Panics
    ///
    /// If `from` or `to` is out of range.
    pub fn moved_order(&self, from: usize, to: usize) -> Vec<Uuid> {
        let mut order = self.clip_order();
        let id = order.remove(from);
        order.insert(to, id);
        order
    }

    /// The order sorted by position in the game: source, then start. Stable,
    /// so clips at the same instant keep their relative order.
    pub fn source_sorted_order(&self) -> Vec<Uuid> {
        let mut clips: Vec<&Clip> = self.clips.iter().collect();
        clips.sort_by(|a, b| {
            a.source_index
                .cmp(&b.source_index)
                .then(a.start_source_seconds.total_cmp(&b.start_source_seconds))
        });
        clips.into_iter().map(|c| c.id).collect()
    }

    /// Remove clip `id` and return it. Its `sort_index` still holds the
    /// position it was removed from, which [`Project::insert_clip`] restores.
    pub fn remove_clip(&mut self, id: Uuid) -> Option<Clip> {
        let i = self.clips.iter().position(|c| c.id == id)?;
        let clip = self.clips.remove(i);
        self.renumber();
        Some(clip)
    }

    /// Insert `clip` at its `sort_index`, clamped to the list. A no-op if a
    /// clip with its id is already present.
    pub fn insert_clip(&mut self, clip: Clip) {
        if self.clips.iter().any(|c| c.id == clip.id) {
            return;
        }
        let at = usize::try_from(clip.sort_index)
            .unwrap_or(0)
            .min(self.clips.len());
        self.clips.insert(at, clip);
        self.renumber();
    }

    /// Set one field of clip `id` (see [`Clip::set`]), or `None` if there is
    /// no such clip.
    pub fn apply_edit(&mut self, id: Uuid, edit: ClipEdit) -> Option<ClipEdit> {
        Some(self.clips.iter_mut().find(|c| c.id == id)?.set(edit))
    }
}
