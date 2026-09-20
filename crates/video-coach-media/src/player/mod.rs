//! The source player: one long-lived `playbin3` holding one source at a time,
//! with the load sequence and a single-flight, latest-wins seek slot inside
//! (spec D1, D3, D4, D8).
//!
//! The player is driven from one thread — the bus thread. The sync handler
//! forwards every GStreamer message through `on_message`; the driving thread
//! feeds each one back into [`SourcePlayer::handle`], which turns it into
//! [`PlayerEvent`]s. Only the [`FrameMailbox`](crate::FrameMailbox) and the
//! [`PositionHandle`] are shared with other threads.
//!
//! **Why a slot.** `ASYNC_DONE` carries no seek seqnum, so the only way to know
//! which seek finished is to have one in flight. A request made while one is
//! in flight waits in `pending`, where a newer request replaces it.
//!
//! **Why `Settling`.** A pause (PLAYING → PAUSED) prerolls and posts an
//! `ASYNC_DONE` of its own. Were a seek issued right behind it, that
//! `ASYNC_DONE` would complete the seek before it landed. So a pause that goes
//! async occupies the slot until its `ASYNC_DONE` arrives, and a request made
//! meanwhile waits in `pending` like any other.

mod sink;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_gl as gst_gl;
use gstreamer_gl::prelude::*;

pub use sink::SinkKind;
pub(crate) use sink::{gl_bin, gl_caps};

use crate::mailbox::FrameMailbox;

/// Who asked for a seek. Reported back on completion, displacement and
/// failure, so the bus can tell a skip's outcome from a scrub's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Skip,
    Scrub,
    /// The bus itself: EOS advance, position restore, reloads.
    System,
}

/// What the player reports back to the bus.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// The flight from `origin` landed.
    SeekDone { origin: Origin },
    /// A pending request from `origin` was replaced by a newer one and will
    /// never be issued.
    SeekDisplaced { origin: Origin },
    /// The flight from `origin` could not be issued. The slot is free again.
    SeekFailed { origin: Origin },
    /// A new source prerolled; its seek is being issued next.
    Loaded { diagnostics: Diagnostics },
    /// End of stream while no flight was busy. (An EOS during a flight was
    /// posted before a flushing seek and is stale.)
    Eos,
    /// A pipeline error. The flight and pending request are dropped, and the
    /// next request reloads its source.
    ///
    /// A load whose PAUSED transition fails synchronously reports
    /// `SeekFailed` at once, and the errors GStreamer posted during that
    /// transition follow as `Error`s. If a request was pending, it is issued
    /// in between and those errors drop it too — so, like every `Error`, they
    /// leave the slot idle, never wedged.
    Error(String),
}

/// What the display path is actually doing (spec D12). Logged on every load,
/// and on every export. A field is `None` where it doesn't apply, e.g. the GL
/// fields with a [`SinkKind::System`] sink.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Factory name of the selected video decoder, e.g. `vah265dec`.
    pub decoder: Option<String>,
    /// Caps on `glupload`'s sink pad. `memory:DMABuf` means zero-copy import;
    /// plain `video/x-raw` means a CPU copy. (The uploader can't be queried.)
    pub glupload_caps: Option<String>,
    /// Platform of the GL context `glupload` uses, e.g. `egl`.
    pub gl_platform: Option<String>,
}

/// A clonable, `Send` handle that can only query the playback position
/// (spec D5: the UI's 30 Hz readout). It never changes pipeline state.
#[derive(Debug, Clone)]
pub struct PositionHandle {
    pipeline: gst::Pipeline,
}

impl PositionHandle {
    /// Position within the current source, in seconds.
    pub fn query_position(&self) -> Option<f64> {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map(seconds)
    }
}

type GlSlot = Arc<Mutex<Option<(gst_gl::GLDisplay, gst_gl::GLContext)>>>;

#[derive(Debug, Clone)]
struct Request {
    uri: String,
    secs: f64,
    accurate: bool,
    origin: Origin,
}

