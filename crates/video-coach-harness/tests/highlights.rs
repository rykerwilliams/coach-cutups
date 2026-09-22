//! Bus end to end: player highlights (match vision spec H) — the four
//! commands, the undo step each is, the one that is allowed while recording,
//! and a ring reaching a real export. What a highlight *is*, and where its
//! ring is drawn, are core's and media's tests.
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project (and, once a run starts, its `exports/`), `<tmp>/media` the
//! fixture game videos.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{Command, Event, TargetState, UserError};
use video_coach_core::highlight::{HighlightEdit, NormRect, PlayerHighlight};
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Project, Quality, Resolution};
use video_coach_core::store::{self, EXPORTS_DIRNAME};
use video_coach_core::stroke::Rgba;
use video_coach_core::zoom::Zoom;
use video_coach_harness::{add_clips, write_project, Harness};
use video_coach_media::fixtures;

/// A project of 2-second fixture videos, as written.
struct Proj {
    folder: PathBuf,
    _tmp: TempDir,
}

impl Proj {
    /// Writes the project and opens it on a fresh bus.
    fn open(videos: &[&str]) -> (Harness, Self) {
        Self::open_with(videos, |_| {})
    }

    /// [`Proj::open`], with `before_open` run on the written project first.
    fn open_with(videos: &[&str], before_open: impl FnOnce(&Path)) -> (Harness, Self) {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&media).unwrap();
        let videos: Vec<(&str, u32)> = videos.iter().map(|&name| (name, 2)).collect();
        write_project(&folder, &media, &videos);
        before_open(&folder);

        let mut h = Harness::new(&tmp.path().join("config"));
        h.send(Command::OpenProject(folder.clone()));
        h.wait_opened();
        (h, Proj { folder, _tmp: tmp })
    }

    fn saved(&self) -> Project {
        store::read(&self.folder).unwrap()
    }
}

/// A key as the H tool sends it: the displayed frame's stream time and a box
/// in source fractions, both captured by the caller.
fn key(id: Uuid, source_index: usize, source_seconds: f64, rect: NormRect) -> Command {
    Command::SetHighlightKey {
        id,
        source_index,
        source_seconds,
        rect,
        color: Rgba::RED,
    }
}

/// A fifth of the frame square, with its top-left corner at `(x, y)`.
fn rect(x: f64, y: f64) -> NormRect {
    NormRect {
        x,
        y,
        w: 0.2,
        h: 0.2,
    }
}

fn highlights(p: &Project) -> Vec<PlayerHighlight> {
    p.player_highlights.clone()
}

fn no_project_changed(rest: &[Event]) {
    assert!(
        !rest.iter().any(|e| matches!(e, Event::ProjectChanged(_))),
        "{rest:#?}"
    );
}

/// The first key creates the highlight with the pen's colour and no label; a
/// second key at the same stream time replaces it, because the same frame
/// always gives the same number (spec H2).
#[test]
fn a_key_creates_a_highlight_and_one_at_the_same_time_replaces_it() {
    let (mut h, p) = Proj::open(&["a.webm"]);
    let id = Uuid::new_v4();

    h.send(key(id, 0, 1.0, rect(0.1, 0.1)));
    let one = highlights(&h.wait_changed().project);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].id, id);
    assert_eq!(one[0].color, Rgba::RED);
    assert_eq!(one[0].label, "");
    assert_eq!(one[0].keys.len(), 1);
    assert!(!one[0].keys[0].tracked, "a hand key is never tracked");

    // A later frame is a second key; the same frame again replaces it.
    h.send(key(id, 0, 1.5, rect(0.3, 0.3)));
    assert_eq!(highlights(&h.wait_changed().project)[0].keys.len(), 2);
    h.send(key(id, 0, 1.5, rect(0.4, 0.4)));
    let replaced = highlights(&h.wait_changed().project);
    assert_eq!(replaced[0].keys.len(), 2);
    assert_eq!(replaced[0].keys[1].rect, rect(0.4, 0.4));

    h.shutdown();
    assert_eq!(highlights(&p.saved()), replaced);
}

/// A label of digits is stored as a shirt number, "Delete key here" on the
/// last key deletes the highlight, and each command is one undo step that
/// takes the whole list back with it.
#[test]
fn edits_are_saved_and_undone_a_step_at_a_time() {
    let (mut h, p) = Proj::open(&["a.webm"]);
    let id = Uuid::new_v4();

    h.send(key(id, 0, 1.0, rect(0.1, 0.1)));
    let created = highlights(&h.wait_changed().project);
    h.send(Command::EditHighlight {
        id,
        edit: HighlightEdit::Label("7".into()),
    });
    let labelled = highlights(&h.wait_changed().project);
    assert_eq!(labelled[0].label, "#7");

    // Its one key: deleting it deletes the highlight.
    h.send(Command::DeleteHighlightKey {
        id,
        source_seconds: 1.0,
    });
    assert!(highlights(&h.wait_changed().project).is_empty());

    // Back a step at a time: the delete, then the label.
    h.send(Command::Undo);
    assert_eq!(highlights(&h.wait_changed().project), labelled);
    h.send(Command::Undo);
    assert_eq!(highlights(&h.wait_changed().project), created);
    h.send(Command::Redo);
    assert_eq!(highlights(&h.wait_changed().project), labelled);

    // And `DeleteHighlight` takes the whole thing, undoably.
    h.send(Command::DeleteHighlight(id));
    assert!(highlights(&h.wait_changed().project).is_empty());
    h.send(Command::Undo);
    assert_eq!(highlights(&h.wait_changed().project), labelled);

    h.shutdown();
    assert_eq!(highlights(&p.saved()), labelled);
}

