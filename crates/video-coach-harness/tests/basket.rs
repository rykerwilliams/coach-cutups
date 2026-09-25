//! Bus end to end: the basket (basket spec C, E, H, O, V) — one film whose
//! pieces come from several matches, gathered across project opens.
//!
//! Layout per test: `<tmp>/config` holds the app's own files (`state.json`,
//! `basket.json`, and the folder a basket's film is written into), `<tmp>/a`
//! and `<tmp>/b` the two matches, `<tmp>/media` their fixture videos.
//!
//! Runs here are 1 s a piece and exported at 720p: on CI they render on
//! llvmpipe.

use std::path::{Path, PathBuf};

use uuid::Uuid;
use video_coach_app::bus::{AppFiles, Command, Event, ExportRun, TargetState, UserError};
use video_coach_core::project::{Quality, Resolution};
use video_coach_core::store::{self, CURRENT_FORMAT_VERSION, RECORDINGS_DIRNAME};
use video_coach_core::undo::ClipEdit;
use video_coach_harness::{add_clips, write_project, Harness, ReadOnly};

/// A match in `<tmp>/<dir>` called `name`, with one 2 s fixture video and one
/// 1 s clip per entry of `clips`, called `clip 0`, `clip 1`, …
///
/// The team names are invented: the repository is public and the coach's
/// footage shows children.
fn write_match(
    tmp: &Path,
    dir: &str,
    name: &str,
    video: &str,
    clips: usize,
) -> (PathBuf, Vec<Uuid>) {
    let folder = tmp.join(dir);
    let media = tmp.join("media");
    for dir in [&folder, &media] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut project = write_project(&folder, &media, &[(video, 2)]);
    let ids = add_clips(&folder, &mut project, &vec![0; clips])
        .iter()
        .map(|c| c.id)
        .collect();
    project.name = name.to_owned();
    for (i, clip) in project.clips.iter_mut().enumerate() {
        clip.name = format!("clip {i}");
    }
    store::write(&folder, &mut project).unwrap();
    (folder, ids)
}

/// Opens each piece's project in turn and adds its clip, as the coach does
/// while working: the basket is gathered **across** opens.
fn gather(h: &mut Harness, pieces: &[(&Path, Uuid)]) {
    for (i, &(folder, clip_id)) in pieces.iter().enumerate() {
        h.send(Command::OpenProject(folder.to_owned()));
        h.wait_opened();
        h.send(Command::AddToBasket { clip_id });
        h.wait_map("the piece added", |e| match e {
            Event::Basket(view) if view.pieces.len() == i + 1 => Some(()),
            _ => None,
        });
    }
}

/// The run's last word: the event with nothing left running.
fn outcome(h: &mut Harness) -> ExportRun {
    h.wait_map("the run's outcome", |e| match e {
        Event::Export(run) if !run.is_running() => Some(run.clone()),
        _ => None,
    })
}

/// Where a basket's film is written for a harness whose config directory is
/// `config` — under it, so no test writes into the coach's own `~/Videos`.
fn films(config: &Path) -> PathBuf {
    AppFiles::in_config_dir(config).basket_dir()
}

/// **A refused run leaves no folder at all**, not an empty one (spec O1): the
/// basket's folder is created on demand by the run that writes a film into it,
/// which is the same rule an export's `exports/` follows — and the difference
/// [`outputs`] cannot see.
fn no_films(config: &Path) {
    let films = films(config);
    assert!(!films.exists(), "a refused run made {films:?}");
}

