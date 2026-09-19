//! The encode side: `appsrc` → zoom → letterbox → NV12 → H.264 → MP4, and the
//! encoder choice (spec X2, X3).

use std::path::Path;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use video_coach_core::export::OUTPUT_FPS;
use video_coach_core::zoom::Zoom;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{ExportError, SharedGl, Stopper, Watch, POLL};

/// The output frame size.
pub(super) const OUTPUT_WIDTH: i32 = 1920;
pub(super) const OUTPUT_HEIGHT: i32 = 1080;
/// The one quality setting until Phase 8's picker: a quantizer, since the
/// hardware encoder is CQP-only. QP 24 is ~11 Mbps on camera footage.
const QP: u32 = 24;
/// Frames `appsrc` may hold before the pump waits for room.
const QUEUED: u64 = 4;

/// The H.264 encoders export can use, in preference order, with their
/// launch-string settings. Only encoders someone has run are listed (spec X3).
fn encoders() -> [(&'static str, String); 2] {
    [
        (
            "vah264lpenc",
            format!("rate-control=cqp qpi={QP} qpp={QP} key-int-max=60"),
        ),
        // Constant quality: smaller than constant QP at the same quality.
        // `medium` runs at 0.39x realtime; `veryfast` keeps up.
        (
            "x264enc",
            format!("pass=qual quantizer={QP} speed-preset=veryfast key-int-max=60"),
        ),
    ]
}

pub(super) struct Encoder {
    /// Held to go to NULL with the encoder.
    _pipeline: Stopper,
    appsrc: gst_app::AppSrc,
    name: &'static str,
    /// Set when the file is complete.
    eos: Arc<AtomicBool>,
}

impl Encoder {
    /// Builds the graph for frames shaped like `first` (the decoder's),
    /// writing to `part`, and sets it PLAYING. Output frame `n` gets
    /// `zooms[n]`. Its errors reach `watch`. `inject` is spliced in before the
    /// encoder (tests).
    ///
    /// The encoder is the first of [`encoders`] installed. A presence check
    /// only: one that fails at start fails the export (BACKLOG #39).
    pub(super) fn start(
        first: &gst::Sample,
        part: &Path,
        zooms: Vec<Zoom>,
        gl: &SharedGl,
        inject: Option<&str>,
        watch: &Watch,
    ) -> Result<Encoder, ExportError> {
        let (name, settings) = encoders()
            .into_iter()
            .find(|(name, _)| gst::ElementFactory::find(name).is_some())
            .ok_or_else(|| {
                ExportError::Failed(
                    "no H.264 encoder: install gst-plugins-ugly (x264enc) or VA drivers".into(),
                )
            })?;
        let inject = inject.map(|i| format!("{i} ! ")).unwrap_or_default();
        // The mixer's output size is its pads' bounding box and its rate the
        // input's, so both are pinned after it. The readback before the
        // encoder is required, and so is the queue.
        let description = format!(
            "appsrc name=src format=time is-live=false block=false \
               max-buffers={QUEUED} max-bytes=0 max-time=0 \
             ! gltransformation name=zoom ortho=true \
             ! glvideomixer name=mix background=black \
             ! video/x-raw(memory:GLMemory),width={OUTPUT_WIDTH},height={OUTPUT_HEIGHT},\
               framerate={OUTPUT_FPS}/1,pixel-aspect-ratio=1/1 \
             ! glcolorconvert ! video/x-raw(memory:GLMemory),format=NV12 \
             ! gldownload ! video/x-raw,format=NV12 ! queue \
             ! {inject}{name} {settings} \
             ! h264parse ! video/x-h264,profile=high,stream-format=avc,alignment=au \
             ! mp4mux name=mux ! filesink name=out"
        );
        let pipeline = gst::parse::launch(&description)
            .map_err(|e| ExportError::Failed(format!("could not build the export graph: {e}")))?
            .downcast::<gst::Pipeline>()
            .expect("a multi-element launch string yields a pipeline");
        let by_name = |n: &str| pipeline.by_name(n).expect("named in the launch string");

        let caps = first
            .caps()
            .ok_or_else(|| ExportError::Failed("a decoded frame has no caps".into()))?;
        let info = gst_video::VideoInfo::from_caps(caps)
            .map_err(|e| ExportError::Failed(format!("unusable decoded caps {caps}: {e}")))?;
        let mut caps = caps.to_owned();
        caps.make_mut()
            .set("framerate", gst::Fraction::new(OUTPUT_FPS as i32, 1));
        let appsrc = by_name("src")
            .downcast::<gst_app::AppSrc>()
            .expect("`src` is an appsrc");
        appsrc.set_caps(Some(&caps));

        let (x, y, w, h) = fit_rect(&info);
        let mix_pad = by_name("mix")
            .static_pad("sink_0")
            .expect("the mixer's first pad is linked");
        mix_pad.set_property("xpos", x);
        mix_pad.set_property("ypos", y);
        mix_pad.set_property("width", w);
        mix_pad.set_property("height", h);
        // `moov` goes first, in space reserved up front, with no temp file
        // (`faststart` writes the whole `mdat` to `$TMPDIR`, which a crash
        // leaks). The reserve must cover the whole file, so it gets a margin.
        let duration = frame_time(zooms.len() as u64);
        install_zoom(&by_name("zoom"), zooms);
        by_name("mux").set_property(
            "reserved-max-duration",
            (duration + duration / 10 + gst::ClockTime::SECOND).nseconds(),
        );
        by_name("out").set_property("location", part);

        let eos = gl.install(&pipeline, watch);
        let pipeline = Stopper(pipeline);
        if pipeline.set_state(gst::State::Playing).is_err() {
            return Err(watch.failure("could not start the encoder"));
        }
        Ok(Encoder {
            _pipeline: pipeline,
            appsrc,
            name,
            eos,
        })
    }

    /// The encoder's factory name.
    pub(super) fn name(&self) -> &'static str {
        self.name
    }

    /// Pushes `sample`'s buffer as output frame `n`. A reference, not a pixel
    /// copy: the same GL texture may go out many times.
    pub(super) fn push(
        &self,
        n: u64,
        sample: &gst::Sample,
        watch: &Watch,
    ) -> Result<(), ExportError> {
        let mut out = sample
            .buffer()
            .expect("the decoder keeps only samples with a buffer")
            .copy();
        {
            let out = out.get_mut().expect("a fresh copy is writable");
            out.set_pts(frame_time(n));
            out.set_dts(gst::ClockTime::NONE);
            out.set_duration(frame_time(n + 1) - frame_time(n));
        }
        // `block=false` never waits, so wait here, where errors are seen.
        while self.appsrc.current_level_buffers() >= QUEUED {
            watch.check()?;
            std::thread::sleep(Duration::from(POLL) / 5);
        }
        self.appsrc
            .push_buffer(out)
            .map_err(|e| watch.failure(format!("pushing frame {n}: {e:?}")))?;
        Ok(())
    }

    /// Ends the stream and waits for the muxer to finish the file.
    pub(super) fn finish(&self, watch: &Watch) -> Result<(), ExportError> {
        let _ = self.appsrc.end_of_stream();
        loop {
            // Read before the check: an error is recorded before any EOS.
            let done = self.eos.load(Ordering::SeqCst);
            watch.check()?;
            if done {
                return Ok(());
            }
            std::thread::sleep(POLL.into());
        }
    }
}

