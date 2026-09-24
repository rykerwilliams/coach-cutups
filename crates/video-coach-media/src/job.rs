//! One shape for every worker-thread job in media (spec B1): a thread that
//! sends **exactly one** terminal message, whatever happens on it.
//!
//! A job's last message is the only thing that frees the bus's running slot,
//! and transcription, analysis and tracking share one slot (spec B2). So a
//! job whose thread ended without saying so would stop all three of them for
//! the rest of the session, with `Command::CancelTranscription` the one way
//! out — which is BACKLOG #64, and this module is its fix: the body's own
//! terminal message, or [`JobMessage::panicked`] carrying the panic's text.
//!
//! **Not a job framework.** There is no queue, no scheduler and no handle
//! here: the queues live on the bus, and a job is cancelled by dropping it
//! without ever joining its thread (see [`Transcriber`](crate::Transcriber)).
//! The one thing this owns is the promise that the terminal message is sent.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// What a job says when it can no longer say anything for itself.
///
/// Implemented by each job's own message enum, so a panic wears that job's
/// terminal variant and its own wording rather than a shape the bus would
/// have to learn twice.
pub trait JobMessage: Send + 'static {
    /// The terminal message for a job whose thread unwound, carrying what the
    /// panic said.
    fn panicked(message: String) -> Self;
}

/// Runs `body` on a thread called `name`, reporting through `send`.
///
/// `body` is handed `send` for its progress messages and **returns the job's
/// terminal message**, which is delivered once it returns — so the one place
/// a job could forget to finish is the one place it cannot reach. A panic on
/// the way there becomes [`JobMessage::panicked`] instead, and either way the
/// sender is dropped straight afterwards, which closes the channel.
///
/// Nothing comes back: the thread is never joined (spec B1). It holds only
/// its own work, its last message is tagged with a generation the bus has
/// moved past, and the exiting process reclaims it.
pub fn spawn<M: JobMessage>(
    name: &str,
    mut send: impl FnMut(M) + Send + 'static,
    body: impl FnOnce(&mut dyn FnMut(M)) -> M + Send + 'static,
) {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // `AssertUnwindSafe` over `send`: it is the job's one channel, and
            // the terminal message is the only thing put on it after an
            // unwind. A panic cannot leave it half way through anything that
            // send would then read.
            let terminal = match catch_unwind(AssertUnwindSafe(|| body(&mut send))) {
                Ok(message) => message,
                // The default hook has already printed the panic and its
                // location; this is the half of it the coach sees.
                Err(payload) => M::panicked(panic_text(&*payload)),
            };
            send(terminal);
        })
        .unwrap_or_else(|e| panic!("could not spawn the {name} thread: {e}"));
}

/// What the panic said, out of the payload [`catch_unwind`] hands back.
///
/// `panic!` and every `unwrap`, `expect` and bounds check leave a `&str` or a
/// `String` there. The fallback is for `panic_any`, which nothing here calls.
fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(said) = payload.downcast_ref::<&str>() {
        return (*said).to_owned();
    }
    if let Some(said) = payload.downcast_ref::<String>() {
        return said.clone();
    }
    "the job panicked".to_owned()
}