#[derive(Debug)]
enum Flight {
    Idle,
    /// READY → `uri` → PAUSED issued; waiting for the load's own `ASYNC_DONE`,
    /// then the request's seek is issued.
    Loading(Request),
    /// Seek issued; the next `ASYNC_DONE` completes it.
    Seeking(Request),
    /// An `ASYNC_DONE` that answers no request is on its way — from a pause
    /// that went async, or from a seek [`SourcePlayer::clear`] dropped. It is
    /// absorbed, and reports nothing.
    Settling,
}

/// `playbin3` flags: video | audio | soft-volume | native-video (spec D1).
/// Soft-volume keeps `volume` on the pipeline rather than the sound server.
const PLAYBIN_FLAGS: &str = "video+audio+soft-volume+native-video";

pub struct SourcePlayer {
    pipeline: gst::Pipeline,
    /// `glupload` inside a [`SinkKind::Gl`] sink, for diagnostics; its
    /// presence also closes the GL gate until `set_gl_context`.
    glupload: Option<gst::Element>,
    gl_slot: GlSlot,
    /// The video sink's mailbox, emptied by [`SourcePlayer::unload`].
    mailbox: FrameMailbox,
    flight: Flight,
    pending: Option<Request>,
    /// The `uri` the pipeline holds, set when its load starts and cleared when
    /// it can no longer be trusted (an error, or a load dropped by `clear`).
    loaded_uri: Option<String>,
    want_playing: bool,
}

impl SourcePlayer {
    /// Creates the `playbin3` with the sinks `kind` names, in NULL. Frames
    /// arrive in `mailbox`, which the bus owns and shares with the preview.
    ///
    /// `on_message` receives every bus message except GL context requests,
    /// which are answered here. It is called on GStreamer's threads.
    pub fn new(
        kind: SinkKind,
        mailbox: FrameMailbox,
        on_message: impl Fn(gst::Message) + Send + Sync + 'static,
    ) -> Self {
        let video = sink::video_sink(kind, mailbox.clone());
        let pipeline = gst::ElementFactory::make("playbin3")
            .property("video-sink", &video.element)
            .property("audio-sink", sink::audio_sink(kind))
            .build()
            .expect("playbin3 is missing (gst-plugins-base)")
            .downcast::<gst::Pipeline>()
            .expect("playbin3 is a pipeline");
        pipeline.set_property_from_str("flags", PLAYBIN_FLAGS);

        let gl_slot = GlSlot::default();
        let bus = pipeline.bus().expect("a pipeline has a bus");
        // Drop everything: nothing piles up on the async bus, which no one
        // pops.
        bus.set_sync_handler({
            let gl_slot = gl_slot.clone();
            move |_, msg| {
                match msg.view() {
                    gst::MessageView::NeedContext(need) => {
                        if let Some((display, context)) = gl_slot.lock().unwrap().as_ref() {
                            answer_need_context(msg, need, display, context);
                        }
                    }
                    _ => on_message(msg.to_owned()),
                }
                gst::BusSyncReply::Drop
            }
        });

        SourcePlayer {
            pipeline,
            glupload: video.glupload,
            gl_slot,
            mailbox,
            flight: Flight::Idle,
            pending: None,
            loaded_uri: None,
            want_playing: false,
        }
    }

    /// Supplies the UI's wrapped GL display and context (spec D3). With a GL
    /// sink the pipeline stays in NULL until this is called; a load requested
    /// before then continues now.
    pub fn set_gl_context(
        &mut self,
        display: gst_gl::GLDisplay,
        context: gst_gl::GLContext,
    ) -> Vec<PlayerEvent> {
        *self.gl_slot.lock().unwrap() = Some((display, context));
        let mut events = Vec::new();
        if matches!(self.flight, Flight::Loading(_))
            && self.pipeline.current_state() == gst::State::Null
        {
            self.preroll(&mut events);
        }
        events
    }

