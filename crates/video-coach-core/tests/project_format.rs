//! Project format v7: defaults, the version guard, and the store's contract.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use video_coach_core::project::{Clip, Preferences, Project, Quality, Resolution, SourceRef};
use video_coach_core::scoreboard_config::{
    MatchEventKind, MatchEventRecord, MatchFormat, ScoreboardConfig, TeamConfig,
};
use video_coach_core::store::{self, StoreError, CURRENT_FORMAT_VERSION};
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
    });
    p.match_events.push(MatchEventRecord {
        id: Uuid::nil(),
        kind: MatchEventKind::StartStop,
        source_index: 0,
        source_seconds: 0.0,
        is_auto_back_anchor: false,
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

#[test]
fn swift_era_v6_is_refused() {
    let dir = TempDir::new().unwrap();
    write_raw(
        dir.path(),
        json!({"formatVersion": 6, "name": "x", "sourceVideos": [], "clips": []}),
    );
    match store::read(dir.path()) {
        Err(StoreError::LegacyProject { found, minimum }) => {
            assert_eq!((found, minimum), (6, CURRENT_FORMAT_VERSION));
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
    value["formatVersion"] = json!(7.0);
    write_raw(dir.path(), value);

    assert!(
        store::read(dir.path()).is_ok(),
        "7.0 must not be read as v1"
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
        json!({"formatVersion": 8, "name": "x", "sourceVideos": [], "clips": []}),
    );
    assert!(matches!(
        store::read(dir.path()),
        Err(StoreError::TooNew { found: 8, .. })
    ));
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

#[test]
fn write_creates_the_recordings_directory() {
    let dir = TempDir::new().unwrap();
    let mut p = sample_project();
    store::write(dir.path(), &mut p).unwrap();
    assert!(dir.path().join("recordings").is_dir());
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
        });
    }
    assert_eq!(p.abs_seconds(0, 5.0), 5.0);
    assert_eq!(p.abs_seconds(1, 5.0), 105.0);
}
