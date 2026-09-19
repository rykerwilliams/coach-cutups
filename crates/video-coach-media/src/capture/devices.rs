//! Capture devices: PipeWire enumeration keyed on `node.name` (spec R2), the
//! camera-mode rule (R3) and the encoder chain (R4).
//!
//! The choices are pure functions over caps and element availability, so they
//! are tested with caps copied from the reference laptop and no hardware.

use gstreamer as gst;
use gstreamer::prelude::*;

/// The only frame rate the app records at.
const FPS: gst::Fraction = gst::Fraction::from_integer(30);
/// The widest mode the app records. 1280×720 is the only mode on the reference
/// webcam that is 16:9 at 30 fps; larger ones would only cost encode time.
const MAX_WIDTH: i32 = 1280;

/// What the camera sends, which decides the head of the encode chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// `image/jpeg`: needs a JPEG decode.
    Mjpeg,
    /// `video/x-raw` in any format but GRAY8: needs a `videoconvert`.
    Raw,
}

/// The mode a camera is recorded in: always 30/1 and exactly 16:9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraMode {
    pub width: i32,
    pub height: i32,
    pub input: Input,
}

/// The largest exactly-16:9 mode no wider than 1280 that offers 30/1 (R3), or
/// `None` if the camera has none. GRAY8 is never usable, so the IR camera gets
/// `None`. On a tie the device's first listing wins.
pub fn choose_camera_mode(caps: &gst::Caps) -> Option<CameraMode> {
    let mut best: Option<CameraMode> = None;
    for s in caps.iter() {
        let input = match s.name().as_str() {
            "image/jpeg" => Input::Mjpeg,
            "video/x-raw" if s.get::<&str>("format").ok() != Some("GRAY8") => Input::Raw,
            _ => continue,
        };
        // Ranges and lists of sizes are not something a UVC camera reports;
        // a structure without fixed sizes is skipped.
        let (Ok(width), Ok(height)) = (s.get::<i32>("width"), s.get::<i32>("height")) else {
            continue;
        };
        if width * 9 != height * 16 || width > MAX_WIDTH || !offers_30fps(s) {
            continue;
        }
        if best.is_none_or(|b| width > b.width) {
            best = Some(CameraMode {
                width,
                height,
                input,
            });
        }
    }
    best
}

/// Whether `framerate` is 30/1, lists it, or is a range containing it.
fn offers_30fps(s: &gst::StructureRef) -> bool {
    let Ok(value) = s.value("framerate") else {
        return false;
    };
    if let Ok(f) = value.get::<gst::Fraction>() {
        f == FPS
    } else if let Ok(list) = value.get::<gst::List>() {
        list.iter().any(|v| v.get::<gst::Fraction>() == Ok(FPS))
    } else if let Ok(range) = value.get::<gst::FractionRange>() {
        range.min() <= FPS && FPS <= range.max()
    } else {
        false
    }
}

/// Which H.264 encoder a recording uses (R4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderChain {
    /// `vah264lpenc`, 8% CPU at 720p30 on the reference laptop.
    Va,
    /// `x264enc`, 110% CPU at 720p30: the fallback where VA is missing (CI).
    Software,
}

/// VA when `vah264lpenc` exists, plus `vajpegdec` for MJPEG; software
/// otherwise. A presence check only: a chain that fails at start takes the
/// recording error path. `vaapih264enc` is never used (it hung a harness).
pub fn choose_encoder(has: impl Fn(&str) -> bool, input: Input) -> EncoderChain {
    let va = has("vah264lpenc") && (input == Input::Raw || has("vajpegdec"));
    if va {
        EncoderChain::Va
    } else {
        EncoderChain::Software
    }
}

type Props = &'static [(&'static str, &'static str)];

