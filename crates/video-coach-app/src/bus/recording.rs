//! Recording (Phase 4 spec R6): the one Active state, from Start to a clip.
//!
//! Active is flagged *starting* until the recorder's first video buffer
//! reaches the muxer. Stopping before then (StopRecording, a recorder error,
//! or the start timeout) aborts: no clip, and the file is deleted. Stopping
//! after it always keeps the clip, even if finalizing didn't go cleanly.
//!
//! The recorder's messages arrive as their own input, tagged here with the
//! generation of the recording that produced them, and never reach the
//! player: its EOS would advance the source and its ERROR reset the player.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gstreamer::glib;
use uuid::Uuid;
use video_coach_core::project::Preferences;
use video_coach_core::recording::{PendingClip, RecordingLog};
use video_coach_core::stroke::Stroke;
use video_coach_core::zoom::Zoom;
use video_coach_media::{
    list_devices, resolve_camera, resolve_mic, CaptureSources, Recorder, RecorderMessage,
};

use super::{Bus, Event, Input, UserError};

/// How long a recording may wait for its first video frame.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a stop waits for the file to finalize.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Where recordings come from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureKind {
    /// The project's preferred camera and microphone, or the defaults.
    Devices,
    /// Live test sources, with video starting `video_delay` in, as a camera
    /// warming up. For tests: no camera, microphone or display.
    Test { video_delay: Duration },
}

/// What the recording is doing, as the UI shows it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordingStatus {
    Idle,
    /// Recording, but no video has arrived yet: stopping now aborts.
    Starting,
    /// `t0_ns` is the recording's time 0 on `now_ns()`'s clock, for the
    /// elapsed-time readout.
    Recording {
        t0_ns: u64,
    },
}

/// The recording in progress.
pub(super) struct Active {
    pub(super) pending: PendingClip,
    recorder: Recorder,
    pub(super) log: RecordingLog,
    /// The file, deleted on an abort.
    path: PathBuf,
    /// When it started, for [`START_TIMEOUT`].
    started: Instant,
    /// The first video buffer reached the muxer: stopping keeps the clip.
    video_seen: bool,
}

impl Bus {
    /// R: starts a recording while idle, else stops (or aborts) it. The UI's
    /// status can lag the bus's, so the bus decides: a second R during
    /// start-up cancels, as the user means.
    pub(super) fn toggle_recording(&mut self, zoom: Zoom) {
        if self.recording.is_some() {
            self.stop_recording();
        } else {
            self.start_recording(zoom);
        }
    }

    /// Starts recording from where the player is heading (R6). Refused with
    /// an error unless a project with sources, none missing, is open and its
    /// current source is loaded or loading.
    fn start_recording(&mut self, zoom: Zoom) {
        if let Err(e) = self.can_record() {
            return self.emit(Event::Error(e));
        }
        let Some(open) = &self.open else {
            return;
        };
        let folder = open.folder.join("recordings");
        // Before anything changes: a refusal leaves the player as it was.
        let sources = match self.capture_sources(&open.project.preferences) {
            Ok(sources) => sources,
            Err(e) => return self.emit(Event::Error(e)),
        };

        // Every clip starts on a still frame.
        if self.playing {
            self.set_playing(false);
        }
        let (source_index, start_source_seconds) = self.heading(None);
        let pending = PendingClip {
            id: Uuid::new_v4(),
            source_index,
            start_source_seconds,
        };
        if let Err(e) = std::fs::create_dir_all(&folder) {
            return self.emit(Event::Error(UserError::Io(format!(
                "{}: {e}",
                folder.display()
            ))));
        }
        let path = folder.join(format!("{}.mkv", pending.id));

        self.generation += 1;
        let (tx, generation) = (self.tx.clone(), self.generation);
        // The last take's final frame must not open this one.
        let _ = self.self_view.take();
        let started = Recorder::start(sources, &path, self.self_view.clone(), move |msg| {
            // Fails only once the bus thread has exited.
            let _ = tx.send(Input::Recorder(generation, msg));
        });
        let recorder = match started {
            Ok(recorder) => recorder,
            Err(e) => {
                // filesink has already created it.
                remove_recording(&path);
                return self.emit(Event::Error(UserError::RecordingFailed(e)));
            }
        };
        self.recording = Some(Active {
            pending,
            log: RecordingLog::new(recorder.t0_ns(), zoom, start_source_seconds),
            recorder,
            path,
            started: Instant::now(),
            video_seen: false,
        });
        self.emit(Event::Recording(RecordingStatus::Starting));
        // Recording always wins (Phase 10 spec S5): the transcript running
        // gives way and goes back to the front of the queue. **Here**, once
        // the recording exists — not at the top of `toggle_recording`, which
        // bails at five points above, where a refused record would have
        // killed a transcript for nothing. After the status, since this joins
        // the transcription thread and the UI is waiting to say Recording.
        self.preempt_transcription();
    }

