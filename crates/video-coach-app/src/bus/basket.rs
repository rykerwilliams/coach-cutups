//! The basket (basket spec E, H, C, O, V): one film whose pieces come from
//! several matches, gathered while the coach works and started when they are
//! done.
//!
//! **A piece is a reference, `(project folder, clip id)`, resolved at Start.**
//! Nothing of a project is copied in: a clip the coach fixes after adding it is
//! exported as it now stands, and a piece whose project has gone is refused by
//! name rather than silently dropped (spec E1).
//!
//! **It lives in its own file, `$XDG_CONFIG_HOME/coach-cuts/basket.json`, and
//! not in `state.json`** — [`StateFile::read`](super::state) discards that
//! whole document on any parse error and every setter rewrites it, so one
//! basket value a build can't read would take the last project, the pen and the
//! speech model with it (spec H1). The same discipline applies inside this
//! file: the two pickers are **string labels**, and one this build doesn't know
//! reads as the default rather than throwing the pieces away.
//!
//! **One resolver, two callers.** Showing the basket and starting it resolve a
//! piece the same way — the open project from memory, every other from
//! `store::read` ([`Bus::project_for`]) — so the sheet can never show a piece
//! that Start then refuses (spec E3a).

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use video_coach_core::audio::audio_regions;
use video_coach_core::export::basket_schedule;
use video_coach_core::metadata::{basket_tags, clip_label, match_label};
use video_coach_core::plan::{clip_source_duration, BasketPiece};
use video_coach_core::project::{Clip, Preferences, Project, Quality, Resolution};
use video_coach_core::scoreboard::ScoreboardContext;
use video_coach_core::store;
use video_coach_media::{Encode, EntryMedia, ExportJob, MatchMedia, Render};

use super::state::StateFile;
use super::{Bus, Event, UserError};

/// Beside `state.json`, and deliberately not inside it (spec H1).
const FILE: &str = "basket.json";

/// What an empty name falls back to, and what the sheet shows as its
/// placeholder, so what the coach sees is what they get (spec O1).
const DEFAULT_NAME: &str = "Basket";

/// One piece: a clip in a project, by reference.
///
/// The folder is canonical (`Open::folder` is), so two references to the same
/// project compare equal whatever path it was opened by (spec E2). Clip ids are
/// unique within a project by construction, not across projects, so the pair is
/// the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Piece {
    folder: PathBuf,
    clip: Uuid,
}

/// The file's whole document.
///
/// Every field defaults, so a file from before one reads. The pickers are
/// labels for the reason `state.json`'s model and pen are: a value this build
/// can't read must not be able to take the pieces down with it (spec H2).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    #[serde(default)]
    name: String,
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    quality: Option<String>,
    /// **The document.** A malformed list is the one thing here that costs a
    /// re-gather, so it is logged rather than passed over in silence.
    #[serde(default)]
    pieces: Vec<Piece>,
}

/// [`Resolution`] as the file spells it. Its serde form, written by hand here
/// so that reading a label back can fail softly.
fn resolution_label(resolution: Resolution) -> &'static str {
    match resolution {
        Resolution::R720 => "r720",
        Resolution::R1080 => "r1080",
        Resolution::R2160 => "r2160",
    }
}

fn resolution_from_label(label: &str) -> Option<Resolution> {
    match label {
        "r720" => Some(Resolution::R720),
        "r1080" => Some(Resolution::R1080),
        "r2160" => Some(Resolution::R2160),
        _ => None,
    }
}

fn quality_label(quality: Quality) -> &'static str {
    match quality {
        Quality::Low => "low",
        Quality::Medium => "medium",
        Quality::High => "high",
    }
}

fn quality_from_label(label: &str) -> Option<Quality> {
    match label {
        "low" => Some(Quality::Low),
        "medium" => Some(Quality::Medium),
        "high" => Some(Quality::High),
        _ => None,
    }
}

/// The basket the bus holds, and the file it is written to on every change.
///
/// It lives on [`Bus`] itself rather than on `Open`, which is replaced on every
/// project open: the basket is precisely the thing that has to survive that
/// (spec H3). Losing the file costs a re-gather, so every failure is logged and
/// otherwise ignored.
pub(super) struct Basket {
    /// `None` when there is no config directory at all, where nothing is
    /// remembered and the basket lasts the session.
    path: Option<PathBuf>,
    name: String,
    resolution: Resolution,
    quality: Quality,
    pieces: Vec<Piece>,
}

