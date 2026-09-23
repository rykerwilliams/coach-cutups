//! The project format: defaults, the version guard, and the store's contract.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::highlight::{HighlightKey, NormRect, PlayerHighlight};
use video_coach_core::plan::ScoreboardMode;
use video_coach_core::project::{
    Clip, Inset, Preferences, Project, Quality, Resolution, SourceRef,
};
use video_coach_core::recording::PendingClip;
use video_coach_core::scoreboard::{
    MatchEventKind, MatchEventRecord, MatchFormat, ScoreboardConfig, TeamConfig,
};
use video_coach_core::store::{
    self, StoreError, CURRENT_FORMAT_VERSION, MIN_READABLE_FORMAT_VERSION,
};
use video_coach_core::stroke::Rgba;

fn sample_clip() -> Clip {
    Clip {
        id: Uuid::nil(),
        name: "Transition".into(),
        notes: String::new(),
        tags: vec!["transition".into()],
        source_index: 0,
        start_source_seconds: 12.5,
        recording_duration: 8.0,
        recording_filename: "00000000-0000-0000-0000-000000000000.mkv".into(),
        events: Vec::new(),
        show_pip: true,
        inset: Inset::Camera,
        sort_index: 0,
        created_at: "2026-09-19T12:00:00Z".into(),
        transcript: String::new(),
    }
}

fn sample_project() -> Project {
    let mut p = Project::new("Match vs Rovers");
    p.source_videos.push(SourceRef {
        relative_path: "../film/first-half.mp4".into(),
        display_name: "first-half.mp4".into(),
        duration_seconds: 2700.0,
        display_aspect: 16.0 / 9.0,
    });
    p.clips.push(sample_clip());
    p.scoreboard = Some(ScoreboardConfig {
        home: TeamConfig::new(
            "Rovers",
            Rgba::RED,
            Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
        ),
        away: TeamConfig::new(
            "United",
            Rgba::RED,
            Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        ),
        format: MatchFormat::default(),
        // On, so the round trip covers the key Phase 9 added in place of the
        // per-event `isAutoBackAnchor` flag.
        auto_back_anchor_p1: true,
    });
    p.match_events.push(MatchEventRecord {
        id: Uuid::nil(),
        kind: MatchEventKind::StartStop,
        source_index: 0,
        source_seconds: 0.0,
        reel_lead_in: None,
        reel_tail: None,
    });
    // One trimmed side and one default, so the round trip covers both.
    p.match_events.push(MatchEventRecord {
        id: Uuid::from_u128(1),
        kind: MatchEventKind::HomeGoal,
        source_index: 0,
        source_seconds: 600.0,
        reel_lead_in: Some(12.5),
        reel_tail: None,
    });
    p.player_highlights.push(PlayerHighlight {
        id: Uuid::from_u128(2),
        source_index: 0,
        color: Rgba::RED,
        label: "#7".into(),
        keys: vec![
            HighlightKey {
                source_seconds: 600.0,
                rect: NormRect {
                    x: 0.1,
                    y: 0.2,
                    w: 0.05,
                    h: 0.2,
                },
                tracked: false,
            },
            HighlightKey {
                source_seconds: 601.0,
                rect: NormRect {
                    x: 0.15,
                    y: 0.2,
                    w: 0.05,
                    h: 0.2,
                },
                tracked: false,
            },
        ],
    });
    p
}

fn write_raw(dir: &Path, value: serde_json::Value) {
    std::fs::write(
        dir.join("project.json"),
        serde_json::to_string_pretty(&value).unwrap(),
    )
    .unwrap();
}

// ---------------------------------------------------------------- defaults

/// A round-trip test can never catch a wrong default — it serializes whatever
/// was constructed and reads it back. This is the test that bites: a blanket
/// `#[serde(default)]` would give 0.0 volumes (silent mute) and `false` for
/// PiP.
#[test]
fn preferences_defaults_are_not_zero() {
    let p: Preferences = serde_json::from_str("{}").unwrap();
    assert_eq!(p.scan_volume, 1.0);
    assert_eq!(p.preview_source_volume, 1.0);
    assert_eq!(p.preview_commentary_volume, 1.0);
    assert_eq!(p.last_export_resolution, Resolution::R1080);
    assert_eq!(p.last_export_quality, Quality::Medium);
    assert!(p.pip_for_new_recordings);
    assert_eq!(p.preferred_camera_id, None);
    assert_eq!(p.preferred_mic_id, None);
}