/// The files in the basket's folder, sorted; empty when no run has created it.
fn outputs(films: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(films)
        .into_iter()
        .flatten()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// **Nothing of any project changes when Start is refused.** The pickers are
/// written back only after the run has begun (spec C3), and a basket has no
/// project to dirty in the first place: `basket.json` is its memory.
#[test]
fn a_refused_start_changes_no_project() {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (a, a_clips) = write_match(tmp.path(), "a", "Rovers v Athletic", "a.webm", 1);
    let (b, b_clips) = write_match(tmp.path(), "b", "City v Athletic", "b.webm", 1);
    let config = tmp.path().join("config");

    let mut h = Harness::new(&config);
    h.send(Command::OpenProject(a.clone()));
    h.wait_opened();
    h.send(Command::AddToBasket {
        clip_id: a_clips[0],
    });
    h.send(Command::OpenProject(b.clone()));
    h.wait_opened();
    h.send(Command::AddToBasket {
        clip_id: b_clips[0],
    });
    let view = h.wait_map("both pieces", |e| match e {
        Event::Basket(view) if view.pieces.len() == 2 => Some(view.clone()),
        _ => None,
    });
    assert_eq!(view.pieces[1].match_label, "City v Athletic");

    // The second piece's game video goes, which only Start can discover.
    std::fs::remove_file(tmp.path().join("media").join("b.webm")).unwrap();
    let saved: Vec<Vec<u8>> = [&a, &b]
        .iter()
        .map(|f| std::fs::read(f.join("project.json")).unwrap())
        .collect();

    h.send(Command::ExportBasket {
        name: "Corners".into(),
        resolution: Resolution::R720,
        quality: Quality::Low,
    });
    assert_eq!(
        h.wait_for_error(),
        UserError::CantExport(
            "City v Athletic — clip 0's game video is missing; relink it first".into()
        )
    );
    // The refusal names the piece, so nothing else need be published: no
    // project was touched, and no run started.
    assert!(
        !h.log()
            .iter()
            .any(|e| matches!(e, Event::ProjectChanged(_) | Event::Export(_))),
        "{:#?}",
        h.log()
    );
    let rest = h.shutdown();
    assert!(
        !rest
            .iter()
            .any(|e| matches!(e, Event::ProjectChanged(_) | Event::Export(_))),
        "{rest:#?}"
    );
    for (folder, before) in [&a, &b].iter().zip(saved) {
        assert_eq!(
            std::fs::read(folder.join("project.json")).unwrap(),
            before,
            "{} was rewritten",
            folder.display()
        );
    }
    no_films(&config);
}

/// **The film Start makes**: one run of one target, rendered from two
/// matches, written into the basket's folder under the name the sheet typed.
///
/// No `.srt` beside it (a basket is clips, spec O5), and no pasteable chapter
/// list either: two one-second pieces make no list YouTube would read, exactly
/// as a two-clip compilation doesn't.
#[test]
fn a_basket_of_two_matches_renders_one_film() {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (a, a_clips) = write_match(tmp.path(), "a", "Rovers v Athletic", "a.webm", 1);
    let (b, b_clips) = write_match(tmp.path(), "b", "City v Athletic", "b.webm", 1);
    let config = tmp.path().join("config");

    let mut h = Harness::new(&config);
    gather(&mut h, &[(&a, a_clips[0]), (&b, b_clips[0])]);
    h.send(Command::ExportBasket {
        name: "Corners".into(),
        resolution: Resolution::R720,
        quality: Quality::Low,
    });

    let run = h.wait_export();
    assert_eq!(run.targets.len(), 1);
    assert_eq!(run.targets[0].label, "Corners");
    // Two one-second pieces, quantized per entry at 30 fps.
    assert_eq!(run.targets[0].frames, 60);
    let done = outcome(&mut h);
    let film = films(&config).join("Corners.mp4");
    assert_eq!(done.targets[0].state, TargetState::Done(film.clone()));
    h.shutdown();
    assert_eq!(outputs(&films(&config)), ["Corners.mp4"]);
    assert!(film.metadata().unwrap().len() > 0);
}

/// A second run of the same basket doesn't overwrite the first film: the name
/// is typed and the basket's contents change under it (spec O1). The run's own
/// label carries the name that was used, which is how the sheet says so.
#[test]
fn a_repeat_start_writes_a_second_film() {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (a, clips) = write_match(tmp.path(), "a", "Rovers v Athletic", "a.webm", 1);
    let config = tmp.path().join("config");
    let mut h = Harness::new(&config);
    gather(&mut h, &[(&a, clips[0])]);

    for (label, file) in [
        ("Corners", "Corners.mp4"),
        ("Corners (2)", "Corners (2).mp4"),
    ] {
        h.send(Command::ExportBasket {
            name: "Corners".into(),
            resolution: Resolution::R720,
            quality: Quality::Low,
        });
        let run = h.wait_export();
        assert_eq!(run.targets[0].label, label);
        let done = outcome(&mut h);
        assert_eq!(
            done.targets[0].state,
            TargetState::Done(films(&config).join(file))
        );
    }
    h.shutdown();
    assert_eq!(outputs(&films(&config)), ["Corners (2).mp4", "Corners.mp4"]);
}

/// The basket survives a project open — which is the whole point of it living
/// outside `Open` — and a restart of the bus, which is the point of its file.
#[test]
fn the_basket_outlives_a_project_open_and_a_restart() {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (a, a_clips) = write_match(tmp.path(), "a", "Rovers v Athletic", "a.webm", 1);
    let (b, _) = write_match(tmp.path(), "b", "City v Athletic", "b.webm", 1);
    let config = tmp.path().join("config");

    let mut h = Harness::new(&config);
    gather(&mut h, &[(&a, a_clips[0])]);
    h.send(Command::OpenProject(b.clone()));
    h.wait_opened();
    h.send(Command::ShowBasket);
    let view = h.wait_basket();
    assert_eq!(view.pieces.len(), 1);
    assert_eq!(view.pieces[0].match_label, "Rovers v Athletic");
    assert_eq!(view.pieces[0].clip_label, "clip 0");
    assert_eq!(view.pieces[0].seconds, 1.0);
    assert!(view.pieces[0].problem.is_empty());
    h.shutdown();

    // A fresh bus on the same config directory: what a relaunch sees.
    let mut h = Harness::new(&config);
    let view = h.wait_basket();
    assert_eq!(view.pieces.len(), 1);
    assert_eq!(view.pieces[0].match_label, "Rovers v Athletic");
    h.shutdown();
}

/// **The open project is resolved from memory, not from its file** (spec E3a):
/// a rename whose save failed is still what the basket shows and what Start
/// would render, so the sheet can't show a piece Start refuses — or the other
/// way round.
#[test]
fn a_piece_of_the_open_project_is_resolved_from_memory() {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (a, clips) = write_match(tmp.path(), "a", "Rovers v Athletic", "a.webm", 1);
    let config = tmp.path().join("config");
    let mut h = Harness::new(&config);
    gather(&mut h, &[(&a, clips[0])]);

    let read_only = ReadOnly::new(&a);
    if !read_only.enforced() {
        return; // running as root: the save would succeed
    }
    h.send(Command::EditClip {
        id: clips[0],
        edit: ClipEdit::Name("Corner, 2nd half".into()),
    });
    h.wait_changed();
    h.send(Command::ShowBasket);
    let view = h.wait_basket();
    assert_eq!(view.pieces[0].clip_label, "Corner, 2nd half");
    drop(read_only);
    h.shutdown();
}

/// Each refusal in spec V1's table, naming its piece, leaving no film.
#[test]
fn every_refusal_names_its_piece_and_writes_nothing() {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (a, a_clips) = write_match(tmp.path(), "a", "Rovers v Athletic", "a.webm", 1);
    let (b, b_clips) = write_match(tmp.path(), "b", "City v Athletic", "b.webm", 1);
    let config = tmp.path().join("config");
    let mut h = Harness::new(&config);

    let start = |h: &Harness| {
        h.send(Command::ExportBasket {
            name: "Corners".into(),
            resolution: Resolution::R720,
            quality: Quality::Low,
        })
    };
    start(&h);
    assert_eq!(
        h.wait_for_error(),
        UserError::CantExport("the basket is empty".into())
    );

    gather(&mut h, &[(&a, a_clips[0]), (&b, b_clips[0])]);

    // Each step takes more of the second match away, so every refusal is the
    // one it names: the recording, then the clip, then the project itself.
    for entry in std::fs::read_dir(b.join(RECORDINGS_DIRNAME)).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
    start(&h);
    assert_eq!(
        h.wait_for_error(),
        UserError::CantExport("City v Athletic — clip 0's commentary recording is missing".into())
    );

    // Back to the first match, so the second is resolved from its file rather
    // than from memory (spec E3a) and deleting its clip there is what Start
    // sees.
    h.send(Command::OpenProject(a.clone()));
    h.wait_opened();
    let mut project = store::read(&b).unwrap();
    project.clips.clear();
    store::write(&b, &mut project).unwrap();
    start(&h);
    assert_eq!(
        h.wait_for_error(),
        UserError::CantExport("City v Athletic — a piece's clip is gone".into())
    );

    // The whole project: one shape, naming the folder (spec V6).
    let text = std::fs::read_to_string(b.join("project.json")).unwrap();
    std::fs::remove_file(b.join("project.json")).unwrap();
    start(&h);
    assert_eq!(
        h.wait_for_error(),
        UserError::CantExport(format!(
            "the project at {} can't be read: no project.json in {}",
            b.display(),
            b.display()
        ))
    );

    // And a format this build is too old for names the folder too, where
    // `TooNewProject`'s own wording names no path at all.
    std::fs::write(
        b.join("project.json"),
        text.replace(
            &format!("\"formatVersion\": {CURRENT_FORMAT_VERSION}"),
            &format!("\"formatVersion\": {}", CURRENT_FORMAT_VERSION + 1),
        ),
    )
    .unwrap();
    start(&h);
    let message = h.wait_for_error().to_string();
    assert!(
        message.starts_with(&format!(
            "can't export: the project at {} can't be read:",
            b.display()
        )) && message.contains(&format!("v{}", CURRENT_FORMAT_VERSION + 1)),
        "{message}"
    );

    let rest = h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Export(_))),
        "{rest:#?}"
    );
    no_films(&config);
}