/// Output frame `n`'s time, `n/30` s, floored to the nanosecond.
fn frame_time(n: u64) -> gst::ClockTime {
    gst::ClockTime::SECOND
        .mul_div_floor(n, u64::from(OUTPUT_FPS))
        .expect("no overflow")
}

/// Sets each buffer's zoom on `transform` as it arrives, keyed on its PTS, so
/// the value matches the frame whatever is queued.
///
/// And makes `transform` render the zoom itself. Offered the choice (by
/// `glvideomixer`), `gltransformation` passes frames through with an affine
/// transformation meta, and the mixer draws the transformed quad unclipped:
/// a zoomed 4:3 source spills into its pillarbox bars (measured). Rendered
/// into its own source-sized texture, the zoom is clipped to the picture.
fn install_zoom(transform: &gst::Element, zooms: Vec<Zoom>) {
    transform
        .static_pad("src")
        .expect("gltransformation has a src pad")
        .add_probe(
            gst::PadProbeType::QUERY_DOWNSTREAM | gst::PadProbeType::PULL,
            |_, info| {
                if let Some(gst::PadProbeData::Query(query)) = &mut info.data {
                    if let gst::QueryViewMut::Allocation(allocation) = query.view_mut() {
                        while let Some(i) = allocation
                            .find_allocation_meta::<gst_video::VideoAffineTransformationMeta>()
                        {
                            allocation.remove_nth_allocation_meta(i);
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            },
        );
    let weak = transform.downgrade();
    transform
        .static_pad("sink")
        .expect("gltransformation has a sink pad")
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            let (Some(pts), Some(transform)) =
                (info.buffer().and_then(|b| b.pts()), weak.upgrade())
            else {
                return gst::PadProbeReturn::Ok;
            };
            let n = pts
                .nseconds()
                .saturating_mul(u64::from(OUTPUT_FPS))
                .saturating_add(500_000_000)
                / 1_000_000_000;
            if let Some(zoom) = zooms.get(n as usize) {
                let (s, tx, ty) = zoom_params(*zoom);
                transform.set_property("scale-x", s);
                transform.set_property("scale-y", s);
                transform.set_property("translation-x", tx);
                transform.set_property("translation-y", ty);
            }
            gst::PadProbeReturn::Ok
        });
}

/// `gltransformation`'s `(scale, translation-x, translation-y)` for `zoom`,
/// placed before the mixer and rendering into a texture of the source's own
/// size (see [`install_zoom`]).
///
/// The visible centre is the source point `(0.5 + pan_x, 0.5 + pan_y)` (y
/// down). Translation is applied after the scale, in units of the picture's
/// width and height, and positive `translation-y` moves the image **down**
/// (measured at s = 0.25, 0.5 and 2 on 4:3 and 16:9). The spec's mapping,
/// with y the other way, was measured on the affine-meta path, which doesn't
/// clip.
pub(super) fn zoom_params(zoom: Zoom) -> (f32, f32, f32) {
    let s = zoom.scale;
    (s as f32, (-zoom.pan_x * s) as f32, (-zoom.pan_y * s) as f32)
}

/// The source's fit rect inside the output, `(x, y, width, height)`: its
/// display aspect (size and PAR, from the decoded caps) letterboxed or
/// pillarboxed into 1920×1080.
pub(super) fn fit_rect(info: &gst_video::VideoInfo) -> (i32, i32, i32, i32) {
    let par = info.par();
    let (par_n, par_d) = if par.numer() > 0 && par.denom() > 0 {
        (par.numer(), par.denom())
    } else {
        (1, 1)
    };
    let aspect =
        f64::from(info.width()) * f64::from(par_n) / (f64::from(info.height()) * f64::from(par_d));
    let (ow, oh) = (f64::from(OUTPUT_WIDTH), f64::from(OUTPUT_HEIGHT));
    let (w, h) = if aspect >= ow / oh {
        (OUTPUT_WIDTH, (ow / aspect).round() as i32)
    } else {
        ((oh * aspect).round() as i32, OUTPUT_HEIGHT)
    };
    ((OUTPUT_WIDTH - w) / 2, (OUTPUT_HEIGHT - h) / 2, w, h)
}