/// A preferences object carrying only some keys keeps real values for the rest.
#[test]
fn partial_preferences_keep_real_defaults_for_missing_keys() {
    let p: Preferences = serde_json::from_str(r#"{"scanVolume":0.25}"#).unwrap();
    assert_eq!(p.scan_volume, 0.25);
    assert_eq!(p.preview_source_volume, 1.0);
    assert!(p.pip_for_new_recordings);
}

/// Required fields are NOT defaulted. Defaulting `clips` would let a truncated
/// file load as an empty project, after which the next save destroys the
/// user's work.
#[test]
fn missing_clips_is_an_error_not_an_empty_project() {
    let dir = TempDir::new().unwrap();
    write_raw(
        dir.path(),
        json!({"formatVersion": 7, "name": "x", "sourceVideos": []}),
    );
    assert!(matches!(
        store::read(dir.path()),
        Err(StoreError::Malformed(_))
    ));
}

/// New. The on-disk shape of a source reference. A round trip can't catch a
/// wrong field name; this can.
#[test]
fn source_ref_has_the_expected_wire_shape() {
    let s = SourceRef {
        relative_path: "../film/a.mp4".into(),
        display_name: "a.mp4".into(),
        duration_seconds: 10.5,
        display_aspect: 1.5,
    };
    assert_eq!(
        serde_json::to_string(&s).unwrap(),
        r#"{"relativePath":"../film/a.mp4","displayName":"a.mp4","durationSeconds":10.5,"displayAspect":1.5}"#
    );
}

/// New. `displayAspect` is required: a `0.0` default would fail every aspect
/// gate, so a source without one is malformed rather than silently unprobed.
#[test]
fn a_source_without_display_aspect_is_malformed() {
    let dir = TempDir::new().unwrap();
    write_raw(
        dir.path(),
        json!({
            "formatVersion": 7,
            "name": "x",
            "sourceVideos": [
                {"relativePath": "a.mp4", "displayName": "a", "durationSeconds": 1.0}
            ],
            "clips": []
        }),
    );
    assert!(matches!(
        store::read(dir.path()),
        Err(StoreError::Malformed(_))
    ));
}

// ----------------------------------------------------------- version guard

/// On-disk enum spellings. Nothing else pins them, and a rename would make
/// every existing project unreadable.
#[test]
fn resolution_and_quality_have_the_expected_wire_spellings() {
    assert_eq!(
        serde_json::to_string(&Resolution::R720).unwrap(),
        r#""r720""#
    );
    assert_eq!(
        serde_json::to_string(&Resolution::R1080).unwrap(),
        r#""r1080""#
    );
    assert_eq!(
        serde_json::to_string(&Resolution::R2160).unwrap(),
        r#""r2160""#
    );
    assert_eq!(serde_json::to_string(&Quality::Low).unwrap(), r#""low""#);
    assert_eq!(
        serde_json::to_string(&Quality::Medium).unwrap(),
        r#""medium""#
    );
    assert_eq!(serde_json::to_string(&Quality::High).unwrap(), r#""high""#);
}

/// A JSON document whose root is not an object is not a project at all, and
/// must not be reported as a macOS-era file — `Value::get` returns None for a
/// non-object, which the absent-key rule would otherwise read as v1.
#[test]
fn a_non_object_root_is_malformed_not_legacy() {
    for body in ["[]", "\"hello\"", "42", "null"] {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("project.json"), body).unwrap();
        match store::read(dir.path()) {
            Err(StoreError::Malformed(_)) => {}
            other => panic!("root {body}: expected Malformed, got {other:?}"),
        }
    }
}

#[test]
fn round_trips_through_the_store() {
    let dir = TempDir::new().unwrap();
    let mut p = sample_project();
    store::write(dir.path(), &mut p).unwrap();
    assert_eq!(store::read(dir.path()).unwrap(), p);
}

/// F1. Every version this build reads, as the build that wrote it last left
/// it: a v7 goal with no trim keys, a v8 one with them, and a v9 file with
/// the highlights key. None of them names an avatar or an inset. Every bump
/// keeps a test like this one.
#[test]
fn v7_to_v9_files_load_under_the_current_version() {
    let goal = |version: u32| {
        let mut goal = json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "kind": "homeGoal",
            "sourceIndex": 0,
            "sourceSeconds": 600.0
        });
        if version >= 8 {
            goal["reelLeadIn"] = json!(12.5);
            goal["reelTail"] = serde_json::Value::Null;
        }
        goal
    };
    for version in [7, 8, 9] {
        let dir = TempDir::new().unwrap();
        let mut raw = json!({
            "formatVersion": version,
            "name": "x",
            "sourceVideos": [{
                "relativePath": "a.mp4",
                "displayName": "a",
                "durationSeconds": 2700.0,
                "displayAspect": 1.5
            }],
            "clips": [{
                "id": "00000000-0000-0000-0000-000000000002",
                "name": "Transition",
                "notes": "",
                "tags": [],
                "sourceIndex": 0,
                "startSourceSeconds": 12.5,
                "recordingDuration": 8.0,
                "recordingFilename": "00000000-0000-0000-0000-000000000002.mkv",
                "events": [],
                "showPip": true,
                "sortIndex": 0,
                "createdAt": "2026-09-19T12:00:00Z",
                "transcript": ""
            }],
            "matchEvents": [goal(version)]
        });
        if version >= 9 {
            raw["playerHighlights"] = json!([]);
        }
        write_raw(dir.path(), raw);
        let mut p = store::read(dir.path()).expect("an older file loads");
        assert_eq!(p.format_version, version, "read keeps the version it found");
        assert_eq!(p.match_events.len(), 1);
        let trims = (p.match_events[0].reel_lead_in, p.match_events[0].reel_tail);
        assert_eq!(
            trims,
            if version >= 8 {
                (Some(12.5), None)
            } else {
                (None, None)
            }
        );
        // v9's addition: a file older than it simply has none.
        assert!(p.player_highlights.is_empty());
        // v10's: no avatar key means a camera project, and a clip with no
        // `inset` was recorded on a camera, which is `Inset::Camera`.
        assert_eq!(p.avatar, None);
        assert!(p.clips.iter().all(|c| c.inset == Inset::Camera));

        store::write(dir.path(), &mut p).unwrap();
        assert_eq!(
            store::read(dir.path()).unwrap().format_version,
            CURRENT_FORMAT_VERSION
        );
        assert!(dir.path().join(format!("project.json.v{version}")).exists());
    }
}

