//! Source probing: what the source list needs to know about a file before it
//! is added or relinked (spec D7).
//!
//! A source is accepted only if it has a video stream and is not rotated. The
//! GL display path does not apply `image-orientation`, so a rotated file would
//! play sideways and its stored aspect would be wrong for the aspect gate.
//! macOS rotation-corrected instead; none of the user's footage is rotated, so
//! the port refuses rather than rotating (BACKLOG).

use std::path::Path;

use gstreamer as gst;
use gstreamer_pbutils as pbutils;

/// How long `Discoverer` may spend on one file.
const DISCOVER_TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(10);

/// What a successful probe learns about a source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Probe {
    pub duration_seconds: f64,
    /// Width / height **after** pixel aspect ratio. Stored on `SourceRef` and
    /// used only by the aspect gate; rendering uses the live caps.
    pub display_aspect: f64,
}

/// Why a file cannot be a source. The messages are shown to the user.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    #[error("the file has no video stream")]
    NoVideo,
    /// Carries the offending `image-orientation` tag value.
    #[error("the video is rotated ({0}); rotated video is not supported")]
    Rotated(String),
    /// Missing, not media, truncated, or needs a plugin that isn't installed.
    #[error("the file could not be read as video: {0}")]
    Unreadable(String),
}

/// Probes `path` with `Discoverer`. Blocks for up to 10 seconds.
pub fn probe(path: &Path) -> Result<Probe, ProbeError> {
    let uri = gst::glib::filename_to_uri(path, None)
        .map_err(|e| ProbeError::Unreadable(e.to_string()))?;
    let discoverer = pbutils::Discoverer::new(DISCOVER_TIMEOUT)
        .map_err(|e| ProbeError::Unreadable(e.to_string()))?;
    let info = discoverer
        .discover_uri(&uri)
        .map_err(|e| ProbeError::Unreadable(e.to_string()))?;
    if info.result() != pbutils::DiscovererResult::Ok {
        return Err(ProbeError::Unreadable(format!("{:?}", info.result())));
    }

    let video = info
        .video_streams()
        .into_iter()
        .next()
        .ok_or(ProbeError::NoVideo)?;

    // Deprecated since 1.20 in favour of per-stream and container tag lists,
    // but it is the one list that merges every tag Discoverer saw, whichever
    // scope the muxer or demuxer gave it. `qtmux` fixtures put
    // `image-orientation` on the container, not the video stream.
    #[allow(deprecated)]
    let tags = info.tags();
    let orientation = tags
        .as_ref()
        .and_then(|t| t.get::<gst::tags::ImageOrientation>());
    check_orientation(orientation.as_ref().map(|v| v.get()))?;

    let par = video.par();
    let (par_n, par_d) = if par.numer() > 0 && par.denom() > 0 {
        (par.numer(), par.denom())
    } else {
        (1, 1)
    };
    let display_aspect = f64::from(video.width()) * f64::from(par_n)
        / (f64::from(video.height()) * f64::from(par_d));
    if !display_aspect.is_finite() || display_aspect <= 0.0 {
        return Err(ProbeError::Unreadable(format!(
            "video stream has no usable size ({}x{})",
            video.width(),
            video.height()
        )));
    }

    let duration_seconds = info
        .duration()
        .map(|d| d.nseconds() as f64 / 1e9)
        .filter(|d| *d > 0.0)
        .ok_or_else(|| ProbeError::Unreadable("the file reports no duration".into()))?;

    Ok(Probe {
        duration_seconds,
        display_aspect,
    })
}

/// Accepts a source's global `image-orientation` tag only when it is absent
/// or `rotate-0`. Anything else — `rotate-90`, `flip-rotate-0`, a value this
/// code has never seen — is refused, because the display path applies none of
/// them.
pub fn check_orientation(tag: Option<&str>) -> Result<(), ProbeError> {
    match tag {
        None | Some("rotate-0") => Ok(()),
        Some(other) => Err(ProbeError::Rotated(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn dir() -> tempfile::TempDir {
        gst::init().unwrap();
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn orientation_absent_or_rotate_0_is_accepted() {
        assert_eq!(check_orientation(None), Ok(()));
        assert_eq!(check_orientation(Some("rotate-0")), Ok(()));
    }

    #[test]
    fn orientation_anything_else_is_refused() {
        for tag in [
            "rotate-90",
            "rotate-180",
            "rotate-270",
            "flip-rotate-0",
            "bogus",
        ] {
            assert_eq!(
                check_orientation(Some(tag)),
                Err(ProbeError::Rotated(tag.to_owned())),
                "{tag}"
            );
        }
    }

    #[test]
    fn webm_duration_is_within_one_frame() {
        let dir = dir();
        let path = fixtures::webm(dir.path(), "a.webm", 2, 320, 180, 30, 15);
        let p = probe(&path).unwrap();
        assert!(
            (p.duration_seconds - 2.0).abs() <= 1.0 / 30.0,
            "duration {}",
            p.duration_seconds
        );
    }

    #[test]
    fn aspect_is_within_relative_tenth_of_a_percent() {
        let dir = dir();
        for (w, h, expected) in [(320, 180, 16.0 / 9.0), (320, 240, 4.0 / 3.0)] {
            let path = fixtures::webm(dir.path(), &format!("{w}x{h}.webm"), 1, w, h, 30, 30);
            let aspect = probe(&path).unwrap().display_aspect;
            assert!(
                ((aspect - expected) / expected).abs() < 0.001,
                "{w}x{h}: aspect {aspect}, expected {expected}"
            );
        }
    }

    #[test]
    fn rotated_mp4_is_refused() {
        let dir = dir();
        let path = fixtures::rotated_mp4(dir.path());
        assert_eq!(probe(&path), Err(ProbeError::Rotated("rotate-90".into())));
    }

    #[test]
    fn audio_only_is_no_video() {
        let dir = dir();
        let path = fixtures::audio_only(dir.path());
        assert_eq!(probe(&path), Err(ProbeError::NoVideo));
    }

    #[test]
    fn missing_file_is_unreadable() {
        let dir = dir();
        let result = probe(&dir.path().join("does-not-exist.webm"));
        assert!(
            matches!(result, Err(ProbeError::Unreadable(_))),
            "{result:?}"
        );
    }

    #[test]
    fn non_media_file_is_unreadable() {
        let dir = dir();
        let path = dir.path().join("notes.webm");
        std::fs::write(&path, "this is not a video").unwrap();
        let result = probe(&path);
        assert!(
            matches!(result, Err(ProbeError::Unreadable(_))),
            "{result:?}"
        );
    }
}