impl Basket {
    /// The basket as `state`'s directory has it, defaulted where the file is
    /// absent or unreadable.
    pub(super) fn load(state: &StateFile) -> Self {
        let path = state.sibling(FILE);
        let stored = path.as_deref().map(read).unwrap_or_default();
        Basket {
            path,
            name: stored.name,
            resolution: stored
                .resolution
                .as_deref()
                .and_then(resolution_from_label)
                .unwrap_or_default(),
            quality: stored
                .quality
                .as_deref()
                .and_then(quality_from_label)
                .unwrap_or_default(),
            pieces: stored.pieces,
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let stored = Stored {
            name: self.name.clone(),
            resolution: Some(resolution_label(self.resolution).to_owned()),
            quality: Some(quality_label(self.quality).to_owned()),
            pieces: self.pieces.clone(),
        };
        if let Err(e) = write(path, &stored) {
            eprintln!("bus: could not write {}: {e}", path.display());
        }
    }
}

/// The file as it stands, defaulted where it is absent or unreadable — the
/// whole document, as `StateFile::read` does, because a file this build can't
/// parse says nothing trustworthy about any of its fields.
fn read(path: &Path) -> Stored {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Stored::default();
    };
    serde_json::from_str::<Stored>(&text).unwrap_or_else(|e| {
        eprintln!("bus: ignoring unreadable {}: {e}", path.display());
        Stored::default()
    })
}

fn write(path: &Path, stored: &Stored) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(stored).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The basket as the UI shows it (spec C4), following the export run's rule:
/// the sheet renders what it is handed, so it can't be left holding a state the
/// bus has moved past. The badge's count is `pieces.len()`.
#[derive(Debug, Clone, PartialEq)]
pub struct BasketView {
    pub name: String,
    pub resolution: Resolution,
    pub quality: Quality,
    pub pieces: Vec<BasketRow>,
}

/// One row of the basket's list, resolved from its project each time the sheet
/// is shown — never cached beside the reference, which would go stale the
/// moment the coach renamed a clip (spec E3).
#[derive(Debug, Clone, PartialEq)]
pub struct BasketRow {
    /// `"Rovers v Athletic"`, or the project's own name (spec T2).
    pub match_label: String,
    pub clip_label: String,
    /// Roughly how long the piece runs: the clip's own `recording_duration`.
    /// **Not a plan** — a row wants "about 12 s", and the run's frame counts
    /// remain the only authority on the film's length.
    pub seconds: f64,
    /// Why this piece can't be exported, or empty (spec V1).
    pub problem: String,
}