    /// Stops the recording, keeping its clip, or aborts it if no video has
    /// arrived. Nothing to do while idle.
    pub(super) fn stop_recording(&mut self) {
        match &self.recording {
            None => {}
            Some(active) if active.video_seen => self.finish_recording(),
            Some(_) => self.abort_recording(),
        }
    }

    pub(super) fn recorder_message(&mut self, generation: u64, msg: RecorderMessage) {
        if generation != self.generation {
            return;
        }
        let Some(active) = &mut self.recording else {
            return;
        };
        match msg {
            RecorderMessage::FirstVideo => {
                active.video_seen = true;
                let t0_ns = active.recorder.t0_ns();
                self.emit(Event::Recording(RecordingStatus::Recording { t0_ns }));
            }
            RecorderMessage::Level { peak_db } => self.emit(Event::Level(peak_db)),
            RecorderMessage::Error(e) => {
                eprintln!("bus: recorder error: {e}");
                self.emit(Event::Error(UserError::RecordingFailed(e)));
                self.stop_recording();
            }
        }
    }

    /// When a recording that has had no video gives up, if one is starting.
    pub(super) fn start_deadline(&self) -> Option<Instant> {
        self.recording
            .as_ref()
            .filter(|active| !active.video_seen)
            .map(|active| active.started + START_TIMEOUT)
    }

    /// No video by [`Bus::start_deadline`].
    pub(super) fn start_timed_out(&mut self) {
        self.emit(Event::Error(UserError::RecordingFailed(format!(
            "no video from the camera within {} seconds",
            START_TIMEOUT.as_secs()
        ))));
        self.abort_recording();
    }

    /// Logs the play state just entered at `host_ns`, while recording.
    pub(super) fn log_playing(&mut self, host_ns: u64, ui_secs: Option<f64>) {
        let (_, anchor) = self.heading(ui_secs);
        let playing = self.playing;
        if let Some(active) = &mut self.recording {
            if playing {
                active.log.play(host_ns, anchor);
            } else {
                active.log.pause(host_ns, anchor);
            }
        }
    }

    /// Logs a zoom change while recording. The UI sends every change, so one
    /// made before its status caught up isn't lost; while idle it's ignored.
    pub(super) fn log_zoom(&mut self, host_ns: u64, zoom: Zoom) {
        if let Some(active) = &mut self.recording {
            active.log.zoom(host_ns, zoom);
        }
    }

    /// Logs a finished drawing while recording, ignored while idle (Phase 6
    /// spec D4). `host_ns` is its pen-up, the moment of its last point. The UI
    /// is what confines drawing to the Recording phase.
    pub(super) fn log_stroke(&mut self, host_ns: u64, stroke: Stroke) {
        if let Some(active) = &mut self.recording {
            active.log.stroke(host_ns, stroke);
        }
    }

    /// Logs a Clear while recording, ignored while idle.
    pub(super) fn log_clear_all(&mut self, host_ns: u64) {
        if let Some(active) = &mut self.recording {
            active.log.clear_all(host_ns);
        }
    }

    pub(super) fn set_camera(&mut self, camera: Option<String>) {
        if let Some(open) = &mut self.open {
            open.project.preferences.preferred_camera_id = camera;
            self.project_changed();
        }
    }