/// F1, for v11. A v10 file has no `lastExportScoreboard` key: it loads, the
/// preference reads `None`, and the next save stamps the current version and
/// keeps the v10 file beside it. Every bump owes this test.
#[test]
fn a_v10_file_loads_under_the_current_version() {
    let dir = TempDir::new().unwrap();
    let mut raw = serde_json::to_value(sample_project()).unwrap();
    raw["formatVersion"] = json!(10);
    raw["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("lastExportScoreboard")
        .expect("v11 writes the key this test removes");
    write_raw(dir.path(), raw);

    let mut p = store::read(dir.path()).expect("a v10 file loads");
    assert_eq!(p.preferences.last_export_scoreboard, None);

    // The picker's choice belongs to the match, so it is written with it.
    p.preferences.last_export_scoreboard = Some(ScoreboardMode::Track);
    store::write(dir.path(), &mut p).unwrap();
    assert_eq!(p.format_version, 11);
    assert_eq!(store::read(dir.path()).unwrap(), p);
    assert!(dir.path().join("project.json.v10").exists());

    let text = std::fs::read_to_string(dir.path().join("project.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["formatVersion"], json!(11));
    assert_eq!(value["preferences"]["lastExportScoreboard"], json!("track"));
}

/// v10. The mode is the picture's file name and nothing else, the inset is a
/// clip's own fact, and both survive a write and a read at the wire spellings
/// a v10 file is expected to hold.
#[test]
fn an_avatar_and_an_inset_round_trip() {
    let dir = TempDir::new().unwrap();
    let mut p = sample_project();
    p.avatar = Some("avatar.png".into());
    p.clips[0].inset = Inset::Avatar;
    store::write(dir.path(), &mut p).unwrap();
    assert_eq!(store::read(dir.path()).unwrap(), p);

    let text = std::fs::read_to_string(dir.path().join("project.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["formatVersion"], json!(CURRENT_FORMAT_VERSION));
    assert_eq!(value["avatar"], json!("avatar.png"));
    assert_eq!(value["clips"][0]["inset"], json!("avatar"));

    // And the other way round: the default is spelled out on disk too, so a
    // v10 file has one shape.
    let mut camera = sample_project();
    store::write(dir.path(), &mut camera).unwrap();
    let text = std::fs::read_to_string(dir.path().join("project.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["avatar"], serde_json::Value::Null);
    assert_eq!(value["clips"][0]["inset"], json!("camera"));
}

/// v8. The trims are always written, `null` for the default, so there is one
/// shape on disk; and they come back from the store as they went in.
#[test]
fn reel_trims_round_trip_and_are_always_written() {
    let dir = TempDir::new().unwrap();
    let mut p = sample_project();
    p.match_events[1].reel_tail = Some(4.0);
    store::write(dir.path(), &mut p).unwrap();
    assert_eq!(store::read(dir.path()).unwrap(), p);

    let text = std::fs::read_to_string(dir.path().join("project.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let start_stop = &value["matchEvents"][0];
    assert_eq!(start_stop["reelLeadIn"], serde_json::Value::Null);
    assert!(start_stop.as_object().unwrap().contains_key("reelTail"));
    assert_eq!(value["matchEvents"][1]["reelLeadIn"], json!(12.5));
    assert_eq!(value["matchEvents"][1]["reelTail"], json!(4.0));
}

#[test]
fn swift_era_v6_is_refused() {
    let dir = TempDir::new().unwrap();
    write_raw(
        dir.path(),
        json!({"formatVersion": 6, "name": "x", "sourceVideos": [], "clips": []}),
    );
    match store::read(dir.path()) {
        Err(StoreError::LegacyProject { found, minimum }) => {
            assert_eq!((found, minimum), (6, MIN_READABLE_FORMAT_VERSION));
        }
        other => panic!("expected LegacyProject, got {other:?}"),
    }
}

/// A Swift v1 file has no `formatVersion` key at all. This is the case a
/// field-sniffing guard would have missed, and it is why the version number
/// continues rather than resetting.
#[test]
fn absent_format_version_is_treated_as_v1_and_refused() {
    let dir = TempDir::new().unwrap();
    write_raw(
        dir.path(),
        json!({"name": "x", "sourceVideos": [], "clips": []}),
    );
    match store::read(dir.path()) {
        Err(StoreError::LegacyProject { found, .. }) => assert_eq!(found, 1),
        other => panic!("expected LegacyProject{{found: 1}}, got {other:?}"),
    }
}

/// A JSON float for an integral version must not be misread as v1 and reported
/// as a macOS-era file — a confidently wrong error is the worst kind.
#[test]
fn integral_float_format_version_is_accepted() {
    let dir = TempDir::new().unwrap();
    let mut p = sample_project();
    store::write(dir.path(), &mut p).unwrap();

    let text = std::fs::read_to_string(dir.path().join("project.json")).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["formatVersion"] = json!(f64::from(CURRENT_FORMAT_VERSION));
    write_raw(dir.path(), value);

    assert!(
        store::read(dir.path()).is_ok(),
        "an integral float must not be read as v1"
    );
}

#[test]
fn non_numeric_format_version_is_malformed() {
    let dir = TempDir::new().unwrap();
    write_raw(
        dir.path(),
        json!({"formatVersion": "7", "name": "x", "clips": []}),
    );
    assert!(matches!(
        store::read(dir.path()),
        Err(StoreError::Malformed(_))
    ));
}

#[test]
fn newer_format_is_refused_as_too_new() {
    let dir = TempDir::new().unwrap();
    write_raw(
        dir.path(),
        json!({"formatVersion": CURRENT_FORMAT_VERSION + 1, "name": "x", "sourceVideos": [], "clips": []}),
    );
    match store::read(dir.path()) {
        Err(StoreError::TooNew { found, supported }) => {
            assert_eq!(
                (found, supported),
                (CURRENT_FORMAT_VERSION + 1, CURRENT_FORMAT_VERSION)
            );
        }
        other => panic!("expected TooNew, got {other:?}"),
    }
}

#[test]
fn truncated_json_is_malformed() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("project.json"),
        "{\"formatVersion\": 7, \"na",
    )
    .unwrap();
    assert!(matches!(
        store::read(dir.path()),
        Err(StoreError::Malformed(_))
    ));
}

/// Phase 2 distinguishes "empty folder, create a project" from "unreadable
/// project.json, refuse and do not overwrite". That needs its own variant.
#[test]
fn empty_folder_reports_missing_project_json() {
    let dir = TempDir::new().unwrap();
    assert!(matches!(
        store::read(dir.path()),
        Err(StoreError::MissingProjectJson(_))
    ));
}

// ------------------------------------------------------------ write contract

#[test]
fn write_stamps_the_current_format_version() {
    let dir = TempDir::new().unwrap();
    let mut p = sample_project();
    p.format_version = 1;
    store::write(dir.path(), &mut p).unwrap();
    assert_eq!(p.format_version, CURRENT_FORMAT_VERSION);
    assert_eq!(
        store::read(dir.path()).unwrap().format_version,
        CURRENT_FORMAT_VERSION
    );
}

/// F1. The first save after an upgrade keeps the file the older build wrote,
/// byte for byte, and never overwrites that copy; a save at the current version
/// makes none.
#[test]
fn an_upgrade_keeps_the_old_file_once() {
    let dir = TempDir::new().unwrap();
    let mut v7 = serde_json::to_value(sample_project()).unwrap();
    v7["formatVersion"] = json!(7);
    write_raw(dir.path(), v7);
    let original = std::fs::read(dir.path().join("project.json")).unwrap();
    let backup = dir.path().join("project.json.v7");

    let mut p = store::read(dir.path()).unwrap();
    store::write(dir.path(), &mut p).unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), original);

    // Read at v7 again (the file on disk is now current, so fake it): the
    // backup already exists and is left alone.
    p.format_version = 7;
    p.name = "Renamed".into();
    store::write(dir.path(), &mut p).unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), original);

    // A project read at the current version makes no backup.
    let current = TempDir::new().unwrap();
    let mut p = sample_project();
    store::write(current.path(), &mut p).unwrap();
    let mut p = store::read(current.path()).unwrap();
    store::write(current.path(), &mut p).unwrap();
    let names: Vec<_> = std::fs::read_dir(current.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        names
            .iter()
            .all(|n| !n.to_string_lossy().starts_with("project.json.v")),
        "got {names:?}"
    );
}

/// A crash mid-copy leaves the backup's temporary file behind, and no backup.
/// The next save still makes the backup, from the file as it stands.
#[test]
fn a_stale_backup_temp_file_does_not_block_the_backup() {
    let dir = TempDir::new().unwrap();
    let mut v7 = serde_json::to_value(sample_project()).unwrap();
    v7["formatVersion"] = json!(7);
    write_raw(dir.path(), v7);
    let original = std::fs::read(dir.path().join("project.json")).unwrap();
    let stale = dir.path().join(".project.json.v7.tmp");
    std::fs::write(&stale, b"{\"half\": ").unwrap();

    let mut p = store::read(dir.path()).unwrap();
    store::write(dir.path(), &mut p).unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("project.json.v7")).unwrap(),
        original
    );
    assert!(!stale.exists(), "the temporary file is renamed away");
}

#[test]
fn write_creates_the_recordings_directory() {
    let dir = TempDir::new().unwrap();
    let mut p = sample_project();
    store::write(dir.path(), &mut p).unwrap();
    assert!(dir.path().join("recordings").is_dir());
}

#[test]
fn write_into_a_missing_folder_fails_and_creates_nothing() {
    let dir = TempDir::new().unwrap();
    let gone = dir.path().join("gone");
    let mut p = sample_project();
    match store::write(&gone, &mut p) {
        Err(StoreError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected Io(NotFound), got {other:?}"),
    }
    assert!(!gone.exists(), "the project folder was recreated");
}

#[test]
fn write_tolerates_an_existing_recordings_directory() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("recordings")).unwrap();
    let mut p = sample_project();
    store::write(dir.path(), &mut p).unwrap();
    assert_eq!(store::read(dir.path()).unwrap(), p);
}

