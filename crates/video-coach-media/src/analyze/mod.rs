//! The match-vision analysis passes (spec D1): one source video read for what
//! it sounds like and what moves in it, so core's pure rules have a number
//! series to work on.
//!
//! One job, **two passes, no seeking**. The [`audio`] pass streams the file
//! at 16 kHz mono through the export's own [`Reader`](crate::composite::audio)
//! and hands core the samples; the motion pass reads it again for a 5 fps
//! thumbnail difference. Two passes rather than one pipeline with two sinks:
//! a sink nobody drains stalls the graph, and the audio pass costs seconds
//! against the motion pass's minutes.
//!
//! **This stores nothing and suggests nothing.** P3 is the measurement phase:
//! the passes exist so the `#[ignore]`d ground-truth run can grade core's
//! rules against the coach's own tags. The commands, the suggestions and the
//! project-format change are P4's.

pub mod audio;

/// Why an analysis pass produced nothing. The composite's error under
/// analysis's name, as [`TranscribeError`](crate::TranscribeError) is under
/// transcription's.
pub type AnalyzeError = crate::composite::CompositeError;
