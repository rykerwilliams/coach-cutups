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
}

impl Bus {
    /// Starts exporting clip `id` to `path`, or says why it can't.
    pub(super) fn export_clip(&mut self, id: Uuid, path: PathBuf) {
        if let Err(e) = self.start_export(id, path) {
            self.emit(Event::Error(e));
        }
    }

    fn start_export(&mut self, id: Uuid, path: PathBuf) -> Result<(), UserError> {
        let refused = |why| Err(UserError::CantExport(why));
        if self.export.is_some() {
            return refused("an export is running");
        }
        let Some(open) = &self.open else {
            return refused("no project is open");
        };
        let Some(clip) = open.project.clips.iter().find(|c| c.id == id) else {
            return refused("the clip is gone");
        };
        let Some(source) = open.project.source_videos.get(clip.source_index) else {
            return refused("the clip's game video is gone");
        };
        if self.missing.get(clip.source_index).copied().unwrap_or(true) {
            return refused("the clip's game video is missing; relink it first");
        }
        // A snapshot: later edits to the project don't reach this export.
        let job = ExportJob {
            source: open.folder.join(&source.relative_path),
            frames: frame_schedule(clip, source.duration_seconds),
            path,
        };
        let tx = self.tx.clone();
        let exporter = Exporter::start(job, move |msg| {
            // Fails only once the bus thread has exited.
            let _ = tx.send(Input::Export(msg));
        })
        .map_err(UserError::ExportFailed)?;
        self.export = Some(exporter);
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
                        self.emit(Event::Error(UserError::ExportFailed(e)));
                    }
                }
            }
        }
    }
}