#[test]
fn second_write_does_not_corrupt_the_file() {
    let dir = TempDir::new().unwrap();
    let mut p = sample_project();
    store::write(dir.path(), &mut p).unwrap();
    p.name = "Renamed".into();
    store::write(dir.path(), &mut p).unwrap();

    let back = store::read(dir.path()).unwrap();
    assert_eq!(back.name, "Renamed");
    assert_eq!(back.clips.len(), 1);
    // The temp file must not survive the rename.
    assert!(!dir.path().join(".project.json.tmp").exists());
}

// --------------------------------------------------------- virtual timeline

#[test]
fn cumulative_offset_accumulates_preceding_sources() {
    let mut p = Project::new("p");
    for d in [10.0, 20.0, 30.0] {
        p.source_videos.push(SourceRef {
            relative_path: "x.mp4".into(),
            display_name: "x".into(),
            duration_seconds: d,
            display_aspect: 16.0 / 9.0,
        });
    }
    assert_eq!(p.cumulative_offset(0), 0.0);
    assert_eq!(p.cumulative_offset(1), 10.0);
    assert_eq!(p.cumulative_offset(2), 30.0);
    assert_eq!(p.total_source_duration(), 60.0);
}

/// Clamped at the top, so an index past the end returns the total rather than
/// panicking. (Swift also clamped the bottom; `usize` makes that unreachable,
/// so the `-1` case is deliberately not ported — it would prove nothing.)
#[test]
fn cumulative_offset_clamps_index_past_the_end() {
    let mut p = Project::new("p");
    p.source_videos.push(SourceRef {
        relative_path: "x.mp4".into(),
        display_name: "x".into(),
        duration_seconds: 10.0,
        display_aspect: 16.0 / 9.0,
    });
    assert_eq!(p.cumulative_offset(99), 10.0);
}