impl Bus {
    /// The project a piece's folder names: the open one **in memory**, or the
    /// file. Every basket refusal about a project comes from here.
    ///
    /// Memory rather than the file for the open project because a failed save
    /// leaves memory ahead of disk and says so, and a sheet reading one with a
    /// Start reading the other would show a piece that Start then refuses
    /// (spec E3a).
    ///
    /// **One shape, naming the folder** (spec V6): `From<StoreError>` maps a
    /// format mismatch to `TooNewProject`, whose wording names no path — fine
    /// for the project the coach just asked to open, misleading for one of
    /// three a basket is reaching into. The store error's own sentence is kept
    /// as the reason.
    fn project_for(&self, folder: &Path) -> Result<Cow<'_, Project>, UserError> {
        if let Some(open) = &self.open {
            if open.folder == folder {
                return Ok(Cow::Borrowed(&open.project));
            }
        }
        store::read(folder).map(Cow::Owned).map_err(|e| {
            UserError::CantExport(format!(
                "the project at {} can't be read: {e}",
                folder.display()
            ))
        })
    }

    /// Adds the open project's clip to the end of the basket (spec E1, U1).
    ///
    /// A clip already in it is a no-op with a **notice**, following
    /// `Command::Transcribe`'s precedent: one click refused, with nothing to
    /// answer.
    pub(super) fn add_to_basket(&mut self, clip_id: Uuid) {
        let Some(open) = &self.open else {
            // The menu the item lives in needs an open project to exist at
            // all, so reaching here is a UI bug.
            return eprintln!("bus: nothing to add to the basket: no project is open");
        };
        let piece = Piece {
            folder: open.folder.clone(),
            clip: clip_id,
        };
        if !open.project.clips.iter().any(|c| c.id == clip_id) {
            return eprintln!("bus: no clip {clip_id} to add to the basket");
        }
        if self.basket.pieces.contains(&piece) {
            return self.emit(Event::Error(UserError::Basket(
                "already in the basket".into(),
            )));
        }
        self.basket.pieces.push(piece);
        self.basket.save();
        self.publish_basket();
    }

    pub(super) fn remove_from_basket(&mut self, index: usize) {
        if index >= self.basket.pieces.len() {
            return;
        }
        self.basket.pieces.remove(index);
        self.basket.save();
        self.publish_basket();
    }

    /// `Vec::remove` + `Vec::insert`, as `MoveClip` is.
    pub(super) fn move_basket_entry(&mut self, from: usize, to: usize) {
        let len = self.basket.pieces.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let piece = self.basket.pieces.remove(from);
        self.basket.pieces.insert(to, piece);
        self.basket.save();
        self.publish_basket();
    }

    /// Empties it, with no undo: what is lost is a list of *references* — every
    /// clip is still in its project — so the cost of a mis-click is a
    /// re-gather (spec U7).
    pub(super) fn clear_basket(&mut self) {
        if self.basket.pieces.is_empty() {
            return;
        }
        self.basket.pieces.clear();
        self.basket.save();
        self.publish_basket();
    }

    /// Resolves every piece against its project and publishes the rows
    /// (spec C4). Also what the bus does at startup, so the badge's count is
    /// right before anything is opened.
    pub(super) fn publish_basket(&mut self) {
        let pieces: Vec<BasketRow> = self
            .basket
            .pieces
            .iter()
            .map(|piece| match self.project_for(&piece.folder) {
                Err(_) => BasketRow {
                    match_label: piece
                        .folder
                        .file_name()
                        .map_or_else(String::new, |f| f.to_string_lossy().into_owned()),
                    clip_label: String::new(),
                    seconds: 0.0,
                    // **A phrase, not the store error's sentence.** A row is
                    // one line of a narrow list, and the sentence that names
                    // the folder and the reason (spec V6) is elided down to
                    // the half of it that says nothing. The folder is in the
                    // label beside this, and the whole sentence is Start's
                    // refusal, on the sheet's message line.
                    problem: "the project can't be read".into(),
                },
                Ok(project) => {
                    let clip = project.clips.iter().find(|c| c.id == piece.clip);
                    BasketRow {
                        match_label: match_label(&project),
                        clip_label: clip.map_or_else(String::new, |c| clip_label(c).to_owned()),
                        seconds: clip.map_or(0.0, |c| c.recording_duration),
                        problem: match clip {
                            Some(_) => String::new(),
                            None => "the clip is gone".into(),
                        },
                    }
                }
            })
            .collect();
        let view = BasketView {
            name: self.basket.name.clone(),
            resolution: self.basket.resolution,
            quality: self.basket.quality,
            pieces,
        };
        self.emit(Event::Basket(view));
    }

    /// Renders the basket as one film, or says why it can't.
    ///
    /// The sheet's name and pickers become the basket's whichever way that
    /// goes, and the file is written on every change, a refused Start included:
    /// it *is* the sheet's memory, and there is no project to dirty (spec C3).
    pub(super) fn export_basket(&mut self, name: String, resolution: Resolution, quality: Quality) {
        self.basket.name = name;
        self.basket.resolution = resolution;
        self.basket.quality = quality;
        self.basket.save();
        match self.basket_job() {
            Ok(job) => {
                if let Err(e) = self.begin(vec![job]) {
                    self.emit(Event::Error(e));
                }
            }
            Err(e) => self.emit(Event::Error(e)),
        }
        // Last, so the sheet's list is re-resolved against whatever Start
        // found: a piece that has gone shows its reason beside the refusal.
        self.publish_basket();
    }

    /// The one job a basket run renders, or the first reason it can't — all or
    /// nothing, because a film silently missing the piece the coach cared about
    /// is worse than no film (spec V1).
    fn basket_job(&self) -> Result<(String, ExportJob), UserError> {
        let refused = |why: String| UserError::CantExport(why);
        // Before three projects' worth of I/O (spec C3).
        self.refuse_if_busy()?;
        if self.basket.pieces.is_empty() {
            return Err(refused("the basket is empty".into()));
        }

        // Each distinct project read once, in the order the pieces name them,
        // and one record of each match shared by every piece of it.
        let mut matches: Vec<(&Path, Cow<Project>, Arc<MatchMedia>)> = Vec::new();
        for piece in &self.basket.pieces {
            if matches.iter().any(|(folder, ..)| *folder == piece.folder) {
                continue;
            }
            let project = self.project_for(&piece.folder)?;
            let media = Arc::new(MatchMedia {
                // Frozen with the project as it stands: the run's own copy.
                scoreboard: ScoreboardContext::for_project(&project),
                highlights: project.player_highlights.clone(),
                avatar: project.avatar.as_ref().map(|file| piece.folder.join(file)),
            });
            matches.push((&piece.folder, project, media));
        }
        // Which match each piece plays from, and the clip itself: a piece is
        // handed to core as the clip, never as an id to look up, so no pairing
        // can degrade to a clip with no events (spec J7).
        let mut played: Vec<(usize, &Clip)> = Vec::with_capacity(self.basket.pieces.len());
        for piece in &self.basket.pieces {
            let index = matches
                .iter()
                .position(|(folder, ..)| *folder == piece.folder)
                .expect("every piece's project was read above");
            let (_, project, _) = &matches[index];
            let clip = project
                .clips
                .iter()
                .find(|c| c.id == piece.clip)
                .ok_or_else(|| {
                    refused(format!("{} — a piece's clip is gone", match_label(project)))
                })?;
            played.push((index, clip));
        }

        let pieces: Vec<BasketPiece> = played
            .iter()
            .map(|&(index, clip)| {
                let (_, project, _) = &matches[index];
                BasketPiece {
                    clip,
                    source_duration: clip_source_duration(project, clip),
                    match_label: match_label(project),
                }
            })
            .collect();
        let compilation = basket_schedule(&pieces);

        let mut entries = Vec::with_capacity(compilation.plan.entries.len());
        for (entry, &(index, _)) in compilation.plan.entries.iter().zip(&played) {
            let (folder, project, media) = &matches[index];
            // Every refusal names the match as well as the clip: two projects
            // can hold clips with the same name, and "Corner's game video is
            // missing" would not say which one to go and fix (spec V1).
            let whose = format!("{} — ", match_label(project));
            let (source, clip) = super::export::entry_media(folder, project, entry, &whose)?;
            entries.push(EntryMedia {
                source,
                clip,
                match_media: media.clone(),
            });
        }

        let name = match self.basket.name.trim() {
            "" => DEFAULT_NAME,
            name => name,
        };
        let dir = self.state.basket_dir();
        let path = output_path(&dir, name);
        // On demand, and after the refusals: a run that can't start leaves no
        // folder behind (spec O1).
        std::fs::create_dir_all(&dir)
            .map_err(|e| refused(format!("could not create {}: {e}", dir.display())))?;
        let tags = basket_tags(
            name,
            pieces.len(),
            &matches
                .iter()
                .map(|(_, project, _)| project.as_ref())
                .collect::<Vec<&Project>>(),
        );
        let job = ExportJob {
            render: Render::Encode(Encode {
                // The default volumes, not the open project's: a project's
                // preview volumes are a scanning convenience, and a film whose
                // level jumps between pieces for an invisible reason is worse
                // than one that doesn't (spec J6).
                audio: audio_regions(&compilation, &Preferences::default()),
                entries,
                resolution: self.basket.resolution,
                quality: self.basket.quality,
            }),
            tags,
            compilation,
            // A basket is clips: the board is burned in, and no `.srt` beside
            // the film is written **or removed** (spec O5).
            cues: None,
            // The sheet reads the file that was written off the run's own
            // `TargetState::Done`, which is how a suffixed name reaches it.
            path,
        };
        Ok((label_of(&job.path), job))
    }
}