    /// Requests source time `secs` of `uri`. A different `uri` from the loaded
    /// one loads it first. While a flight is busy the request waits, replacing
    /// (and reporting as displaced) any request already waiting.
    pub fn seek_to(
        &mut self,
        uri: &str,
        secs: f64,
        accurate: bool,
        origin: Origin,
    ) -> Vec<PlayerEvent> {
        let request = Request {
            uri: uri.to_owned(),
            secs,
            accurate,
            origin,
        };
        let mut events = Vec::new();
        if !self.is_idle() {
            if let Some(displaced) = self.pending.replace(request) {
                events.push(PlayerEvent::SeekDisplaced {
                    origin: displaced.origin,
                });
            }
        } else {
            self.issue(request, &mut events);
        }
        events
    }

    /// Records whether playback should run. Applied once no flight is busy,
    /// so a load never flashes its first frame by playing early.
    pub fn set_playing(&mut self, playing: bool) {
        self.want_playing = playing;
        if self.is_idle() {
            self.apply_playing();
        }
    }

    /// Drops the flight and the pending request: neither is reported again.
    /// The pipeline keeps its source unless a load was cut short. A dropped
    /// seek's `ASYNC_DONE` is still coming, so the slot settles on it before
    /// issuing the next request.
    pub fn clear(&mut self) {
        self.flight = match std::mem::replace(&mut self.flight, Flight::Idle) {
            Flight::Loading(_) => {
                // Reloaded by the next request; a stale ASYNC_DONE is
                // filtered by the `Loading` arm.
                self.loaded_uri = None;
                Flight::Idle
            }
            Flight::Seeking(_) | Flight::Settling => Flight::Settling,
            Flight::Idle => Flight::Idle,
        };
        self.pending = None;
    }

    /// Drops the loaded source: the pipeline goes to READY and the flight,
    /// the pending request and the mailbox's frame are dropped, so nothing of
    /// the old source remains to be drawn. Nothing plays until the next
    /// request loads a source. Idempotent.
    pub fn unload(&mut self) {
        // A gated GL pipeline never left NULL, and must not leave it now.
        if !self.gl_gated() {
            // Downward, so synchronous: the streaming threads have stopped
            // and can't refill the mailbox once this returns.
            let _ = self.pipeline.set_state(gst::State::Ready);
        }
        self.reset();
        self.mailbox.take();
    }