    pub(super) fn set_mic(&mut self, mic: Option<String>) {
        if let Some(open) = &mut self.open {
            open.project.preferences.preferred_mic_id = mic;
            self.project_changed();
        }
    }

    /// Start's preconditions (R6), and no export or preview running. A skip or
    /// scrub in flight is not a reason to refuse: the recording starts where
    /// the player is heading.
    fn can_record(&self) -> Result<(), UserError> {
        let refused = |why| Err(UserError::CantRecord(why));
        match &self.open {
            None => refused("no project is open"),
            // They never overlap: sharing the video engine costs the
            // recording frames (Phase 5 spec X4).
            Some(_) if self.export.is_some() => refused("an export is running"),
            // The preview holds the picture and the audio sink, and a
            // recording is made over the game video (spec P5).
            Some(_) if self.preview.is_some() => refused("a preview is open; close it first"),
            Some(open) if open.project.source_videos.is_empty() => {
                refused("add a game video to record over first")
            }
            Some(_) if self.any_missing() => refused("a game video is missing; relink it first"),
            Some(_) if !self.loaded() => refused("the game video isn't loaded"),
            Some(_) => Ok(()),
        }
    }

    /// The recorder's sources. For devices: the preferred camera and mic if
    /// connected, else the defaults with a notice, keeping the preference
    /// (R2).
    fn capture_sources(&self, preferences: &Preferences) -> Result<CaptureSources, UserError> {
        if let CaptureKind::Test { video_delay } = self.capture {
            return Ok(CaptureSources::Test { video_delay });
        }
        let devices = list_devices();
        let (camera, camera_fell_back) =
            resolve_camera(&devices.cameras, preferences.preferred_camera_id.as_deref())
                .ok_or(UserError::NoCamera)?;
        let (mic, mic_fell_back) =
            resolve_mic(&devices.mics, preferences.preferred_mic_id.as_deref());
        if camera_fell_back {
            self.emit(Event::Error(UserError::DeviceFallback { what: "camera" }));
        }
        if mic_fell_back {
            self.emit(Event::Error(UserError::DeviceFallback {
                what: "microphone",
            }));
        }
        Ok(CaptureSources::Devices {
            camera: camera.clone(),
            mic: mic.map(str::to_owned),
        })
    }

    /// Stops the recorder and adds the clip (R6's stop): the log is closed
    /// first, so no event outlasts the file.
    fn finish_recording(&mut self) {
        let Some(active) = self.recording.take() else {
            return;
        };
        let events = active.log.finish();
        let clip_id = active.pending.id;
        let outcome = active.recorder.stop(STOP_TIMEOUT);
        if let Some(open) = &mut self.open {
            open.project
                .add_recorded_clip(active.pending, outcome.duration, events, created_at());
        }
        self.project_changed();
        self.emit(Event::Recording(RecordingStatus::Idle));
        if !outcome.clean {
            self.emit(Event::Error(UserError::StopNotClean));
        }
        // The clip exists and is saved, so it can be transcribed (Phase 10
        // spec S6). Whatever this recording preempted resumes on its own:
        // `Bus::run`'s tail starts the queue once nothing is in its way.
        self.transcribe_after_recording(clip_id);
    }

    /// Drops a recording that never got video: no clip, no file.
    fn abort_recording(&mut self) {
        let Some(active) = self.recording.take() else {
            return;
        };
        // NULL first, so nothing still writes the file.
        drop(active.recorder);
        remove_recording(&active.path);
        self.emit(Event::Recording(RecordingStatus::Idle));
    }
}

/// Deletes a recording's file. One that was never created is already gone.
fn remove_recording(path: &std::path::Path) {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            eprintln!("bus: could not delete {}: {e}", path.display());
        }
        _ => {}
    }
}

/// Now as RFC3339 in whole seconds, matching the fixtures and macOS. Empty
/// if the system time can't be read: nothing reads it.
fn created_at() -> String {
    glib::DateTime::now_utc()
        .and_then(|now| now.format("%Y-%m-%dT%H:%M:%SZ"))
        .map(Into::into)
        .unwrap_or_default()
}