/// The coach rings a player while the take is paused, so `SetHighlightKey`
/// is on the recording allow-list (spec H3). Every other highlight edit
/// waits, as every other edit does.
#[test]
fn a_key_lands_while_recording_and_the_other_edits_are_refused() {
    let (mut h, p) = Proj::open(&["a.webm"]);
    h.wait_settled();
    let id = Uuid::new_v4();
    h.send(key(id, 0, 1.0, rect(0.1, 0.1)));
    h.wait_changed();

    h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    h.wait_recording();
    h.wait_recording();

    h.send(key(id, 0, 1.5, rect(0.3, 0.3)));
    assert_eq!(highlights(&h.wait_changed().project)[0].keys.len(), 2);

    h.send(Command::EditHighlight {
        id,
        edit: HighlightEdit::Label("7".into()),
    });
    h.send(Command::DeleteHighlight(id));
    // A refusal while recording is silent, so the stop behind them is what
    // proves they never landed.
    h.send(Command::StopRecording);
    h.wait_changed();
    h.wait_recording();

    h.shutdown();
    let saved = p.saved();
    assert_eq!(saved.player_highlights.len(), 1);
    assert_eq!(saved.player_highlights[0].keys.len(), 2);
    assert_eq!(saved.player_highlights[0].label, "");
}

/// A source a highlight sits on can't be removed, and the notice says so
/// rather than naming only clips and match events.
#[test]
fn a_source_with_a_highlight_cant_be_removed() {
    let (mut h, p) = Proj::open(&["a.webm", "b.webm"]);
    h.send(key(Uuid::new_v4(), 1, 1.0, rect(0.1, 0.1)));
    h.wait_changed();

    h.send(Command::RemoveSource(1));
    let err = h.wait_for_error();
    assert_eq!(err, UserError::SourceReferenced { index: 1 });
    assert!(err.to_string().contains("highlight"), "{err}");

    let rest = h.shutdown();
    no_project_changed(&rest);
    assert_eq!(p.saved().source_videos.len(), 2);
}

/// A source move remaps the stored highlights but not the snapshots on the
/// undo and redo stacks, so both are purged: undo can't restore a stale
/// `source_index` (spec F4).
#[test]
fn a_source_move_purges_the_highlight_history() {
    let (mut h, p) = Proj::open(&["a.webm", "b.webm"]);
    let id = Uuid::new_v4();
    h.send(key(id, 1, 1.0, rect(0.1, 0.1)));
    h.wait_changed();

    h.send(Command::MoveSource { from: 1, to: 0 });
    let moved = highlights(&h.wait_changed().project);
    assert_eq!(moved[0].source_index, 0);

    h.send(Command::Undo);
    h.send(Command::Redo);
    let rest = h.shutdown();
    no_project_changed(&rest);
    assert_eq!(highlights(&p.saved()), moved);
}

/// The highlights the commands write reach the export driver, so the ring is
/// burned into the file. What the ring looks like is media's test; this pins
/// the round trip from a command to a pixel.
#[test]
fn a_highlight_reaches_an_export() {
    let mut clip = None;
    let (mut h, p) = Proj::open_with(&["a.webm"], |folder| {
        let mut project = store::read(folder).unwrap();
        clip = Some(add_clips(folder, &mut project, &[0])[0].id);
    });
    let clip = clip.expect("the clip was added before the open");

    // The clip runs over source seconds [0.5, 1.5]; two keys either side of
    // it hold one box over every exported frame.
    let id = Uuid::new_v4();
    let held = rect(0.4, 0.4);
    h.send(key(id, 0, 0.4, held));
    h.wait_changed();
    h.send(key(id, 0, 1.6, held));
    h.wait_changed();

    h.send(Command::Export {
        targets: vec![ExportTarget::Clip(clip)],
        resolution: Resolution::R720,
        quality: Quality::Low,
    });
    let done = h.wait_map("the run's outcome", |e| match e {
        Event::Export(run) if !run.is_running() => Some(run.clone()),
        _ => None,
    });
    let TargetState::Done(path) = &done.targets[0].state else {
        panic!("{:?}", done.targets[0]);
    };
    let path = path.clone();
    assert_eq!(path.parent().unwrap(), p.folder.join(EXPORTS_DIRNAME));
    h.shutdown();

    // The picture fills the 1280x720 output (the fixture is 16:9), so the
    // box is at (512, 288), 256x144, and its ring is an ellipse centred on
    // the box's bottom edge, 1.4x as wide. The bottom of that ring is where
    // the stroke runs horizontally, which is what survives an encode at Low.
    let (bx, by, bw, bh) = (0.4 * 1280.0, 0.4 * 720.0, 0.2 * 1280.0, 0.2 * 720.0);
    let rx = 1.4 * bw / 2.0;
    let (cx, cy) = (bx + bw / 2.0, by + bh);

    let frames = fixtures::decode_rgb(&path);
    let frame = &frames[frames.len() / 2];
    let ring = frame.at(cx as usize, (cy + 0.35 * rx) as usize);
    let redness = |[r, g, b]: [u8; 3]| i32::from(r) - i32::from(g).max(i32::from(b));
    assert!(redness(ring) > 60, "the ring's red: got {ring:?}");
    // The box's own middle is untouched: only the ring and the label are
    // drawn, and this highlight has no label.
    let inside = frame.at(cx as usize, (by + bh / 2.0) as usize);
    assert!(
        redness(inside) < 30,
        "the box's middle was painted: {inside:?}"
    );
}