#[test]
fn cumulative_offset_of_empty_project_is_zero() {
    let p = Project::new("p");
    assert_eq!(p.cumulative_offset(0), 0.0);
    assert_eq!(p.cumulative_offset(5), 0.0);
    assert_eq!(p.total_source_duration(), 0.0);
}

#[test]
fn abs_seconds_projects_onto_the_concat_timeline() {
    let mut p = Project::new("p");
    for d in [100.0, 200.0] {
        p.source_videos.push(SourceRef {
            relative_path: "x.mp4".into(),
            display_name: "x".into(),
            duration_seconds: d,
            display_aspect: 16.0 / 9.0,
        });
    }
    assert_eq!(p.abs_seconds(0, 5.0), 5.0);
    assert_eq!(p.abs_seconds(1, 5.0), 105.0);
}

// ---- add_recorded_clip (new; macOS built clips inline in ContentView) ----

fn pending(start_source_seconds: f64) -> PendingClip {
    PendingClip {
        id: Uuid::from_u128(0x1234),
        source_index: 1,
        start_source_seconds,
    }
}

#[test]
fn a_recorded_clip_is_built_from_the_pending_clip() {
    let mut p = Project::new("p");
    let events = vec![CommentaryEvent::new(0.0, EventKind::ClearAll)];
    let c = p
        .add_recorded_clip(
            pending(3725.9),
            42.5,
            events.clone(),
            "2026-09-19T12:00:00Z".into(),
        )
        .clone();
    assert_eq!(c.id, Uuid::from_u128(0x1234));
    // 3725.9 s floors to 1 h 2 min 5 s; the source number is 1-based.
    assert_eq!(c.name, "2-01:02:05");
    assert_eq!(
        c.recording_filename,
        "00000000-0000-0000-0000-000000001234.mkv"
    );
    assert_eq!(c.source_index, 1);
    assert_eq!(c.start_source_seconds, 3725.9);
    assert_eq!(c.recording_duration, 42.5);
    assert_eq!(c.events, events);
    assert_eq!(c.created_at, "2026-09-19T12:00:00Z");
    assert!(c.notes.is_empty() && c.tags.is_empty() && c.transcript.is_empty());
    assert_eq!(c.sort_index, 0, "the first clip");
    assert_eq!(p.clips, vec![c]);
}