/// Removing and reordering pieces changes the film: the rows follow the list,
/// and the run's frame count follows the rows.
#[test]
fn removing_and_moving_pieces_changes_the_run() {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (a, clips) = write_match(tmp.path(), "a", "Rovers v Athletic", "a.webm", 2);
    let config = tmp.path().join("config");
    let mut h = Harness::new(&config);
    gather(&mut h, &[(&a, clips[0]), (&a, clips[1])]);

    h.send(Command::MoveBasketEntry { from: 1, to: 0 });
    let view = h.wait_basket();
    assert_eq!(
        view.pieces
            .iter()
            .map(|p| p.clip_label.as_str())
            .collect::<Vec<_>>(),
        ["clip 1", "clip 0"]
    );

    h.send(Command::RemoveFromBasket { index: 0 });
    let view = h.wait_basket();
    assert_eq!(
        view.pieces
            .iter()
            .map(|p| p.clip_label.as_str())
            .collect::<Vec<_>>(),
        ["clip 0"]
    );

    h.send(Command::ExportBasket {
        name: "One".into(),
        resolution: Resolution::R720,
        quality: Quality::Low,
    });
    let run = h.wait_export();
    assert_eq!(run.targets[0].frames, 30);
    h.send(Command::CancelExport);
    let done = outcome(&mut h);
    assert_eq!(done.targets[0].state, TargetState::Cancelled);
    h.shutdown();
    // A cancelled run leaves no film and no `.part` (spec E5).
    assert!(outputs(&films(&config)).is_empty());
}

/// Clearing empties it, and a clip already in the basket is a **notice**, not
/// a modal: one click refused with nothing to answer (spec V5).
#[test]
fn adding_a_clip_twice_says_so_and_changes_nothing() {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (a, clips) = write_match(tmp.path(), "a", "Rovers v Athletic", "a.webm", 1);
    let config = tmp.path().join("config");
    let mut h = Harness::new(&config);
    gather(&mut h, &[(&a, clips[0])]);

    h.send(Command::AddToBasket { clip_id: clips[0] });
    let e = h.wait_for_error();
    assert_eq!(e, UserError::Basket("already in the basket".into()));
    assert!(e.is_notice());

    h.send(Command::ShowBasket);
    assert_eq!(h.wait_basket().pieces.len(), 1);

    h.send(Command::ClearBasket);
    assert!(h.wait_basket().pieces.is_empty());
    // And the emptying is remembered, not just published.
    h.shutdown();
    let mut h = Harness::new(&config);
    assert!(h.wait_basket().pieces.is_empty());
    h.shutdown();
}