impl EncoderChain {
    /// The chain's elements, camera side first, with the properties each is
    /// given (as strings, so enum and flag nicks parse the way `gst-launch`
    /// parses them).
    pub fn elements(self, input: Input) -> Vec<(&'static str, Props)> {
        // `vah264lpenc` is CQP-only on the reference driver: bitrate settings
        // are ignored there.
        const VA: Props = &[
            ("rate-control", "cqp"),
            ("qpi", "24"),
            ("qpp", "26"),
            ("key-int-max", "30"),
        ];
        const X264: Props = &[
            ("speed-preset", "ultrafast"),
            ("tune", "zerolatency"),
            ("key-int-max", "30"),
        ];
        match (self, input) {
            (EncoderChain::Va, Input::Mjpeg) => vec![("vajpegdec", &[]), ("vah264lpenc", VA)],
            // `vah264lpenc` accepts NV12 only; YUY2 won't link.
            (EncoderChain::Va, Input::Raw) => vec![("videoconvert", &[]), ("vah264lpenc", VA)],
            (EncoderChain::Software, Input::Mjpeg) => {
                vec![("jpegdec", &[]), ("videoconvert", &[]), ("x264enc", X264)]
            }
            (EncoderChain::Software, Input::Raw) => {
                vec![("videoconvert", &[]), ("x264enc", X264)]
            }
        }
    }

    /// Makes the chain's elements, adds them to `bin` and links them. Returns
    /// the `(head, tail)` for the caller to link the camera and parser to.
    pub fn build(
        self,
        input: Input,
        bin: &gst::Bin,
    ) -> Result<(gst::Element, gst::Element), gst::glib::BoolError> {
        let elements = self
            .elements(input)
            .into_iter()
            .map(|(factory, props)| {
                let element = gst::ElementFactory::make(factory).build()?;
                for (name, value) in props {
                    element.set_property_from_str(name, value);
                }
                Ok(element)
            })
            .collect::<Result<Vec<_>, gst::glib::BoolError>>()?;
        bin.add_many(&elements)?;
        gst::Element::link_many(&elements)?;
        let head = elements.first().expect("every chain has elements").clone();
        let tail = elements.last().expect("every chain has elements").clone();
        Ok((head, tail))
    }
}

/// A camera the app can record from.
#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    /// PipeWire's `node.name`: stable across reboots, the stored preference
    /// key (R2).
    pub node_name: String,
    /// The display name. Not unique: the reference laptop's webcam and IR
    /// camera share one.
    pub label: String,
    /// `/dev/videoN` for `v4l2src`. Volatile: resolved at record time, never
    /// stored.
    pub v4l2_path: String,
    pub mode: CameraMode,
    /// PipeWire's `priority.session`. The highest is the system default.
    pub priority: i32,
}

/// A microphone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mic {
    /// PipeWire's `node.name`, given to `pipewiresrc` as `target-object`.
    pub node_name: String,
    pub label: String,
}

/// What [`list_devices`] found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Devices {
    /// Only cameras with a usable mode (R3), highest priority first.
    pub cameras: Vec<Camera>,
    pub mics: Vec<Mic>,
}

/// Enumerates cameras and microphones with a short-lived `DeviceMonitor`
/// (R2). By default it sees only PipeWire devices, which hide their v4l2 and
/// pulse duplicates. Listing opens no device. Takes ~250 ms the first time in
/// a process and ~36 ms after.
///
/// Devices without a `node.name` are dropped: that is the only key a
/// preference can hold. Cameras without an `api.v4l2.path` are dropped too,
/// since the recorder captures with `v4l2src`.
pub fn list_devices() -> Devices {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Source"), None);
    monitor.add_filter(Some("Audio/Source"), None);
    if monitor.start().is_err() {
        return Devices::default();
    }
    let found = monitor.devices();
    monitor.stop();

    let mut devices = Devices::default();
    for device in found {
        let Some(props) = device.properties() else {
            continue;
        };
        let Ok(node_name) = props.get::<String>("node.name") else {
            continue;
        };
        let label = device.display_name().to_string();
        if device.has_classes("Video/Source") {
            let (Ok(v4l2_path), Some(mode)) = (
                props.get::<String>("api.v4l2.path"),
                device.caps().as_ref().and_then(choose_camera_mode),
            ) else {
                continue;
            };
            devices.cameras.push(Camera {
                node_name,
                label,
                v4l2_path,
                mode,
                // PipeWire gives every property as a string.
                priority: props
                    .get::<&str>("priority.session")
                    .ok()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(0),
            });
        } else if device.has_classes("Audio/Source") {
            devices.mics.push(Mic { node_name, label });
        }
    }
    devices
        .cameras
        .sort_by_key(|c| std::cmp::Reverse(c.priority));
    devices
}