/// Clips are kept in order with `sort_index == position` (Phase 3 spec C3),
/// so a file with gaps, ties or an unsorted array (Phase 4 wrote `max + 1`) is
/// normalized on read, stably, and a recorded clip is appended after it.
#[test]
fn clip_order_is_normalized_on_read_and_a_recorded_clip_appends() {
    let dir = TempDir::new().unwrap();
    let mut p = sample_project();
    let clip = |n: u128, sort_index: i64| Clip {
        id: Uuid::from_u128(n),
        sort_index,
        ..sample_clip()
    };
    p.clips = vec![clip(1, 9), clip(2, 5), clip(3, 0), clip(4, 5)];
    store::write(dir.path(), &mut p).unwrap();

    let mut read = store::read(dir.path()).unwrap();
    let order: Vec<(u128, i64)> = read
        .clips
        .iter()
        .map(|c| (c.id.as_u128(), c.sort_index))
        .collect();
    assert_eq!(order, [(3, 0), (2, 1), (4, 2), (1, 3)]);

    let c = read.add_recorded_clip(pending(0.0), 1.0, Vec::new(), String::new());
    assert_eq!(c.sort_index, 4);
    assert_eq!(read.clips.last().unwrap().id, Uuid::from_u128(0x1234));
}

#[test]
fn show_pip_comes_from_preferences() {
    let mut p = Project::new("p");
    assert!(
        p.add_recorded_clip(pending(0.0), 1.0, Vec::new(), String::new())
            .show_pip
    );
    p.preferences.pip_for_new_recordings = false;
    assert!(
        !p.add_recorded_clip(pending(0.0), 1.0, Vec::new(), String::new())
            .show_pip
    );
}

