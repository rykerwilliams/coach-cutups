//! Export (Phase 5 spec X4): one clip at a time, rendered by an [`Exporter`]
//! on its own thread while the bus goes on.
//!
//! The exporter's messages arrive as their own input. It sends exactly one
//! `Finished`, last, and there is one exporter at a time on a FIFO channel,
//! so no message can be stale: the thread's own result decides the outcome,
//! and a cancel that loses the race to a finished file reports `Done`.
//!
//! Recording and export never overlap (a user decision): the recording guard
//! drops `ExportClip`, and `can_record` refuses to record while one runs.

use std::path::PathBuf;

use uuid::Uuid;
use video_coach_core::export::frame_schedule;
use video_coach_media::{ExportError, ExportJob, ExportMessage, Exporter};

use super::{Bus, Event, Input, UserError};

/// What the export is doing, as the UI shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportStatus {
    /// Whole percent of the frames rendered; 0 as it starts.
    Running(u8),
    /// The file is complete at this path.
    Done(PathBuf),
    /// Cancelled: no file, and any file already at the path untouched.
    Cancelled,
    /// Failed, with no file and any file already at the path untouched.
    /// A refusal to start is a [`UserError::CantExport`] instead.
    Failed(String),
}

impl Bus {
    /// Starts exporting clip `id` to `path`, or says why it can't.
    pub(super) fn export_clip(&mut self, id: Uuid, path: PathBuf) {
        if let Err(e) = self.start_export(id, path) {
            self.emit(Event::Error(e));
        }
    }

    fn start_export(&mut self, id: Uuid, path: PathBuf) -> Result<(), UserError> {
        let refused = |why: &str| Err(UserError::CantExport(why.into()));
        if self.export.is_some() {
            return refused("an export is running");
        }
        let Some(open) = &self.open else {
            return refused("no project is open");
        };
        let Some(clip) = open.project.clips.iter().find(|c| c.id == id) else {
            return refused("the clip is gone");
        };
        let Some(video) = open.project.source_videos.get(clip.source_index) else {
            return refused("the clip's game video is gone");
        };
        if self.missing.get(clip.source_index).copied().unwrap_or(true) {
            return refused("the clip's game video is missing; relink it first");
        }
        let source = open.folder.join(&video.relative_path);
        // The finished file would replace the game video.
        if path
            .canonicalize()
            .is_ok_and(|p| Some(p) == source.canonicalize().ok())
        {
            return refused("that file is the clip's game video");
        }
        let frames = frame_schedule(clip, video.duration_seconds);
        if frames.is_empty() {
            return refused("the clip has nothing to export");
        }
        // A snapshot: later edits to the project don't reach this export.
        let job = ExportJob {
            source,
            frames,
            path,
        };
        let tx = self.tx.clone();
        self.export = Some(Exporter::start(job, move |msg| {
            // Fails only once the bus thread has exited.
            let _ = tx.send(Input::Export(msg));
        }));
        self.emit(Event::Export(ExportStatus::Running(0)));
        Ok(())
    }

    /// Asks the running export, if any, to stop. Its `Finished` reports how
    /// it ended.
    pub(super) fn cancel_export(&self) {
        if let Some(exporter) = &self.export {
            exporter.cancel();
        }
    }

    pub(super) fn export_message(&mut self, msg: ExportMessage) {
        match msg {
            ExportMessage::Progress(percent) => {
                self.emit(Event::Export(ExportStatus::Running(percent)));
            }
            ExportMessage::Finished(result) => {
                // Joins the thread, which has nothing left to do.
                self.export = None;
                match result {
                    Ok(done) => {
                        let d = &done.diagnostics;
                        eprintln!(
                            "bus: exported {}: decoder {:?}, glupload caps {:?}, encoder {}",
                            done.path.display(),
                            d.decoder,
                            d.glupload_caps,
                            done.encoder
                        );
                        self.emit(Event::Export(ExportStatus::Done(done.path)));
                    }
                    Err(ExportError::Cancelled) => {
                        self.emit(Event::Export(ExportStatus::Cancelled));
                    }
                    Err(ExportError::Failed(e)) => {
                        eprintln!("bus: export failed: {e}");
                        self.emit(Event::Export(ExportStatus::Failed(e)));
                    }
                }
            }
        }
    }
}