    /// Sets the volume from a linear slider value in `0..=1`, mapped as `x³`
    /// (mpv's perceptual curve). Survives source changes.
    pub fn set_volume(&self, linear: f64) {
        let x = if linear.is_finite() {
            linear.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.pipeline.set_property("volume", x.powi(3));
    }

    /// The source time the slot is heading for: the pending request's, else
    /// the in-flight one's. `None` once it has landed (or with nothing
    /// requested).
    pub fn target_secs(&self) -> Option<f64> {
        match (&self.pending, &self.flight) {
            (Some(request), _) => Some(request.secs),
            (None, Flight::Loading(request) | Flight::Seeking(request)) => Some(request.secs),
            (None, Flight::Idle | Flight::Settling) => None,
        }
    }

    /// Whether `uri` is the source the player holds or is heading to: the
    /// pending request's, else the loaded (or loading) one. False once the
    /// source was dropped — by an error, `unload`, or a load `clear` cut
    /// short.
    pub fn holds(&self, uri: &str) -> bool {
        let heading = match &self.pending {
            Some(request) => Some(request.uri.as_str()),
            None => self.loaded_uri.as_deref(),
        };
        heading == Some(uri)
    }

    /// The `uri` the pipeline holds, for logging.
    pub fn loaded_uri(&self) -> Option<&str> {
        self.loaded_uri.as_deref()
    }

    /// No flight is busy, so no request is pending either: a request now is
    /// issued at once.
    pub fn is_idle(&self) -> bool {
        matches!(self.flight, Flight::Idle)
    }

    pub fn position_handle(&self) -> PositionHandle {
        PositionHandle {
            pipeline: self.pipeline.clone(),
        }
    }

    /// Advances the slot on one forwarded bus message.
    pub fn handle(&mut self, msg: &gst::Message) -> Vec<PlayerEvent> {
        let mut events = Vec::new();
        match msg.view() {
            gst::MessageView::AsyncDone(_) => {
                match std::mem::replace(&mut self.flight, Flight::Idle) {
                    Flight::Idle => {}
                    Flight::Loading(request) => {
                        // A stale ASYNC_DONE (from before the READY) can arrive
                        // while the load is still prerolling; only a finished
                        // preroll is this load's.
                        if self.pipeline.state(gst::ClockTime::ZERO)
                            == (
                                Ok(gst::StateChangeSuccess::Success),
                                gst::State::Paused,
                                gst::State::VoidPending,
                            )
                        {
                            events.push(PlayerEvent::Loaded {
                                diagnostics: diagnostics(&self.pipeline, self.glupload.as_ref()),
                            });
                            self.seek(request, &mut events);
                        } else {
                            self.flight = Flight::Loading(request);
                        }
                    }
                    Flight::Seeking(request) => {
                        events.push(PlayerEvent::SeekDone {
                            origin: request.origin,
                        });
                        self.advance(&mut events);
                    }
                    Flight::Settling => self.advance(&mut events),
                }
            }
            gst::MessageView::Eos(_) if self.is_idle() => events.push(PlayerEvent::Eos),
            gst::MessageView::Error(err) => {
                // Unlike `clear`, nothing is left to settle on: the error
                // ends whatever transition was under way.
                self.reset();
                events.push(PlayerEvent::Error(format!(
                    "{} (from {}; {})",
                    err.error(),
                    msg.src()
                        .map(|s| s.path_string().to_string())
                        .unwrap_or_default(),
                    err.debug().map(|d| d.to_string()).unwrap_or_default()
                )));
            }
            _ => {}
        }
        events
    }

    /// Forgets the flight, the pending request and the loaded source.
    fn reset(&mut self) {
        self.flight = Flight::Idle;
        self.pending = None;
        self.loaded_uri = None;
    }

    /// With a GL sink, nothing may preroll before the UI's context arrives:
    /// GStreamer would create its own (GLX on X11, which copies every frame).
    fn gl_gated(&self) -> bool {
        self.glupload.is_some() && self.gl_slot.lock().unwrap().is_none()
    }

    fn issue(&mut self, request: Request, events: &mut Vec<PlayerEvent>) {
        if self.loaded_uri.as_deref() == Some(request.uri.as_str()) {
            self.seek(request, events);
            return;
        }
        // Load sequence (spec D4): READY, set `uri`, PAUSED, wait for the
        // load's ASYNC_DONE, then seek. Setting `uri` outside READY/NULL only
        // queues the next file, and a seek before preroll is dropped.
        if !self.gl_gated() {
            let _ = self.pipeline.set_state(gst::State::Ready);
        }
        self.pipeline.set_property("uri", &request.uri);
        self.loaded_uri = Some(request.uri.clone());
        self.flight = Flight::Loading(request);
        if !self.gl_gated() {
            self.preroll(events);
        }
    }

    /// Starts the preroll of a `Loading` flight.
    fn preroll(&mut self, events: &mut Vec<PlayerEvent>) {
        if self.pipeline.set_state(gst::State::Paused).is_err() {
            if let Flight::Loading(request) = std::mem::replace(&mut self.flight, Flight::Idle) {
                self.loaded_uri = None;
                events.push(PlayerEvent::SeekFailed {
                    origin: request.origin,
                });
                self.advance(events);
            }
        }
    }

    fn seek(&mut self, request: Request, events: &mut Vec<PlayerEvent>) {
        let flags = gst::SeekFlags::FLUSH
            | if request.accurate {
                gst::SeekFlags::ACCURATE
            } else {
                gst::SeekFlags::KEY_UNIT
            };
        let position = seconds_to_clock(request.secs);
        let origin = request.origin;
        match self.pipeline.seek_simple(flags, position) {
            Ok(()) => self.flight = Flight::Seeking(request),
            Err(_) => {
                events.push(PlayerEvent::SeekFailed { origin });
                self.advance(events);
            }
        }
    }

    /// Frees the slot, then issues the pending request, or applies
    /// `want_playing` if there is none.
    fn advance(&mut self, events: &mut Vec<PlayerEvent>) {
        self.flight = Flight::Idle;
        match self.pending.take() {
            Some(next) => self.issue(next, events),
            None => self.apply_playing(),
        }
    }

    /// Called with the slot idle. Nothing is played (or paused) with no
    /// source loaded: the pipeline stays where `unload` or a failed load left
    /// it.
    ///
    /// The target is always set, never skipped as already reached: the
    /// pipeline's current and pending states don't say where it is going. A
    /// flushing seek in PLAYING leaves it PAUSED, pending PAUSED, until it
    /// prerolls and returns to PLAYING by itself; a pause skipped then would
    /// be lost. A redundant change is harmless: PAUSED on a settled PAUSED
    /// pipeline returns success.
    ///
    /// A pause that goes async posts an `ASYNC_DONE`, so the slot settles on
    /// it (see the module docs). Only a pause: going to PLAYING can return
    /// async too, but posts no `ASYNC_DONE`, and waiting for one would wedge
    /// the slot.
    fn apply_playing(&mut self) {
        if self.loaded_uri.is_none() || self.gl_gated() {
            return;
        }
        let target = if self.want_playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        let change = self.pipeline.set_state(target);
        if target == gst::State::Paused && change == Ok(gst::StateChangeSuccess::Async) {
            self.flight = Flight::Settling;
        }
    }
}

/// Dropping the player takes the pipeline to NULL, which is how the bus shuts
/// it down (and must happen before the UI's GL context goes away).
impl Drop for SourcePlayer {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// What `pipeline`'s video path is doing: its video decoder, and with a
/// `glupload` inside, the caps it was fed and its GL platform.
pub(crate) fn diagnostics(
    pipeline: &gst::Pipeline,
    glupload: Option<&gst::Element>,
) -> Diagnostics {
    let decoder = pipeline
        .iterate_recurse()
        .into_iter()
        .flatten()
        .filter_map(|e| e.factory())
        .find(|f| {
            let klass = f.klass();
            klass.contains("Decoder") && klass.contains("Video")
        })
        .map(|f| f.name().to_string());
    let glupload_caps = glupload
        .and_then(|u| u.static_pad("sink"))
        .and_then(|p| p.current_caps())
        .map(|c| c.to_string());
    let gl_platform = glupload
        .and_then(|u| u.property::<Option<gst_gl::GLContext>>("context"))
        .map(|c| c.gl_platform().to_string());
    Diagnostics {
        decoder,
        glupload_caps,
        gl_platform,
    }
}

/// Answers a GL element's context request with `display` and `context`, so
/// every GL element shares them. Other context types (e.g. a VA display) are
/// left to the element.
pub(crate) fn answer_need_context(
    msg: &gst::Message,
    need: &gst::message::NeedContext,
    display: &gst_gl::GLDisplay,
    context: &gst_gl::GLContext,
) {
    let Some(element) = msg.src().and_then(|s| s.downcast_ref::<gst::Element>()) else {
        return;
    };
    let ctx_type = need.context_type();
    if ctx_type == *gst_gl::GL_DISPLAY_CONTEXT_TYPE {
        let ctx = gst::Context::new(ctx_type, true);
        ctx.set_gl_display(display);
        element.set_context(&ctx);
    } else if ctx_type == "gst.gl.app_context" {
        let mut ctx = gst::Context::new(ctx_type, true);
        ctx.get_mut()
            .expect("a new context is writable")
            .structure_mut()
            .set("context", context);
        element.set_context(&ctx);
    }
}

fn seconds(t: gst::ClockTime) -> f64 {
    t.nseconds() as f64 / 1e9
}

/// Seconds to a `ClockTime`, **rounded** to the nanosecond. Truncating lands
/// 1 ns short of a frame boundary for ~2% of frame times (e.g. `k / 30.0`), and
/// an accurate seek there shows the previous frame (export-graph spike).
/// `f64::max` maps NaN to 0; an infinite target saturates and lands at the end.
pub(crate) fn seconds_to_clock(secs: f64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds((secs.max(0.0) * 1e9).round() as u64)
}