/// The camera to record with: `preferred` if it is listed, else the first
/// (highest-priority) camera. The flag is true when a preference was set but
/// is absent, so the caller shows a notice; the preference is kept (R2).
/// `None` when no camera is usable.
pub fn resolve_camera<'a>(
    cameras: &'a [Camera],
    preferred: Option<&str>,
) -> Option<(&'a Camera, bool)> {
    let found = preferred.and_then(|p| cameras.iter().find(|c| c.node_name == p));
    match found {
        Some(camera) => Some((camera, false)),
        None => cameras.first().map(|c| (c, preferred.is_some())),
    }
}

/// The `target-object` for `pipewiresrc`: `preferred` if it is listed, else
/// `None`, which lets PipeWire's session manager pick the default mic. The
/// flag is true when a preference was set but is absent.
pub fn resolve_mic<'a>(mics: &[Mic], preferred: Option<&'a str>) -> (Option<&'a str>, bool) {
    match preferred {
        Some(p) if mics.iter().any(|m| m.node_name == p) => (Some(p), false),
        Some(_) => (None, true),
        None => (None, false),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    /// The reference laptop's RGB webcam, from `gst-device-monitor-1.0
    /// Video/Source`.
    const WEBCAM: &str = "image/jpeg, width=1280, height=720, framerate=30/1; \
        image/jpeg, width=960, height=540, framerate=30/1; \
        image/jpeg, width=848, height=480, framerate=30/1; \
        image/jpeg, width=640, height=480, framerate=30/1; \
        image/jpeg, width=640, height=360, framerate=30/1; \
        video/x-raw, format=YUY2, width=640, height=480, framerate=30/1; \
        video/x-raw, format=YUY2, width=640, height=360, framerate=30/1; \
        video/x-raw, format=YUY2, width=424, height=240, framerate=30/1; \
        video/x-raw, format=YUY2, width=320, height=240, framerate=30/1; \
        video/x-raw, format=YUY2, width=320, height=180, framerate=30/1; \
        video/x-raw, format=YUY2, width=160, height=120, framerate=30/1";
    /// Its IR face-unlock camera, which shares the webcam's display name.
    const IR: &str = "video/x-raw, format=GRAY8, width=640, height=360, framerate=30/1";

    fn mode(caps: &str) -> Option<CameraMode> {
        gst::init().unwrap();
        choose_camera_mode(&gst::Caps::from_str(caps).unwrap())
    }

    fn mjpeg(width: i32, height: i32) -> Option<CameraMode> {
        Some(CameraMode {
            width,
            height,
            input: Input::Mjpeg,
        })
    }

    #[test]
    fn webcam_records_720p_mjpeg() {
        assert_eq!(mode(WEBCAM), mjpeg(1280, 720));
    }

    #[test]
    fn gray8_is_never_usable() {
        assert_eq!(mode(IR), None);
    }

    #[test]
    fn near_16_9_is_not_16_9() {
        assert_eq!(
            mode(
                "image/jpeg, width=848, height=480, framerate=30/1; \
                  video/x-raw, format=YUY2, width=424, height=240, framerate=30/1"
            ),
            None
        );
    }

    #[test]
    fn camera_without_a_16_9_mode_is_refused() {
        assert_eq!(
            mode(
                "image/jpeg, width=640, height=480, framerate=30/1; \
                  video/x-raw, format=YUY2, width=320, height=240, framerate=30/1"
            ),
            None
        );
    }

    #[test]
    fn wider_than_1280_and_not_30fps_are_skipped() {
        assert_eq!(
            mode(
                "image/jpeg, width=1920, height=1080, framerate=30/1; \
                  image/jpeg, width=1280, height=720, framerate=15/1; \
                  video/x-raw, format=YUY2, width=640, height=360, framerate=30/1"
            ),
            Some(CameraMode {
                width: 640,
                height: 360,
                input: Input::Raw,
            })
        );
    }

    #[test]
    fn framerate_list_and_range() {
        assert_eq!(
            mode("image/jpeg, width=1280, height=720, framerate={ 30/1, 15/1 }"),
            mjpeg(1280, 720)
        );
        assert_eq!(
            mode("image/jpeg, width=1280, height=720, framerate={ 60/1, 15/1 }"),
            None
        );
        assert_eq!(
            mode("image/jpeg, width=960, height=540, framerate=[ 5/1, 30/1 ]"),
            mjpeg(960, 540)
        );
        assert_eq!(
            mode("image/jpeg, width=960, height=540, framerate=[ 5/1, 15/1 ]"),
            None
        );
    }

    fn names(chain: EncoderChain, input: Input) -> Vec<&'static str> {
        chain.elements(input).into_iter().map(|(n, _)| n).collect()
    }

    #[test]
    fn va_needs_the_jpeg_decoder_only_for_mjpeg() {
        let all = |_: &str| true;
        let no_jpeg = |f: &str| f != "vajpegdec";
        let none = |_: &str| false;
        assert_eq!(choose_encoder(all, Input::Mjpeg), EncoderChain::Va);
        assert_eq!(choose_encoder(all, Input::Raw), EncoderChain::Va);
        assert_eq!(
            choose_encoder(no_jpeg, Input::Mjpeg),
            EncoderChain::Software
        );
        assert_eq!(choose_encoder(no_jpeg, Input::Raw), EncoderChain::Va);
        assert_eq!(choose_encoder(none, Input::Mjpeg), EncoderChain::Software);
        assert_eq!(choose_encoder(none, Input::Raw), EncoderChain::Software);
    }

    #[test]
    fn chain_elements() {
        use EncoderChain::*;
        assert_eq!(names(Va, Input::Mjpeg), ["vajpegdec", "vah264lpenc"]);
        // NV12 only: raw input needs a converter.
        assert_eq!(names(Va, Input::Raw), ["videoconvert", "vah264lpenc"]);
        assert_eq!(
            names(Software, Input::Mjpeg),
            ["jpegdec", "videoconvert", "x264enc"]
        );
        assert_eq!(names(Software, Input::Raw), ["videoconvert", "x264enc"]);
        let (_, va) = Va.elements(Input::Raw).pop().unwrap();
        assert!(va.contains(&("rate-control", "cqp")));
    }

    /// Builds the software chain for real, which checks every property name
    /// and value parses. CI has x264 (plugins-ugly); VA is absent there.
    #[test]
    fn software_chain_builds_and_links() {
        gst::init().unwrap();
        let bin = gst::Bin::new();
        let (head, tail) = EncoderChain::Software.build(Input::Mjpeg, &bin).unwrap();
        assert_eq!(head.factory().unwrap().name(), "jpegdec");
        assert_eq!(tail.factory().unwrap().name(), "x264enc");
        assert_eq!(tail.property::<u32>("key-int-max"), 30);
        assert_eq!(bin.children().len(), 3);
    }

    #[test]
    fn va_chain_builds_where_va_exists() {
        gst::init().unwrap();
        let has = |f: &str| gst::ElementFactory::find(f).is_some();
        if choose_encoder(has, Input::Mjpeg) != EncoderChain::Va {
            return; // No `/dev/dri` (CI): the va plugin registers nothing.
        }
        let bin = gst::Bin::new();
        let (_, tail) = EncoderChain::Va.build(Input::Mjpeg, &bin).unwrap();
        assert_eq!(tail.property::<u32>("qpi"), 24);
        assert_eq!(tail.property::<u32>("qpp"), 26);
    }

    fn camera(node_name: &str) -> Camera {
        Camera {
            node_name: node_name.into(),
            label: "Cam".into(),
            v4l2_path: "/dev/video0".into(),
            mode: mjpeg(1280, 720).unwrap(),
            priority: 0,
        }
    }

    #[test]
    fn camera_resolution() {
        let cams = [camera("a"), camera("b")];
        let pick = |p| resolve_camera(&cams, p).map(|(c, fell)| (c.node_name.as_str(), fell));
        assert_eq!(pick(Some("b")), Some(("b", false)));
        assert_eq!(pick(None), Some(("a", false)));
        assert_eq!(pick(Some("gone")), Some(("a", true)));
        assert_eq!(resolve_camera(&[], Some("a")), None);
    }

    #[test]
    fn mic_resolution() {
        let mics = [Mic {
            node_name: "m".into(),
            label: "Mic".into(),
        }];
        assert_eq!(resolve_mic(&mics, Some("m")), (Some("m"), false));
        assert_eq!(resolve_mic(&mics, Some("gone")), (None, true));
        assert_eq!(resolve_mic(&mics, None), (None, false));
    }

    /// Lists the real devices (listing opens none). Run by hand:
    /// `cargo test -p video-coach-media list_real_devices -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn list_real_devices() {
        gst::init().unwrap();
        let devices = list_devices();
        println!("{devices:#?}");
        assert!(!devices.cameras.is_empty());
        assert!(devices.cameras.iter().all(|c| c.priority > 0));
    }
}