/// What the run's one row is called: the film's own stem, so a name that had to
/// be suffixed says so wherever the run is shown (spec O1).
fn label_of(path: &Path) -> String {
    path.file_stem().map_or_else(
        || DEFAULT_NAME.to_owned(),
        |s| s.to_string_lossy().into_owned(),
    )
}

/// `<dir>/<name>.mp4`, with `/` and `:` replaced as an export's file name does,
/// and ` (2)`, ` (3)` … rather than overwriting a film already there.
///
/// An export's `<label> - <project>.mp4` is safe to overwrite because it is
/// derived from stable identity. A basket's name is typed, free-form, and its
/// *contents* change under it: `Corners` in October and `Corners` in March are
/// two films. Suffixing rather than refusing, because Start is the "walk away"
/// button — refusing at the last possible moment, after twenty pieces have
/// resolved, over a file the coach may not care about, is the worse failure
/// (spec O1).
fn output_path(dir: &Path, name: &str) -> PathBuf {
    let stem = name.replace(['/', ':'], "-");
    let mut path = dir.join(format!("{stem}.mp4"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}).mp4"));
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(dir: &Path) -> PathBuf {
        StateFile::in_config_dir(dir)
            .sibling(FILE)
            .expect("a config directory")
    }

    /// The file round-trips, and a fresh handle on it — what a relaunch sees —
    /// reads the same basket.
    #[test]
    fn the_basket_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateFile::in_config_dir(dir.path());
        let mut basket = Basket::load(&state);
        assert!(basket.pieces.is_empty());
        basket.name = "Corners".into();
        basket.resolution = Resolution::R2160;
        basket.quality = Quality::High;
        basket.pieces.push(Piece {
            folder: "/p/game".into(),
            clip: Uuid::nil(),
        });
        basket.save();

        let read = Basket::load(&state);
        assert_eq!(read.name, "Corners");
        assert_eq!(read.resolution, Resolution::R2160);
        assert_eq!(read.quality, Quality::High);
        assert_eq!(read.pieces, basket.pieces);
    }

    /// **A picker this build can't read must not take the pieces with it**
    /// (spec H2): the labels read as the defaults and the evening's gathering
    /// survives.
    #[test]
    fn an_unknown_picker_label_reads_as_the_default_and_keeps_the_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let path = stored(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"name":"Corners","resolution":"r4320","quality":"insane",
                "pieces":[{"folder":"/p/game","clip":"00000000-0000-0000-0000-000000000000"}]}"#,
        )
        .unwrap();
        let basket = Basket::load(&StateFile::in_config_dir(dir.path()));
        assert_eq!(basket.resolution, Resolution::default());
        assert_eq!(basket.quality, Quality::default());
        assert_eq!(basket.name, "Corners");
        assert_eq!(basket.pieces.len(), 1);
    }

    /// A file that won't parse reads as an empty basket — the one loss here
    /// that costs a re-gather, which is why it is logged.
    #[test]
    fn a_corrupt_file_reads_as_an_empty_basket() {
        let dir = tempfile::tempdir().unwrap();
        let path = stored(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        let basket = Basket::load(&StateFile::in_config_dir(dir.path()));
        assert!(basket.pieces.is_empty());
        assert_eq!(basket.name, "");
    }

    /// **Writing the basket touches nothing in `state.json`** — the whole
    /// reason it is a file of its own (spec H1).
    #[test]
    fn the_basket_and_the_state_file_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateFile::in_config_dir(dir.path());
        state.set_last_project(Some(Path::new("/p/game")));
        state.set_pen(crate::drawing::Pen::Pink);

        let mut basket = Basket::load(&state);
        basket.pieces.push(Piece {
            folder: "/p/game".into(),
            clip: Uuid::nil(),
        });
        basket.save();

        assert_eq!(state.last_project(), Some(PathBuf::from("/p/game")));
        assert_eq!(state.pen(), crate::drawing::Pen::Pink);
        // And a basket this build can't read leaves them alone too, which a
        // key in that file could not promise.
        std::fs::write(stored(dir.path()), "{not json").unwrap();
        assert_eq!(state.last_project(), Some(PathBuf::from("/p/game")));
        assert_eq!(state.pen(), crate::drawing::Pen::Pink);
    }

    /// The name: trimmed, defaulted, path characters replaced, and never an
    /// overwrite.
    #[test]
    fn the_film_is_named_after_the_basket_and_never_overwrites_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| output_path(dir.path(), name);
        assert_eq!(path("Corners"), dir.path().join("Corners.mp4"));
        assert_eq!(
            path("4/4 press: away"),
            dir.path().join("4-4 press- away.mp4")
        );

        std::fs::write(dir.path().join("Corners.mp4"), b"a film").unwrap();
        assert_eq!(path("Corners"), dir.path().join("Corners (2).mp4"));
        std::fs::write(dir.path().join("Corners (2).mp4"), b"another").unwrap();
        assert_eq!(path("Corners"), dir.path().join("Corners (3).mp4"));
    }

    /// An empty or blank name is the default, which is also the sheet's
    /// placeholder — so `" .mp4"`, a dotfile with no stem, is unreachable.
    #[test]
    fn a_blank_name_falls_back_to_the_default() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["", "   "] {
            let trimmed = match name.trim() {
                "" => DEFAULT_NAME,
                name => name,
            };
            assert_eq!(
                output_path(dir.path(), trimmed),
                dir.path().join("Basket.mp4"),
                "{name:?}"
            );
        }
    }
}
