//! Spec B1: a job thread sends **exactly one** terminal message, on every
//! path — including the one where its body panics (BACKLOG #64).
//!
//! The bus frees its running slot on that message and on nothing else, and
//! one slot is shared by transcription, analysis and tracking (spec B2), so a
//! job that ended without saying so would stop all three for the session.
//!
//! These tests panic on purpose, so the default hook's `thread 'a-job'
//! panicked at …` line on stderr is part of a passing run — it is how the
//! panic still reaches a developer once the coach has been told in prose.

use std::sync::mpsc;

use video_coach_media::job;
use video_coach_media::{TranscribeError, TranscribeMessage};

/// Everything a job sent, in order. `rx.iter()` ends when the job thread
/// drops its sender, so this needs no timeout and no sleep — and its end is
/// itself the assertion that the thread is over.
fn collect(
    body: impl FnOnce(&mut dyn FnMut(TranscribeMessage)) -> TranscribeMessage + Send + 'static,
) -> Vec<TranscribeMessage> {
    let (tx, rx) = mpsc::channel();
    job::spawn(
        "a-job",
        move |msg| {
            let _ = tx.send(msg);
        },
        body,
    );
    rx.iter().collect()
}

/// A body that unwinds finishes `Failed`, carrying what the panic said, and
/// the messages it managed to send first still stand.
#[test]
fn a_panicking_job_finishes_failed_with_the_panic_text() {
    let messages = collect(|send| {
        send(TranscribeMessage::Progress(10));
        panic!("the seventh slice of three");
    });

    let [progress, terminal] = &messages[..] else {
        panic!("expected a progress and one failure, got {messages:?}");
    };
    assert_eq!(*progress, TranscribeMessage::Progress(10));
    let TranscribeMessage::Finished(Err(TranscribeError::Failed(said))) = terminal else {
        panic!("expected a failure, got {terminal:?}");
    };
    assert!(
        said.contains("the seventh slice of three"),
        "the panic's own words are missing: {said}"
    );
}

/// And a body that returns sends its own terminal message, once: the helper
/// adds nothing to a job that finished for itself.
#[test]
fn a_job_that_returns_sends_its_terminal_message_and_no_other() {
    let messages = collect(|_| TranscribeMessage::Finished(Ok("nice ball".into())));
    assert_eq!(
        messages,
        [TranscribeMessage::Finished(Ok("nice ball".into()))]
    );
}