/// B1: the picture *is* the mode, so a take records the inset the project was
/// in when it was made, and a project holding both kinds renders each clip the
/// way it was recorded.
#[test]
fn the_inset_of_a_new_clip_comes_from_the_projects_avatar() {
    let mut p = Project::new("p");
    assert_eq!(
        p.add_recorded_clip(pending(0.0), 1.0, Vec::new(), String::new())
            .inset,
        Inset::Camera
    );
    p.avatar = Some("avatar.png".into());
    assert_eq!(
        p.add_recorded_clip(pending(0.0), 1.0, Vec::new(), String::new())
            .inset,
        Inset::Avatar
    );
    p.avatar = None;
    assert_eq!(
        p.add_recorded_clip(pending(0.0), 1.0, Vec::new(), String::new())
            .inset,
        Inset::Camera,
        "removing the picture puts new takes back on the camera"
    );
}

/// B3: the one reading of `show_pip` × `inset`, all four combinations. The
/// two can never both be true — one inset is drawn, or none is.
#[test]
fn one_predicate_per_inset_and_never_both() {
    let mut clip = sample_clip();
    for (show_pip, inset, camera, avatar) in [
        (true, Inset::Camera, true, false),
        (true, Inset::Avatar, false, true),
        (false, Inset::Camera, false, false),
        (false, Inset::Avatar, false, false),
    ] {
        clip.show_pip = show_pip;
        clip.inset = inset;
        assert_eq!(clip.shows_camera_pip(), camera, "{show_pip} {inset:?}");
        assert_eq!(clip.shows_avatar(), avatar, "{show_pip} {inset:?}");
        assert!(!(clip.shows_camera_pip() && clip.shows_avatar()));
    }
}
