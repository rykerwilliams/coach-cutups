//! A speech model, fetched the first time it is needed (Phase 11 spec S3).
//!
//! ```text
//! souphttpsrc location=<url> iradio-mode=false ! filesink location=<dest>.part
//! ```
//!
//! **GStreamer is the HTTP client and glib is the hash**, so this adds no
//! crate: `souphttpsrc` is `plugins-good`, whose `libsoup` brings TLS through
//! `glib-networking`, and `glib::Checksum` is already linked through
//! `gst::glib`. It follows redirects, which Hugging Face's CDN needs.
//!
//! **`.part`, verified, then renamed**, as export does: nothing at the final
//! path is ever less than the whole file, so "is the model there" stays one
//! `is_file`.
//!
//! **Network facts, measured for the spec and not handled here:**
//! `souphttpsrc` gives up on a read after 15 s and retries 3 times, and the
//! CDN's signed URL expires about an hour after issue — which only matters
//! below roughly 135 kB/s for `small.en`.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;

use crate::composite::{CompositeError, Stopper};

/// How often the download looks at the cancel flag and its own progress.
const POLL: gst::ClockTime = gst::ClockTime::from_mseconds(100);

/// How much of the file is hashed at a time: small enough that a cancel is
/// noticed within a few milliseconds, big enough that the loop costs nothing
/// beside the hash.
const HASH_CHUNK: usize = 1 << 20;

/// Where a file comes from and how to know it arrived whole.
///
/// **Its presence is the permission.** A [`TranscribeKind::Whisper`](crate::TranscribeKind::Whisper)
/// with no `Fetch` never downloads, whatever its path is named: the app sets
/// one only for a model under its own cache directory, never under
/// `$COACH_CUTS_WHISPER_MODEL`, and the tests that point the transcriber at a
/// missing file carrying our own name would otherwise pull 488 MB from
/// Hugging Face on CI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetch {
    pub url: String,
    /// Lower-case hex, as [`glib::Checksum::string`] writes it.
    pub sha256: String,
    /// The whole file's length: the progress denominator, and the first
    /// check on what arrived.
    pub bytes: u64,
}

/// Downloads `fetch` to `dest`, reporting whole percents to `progress` from
/// this thread, and returns once `dest` is the verified file.
///
/// `progress(0)` comes first, before anything can fail, so a caller showing
/// a download has something to show even for one that fails at once.
///
/// **A cancel touches no file.** It leaves the `.part` where it is: the
/// transcriber is never joined, so the next job may already have opened the
/// same `.part` by the time this one notices, and a delete then would send
/// its whole download into an unlinked inode. The next attempt truncates it
/// anyway, as does quitting mid-download.
///
/// **A file that fails its check is deleted**, and the failure names where
/// it was going. No retry here: pressing Transcribe again is the retry.
pub fn download(
    fetch: &Fetch,
    dest: &Path,
    progress: &mut dyn FnMut(u8),
    cancel: &AtomicBool,
) -> Result<(), CompositeError> {
    progress(0);
    // `filesink` opens its file and nothing else, so a first run on a
    // machine with no cache directory yet would fail here without this.
    if let Some(dir) = dest.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| {
            CompositeError::Failed(format!("could not create {}: {e}", dir.display()))
        })?;
    }
    let part = part_path(dest);
    fetch_to(fetch, &part, progress, cancel)?;
    let verdict = check(fetch, &part, cancel)?;
    // Past here nothing reads the flag, so this is where a cancel that came
    // in during the hash stops it from touching the files.
    if cancel.load(Ordering::SeqCst) {
        return Err(CompositeError::Cancelled);
    }
    if let Err(why) = verdict {
        let _ = std::fs::remove_file(&part);
        return Err(CompositeError::Failed(format!(
            "the download of {} failed its check ({why}), and was deleted",
            dest.display()
        )));
    }
    std::fs::rename(&part, dest).map_err(|e| {
        CompositeError::Failed(format!(
            "could not move {} to {}: {e}",
            part.display(),
            dest.display()
        ))
    })
}

/// `dest` with `.part` on the end of its whole name, as export writes its
/// output: `ggml-small.en.bin.part`, never `ggml-small.en.part`.
fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// The transfer: `fetch.url` into `part`, until EOS, an error or a cancel.
///
/// **Progress is polled, not probed:** `filesink` answers a byte-position
/// query with what it has written, which is the number the coach cares about
/// and needs no callback on the streaming thread.
fn fetch_to(
    fetch: &Fetch,
    part: &Path,
    progress: &mut dyn FnMut(u8),
    cancel: &AtomicBool,
) -> Result<(), CompositeError> {
    let make = |factory: &str| {
        gst::ElementFactory::make(factory)
            .build()
            .map_err(|e| CompositeError::Failed(format!("{factory} is missing: {e}")))
    };
    let src = make("souphttpsrc")?;
    src.set_property("location", &fetch.url);
    // Off, so a server that happens to send ICY headers can't turn this into
    // a radio stream with caps of its own.
    src.set_property("iradio-mode", false);
    let sink = make("filesink")?;
    sink.set_property("location", part);
    let pipeline = gst::Pipeline::new();
    pipeline
        .add_many([&src, &sink])
        .expect("add the download elements");
    src.link(&sink).expect("link souphttpsrc to filesink");
    let pipeline = Stopper(pipeline);
    let bus = pipeline.bus().expect("a pipeline has a bus");

    let failed =
        |what: String| CompositeError::Failed(format!("could not download {}: {what}", fetch.url));
    if pipeline.set_state(gst::State::Playing).is_err() {
        // The element's own reason, if it posted one before refusing.
        return Err(failed(
            bus.pop_filtered(&[gst::MessageType::Error])
                .and_then(|msg| match msg.view() {
                    gst::MessageView::Error(err) => Some(crate::error_text(err)),
                    _ => None,
                })
                .unwrap_or_else(|| "it would not start".into()),
        ));
    }
    let mut reported = 0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CompositeError::Cancelled);
        }
        if let Some(msg) =
            bus.timed_pop_filtered(POLL, &[gst::MessageType::Eos, gst::MessageType::Error])
        {
            match msg.view() {
                gst::MessageView::Error(err) => return Err(failed(crate::error_text(err))),
                _ => {
                    // All of it, which a fast transfer can reach between
                    // two polls.
                    if reported != 100 {
                        progress(100);
                    }
                    return Ok(());
                }
            }
        }
        if let Some(done) = pipeline.query_position::<gst::format::Bytes>() {
            let percent = (u64::from(done) * 100 / fetch.bytes.max(1)).min(100) as u8;
            if percent != reported {
                reported = percent;
                progress(percent);
            }
        }
    }
}

/// Whether `part` is the file `fetch` describes: `Ok(Err(why))` when it
/// isn't, `Err` when it couldn't be read or the check was cancelled.
///
/// **The file is hashed, not the stream.** What is checked is then exactly
/// what gets renamed, whatever `filesink` did or didn't write. The cost is
/// glib's speed, not the disk's: about 90 MB/s on the reference laptop
/// (148 MB in 1.6 s), so `small.en` sits at 100% for some five seconds while
/// it is checked.
fn check(
    fetch: &Fetch,
    part: &Path,
    cancel: &AtomicBool,
) -> Result<Result<(), String>, CompositeError> {
    let unreadable = |e: std::io::Error| {
        CompositeError::Failed(format!("could not read {}: {e}", part.display()))
    };
    let mut file = File::open(part).map_err(unreadable)?;
    // The cheap check first, and the one that says the most when it fails:
    // a transfer that stopped early.
    let len = file.metadata().map_err(unreadable)?.len();
    if len != fetch.bytes {
        return Ok(Err(format!("{len} bytes of {}", fetch.bytes)));
    }
    let mut sum = glib::Checksum::new(glib::ChecksumType::Sha256).expect("glib has sha256");
    let mut chunk = vec![0; HASH_CHUNK];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CompositeError::Cancelled);
        }
        let n = file.read(&mut chunk).map_err(unreadable)?;
        if n == 0 {
            break;
        }
        sum.update(&chunk[..n]);
    }
    let got = sum.string().expect("a sha256 has a hex form");
    Ok(if got == fetch.sha256 {
        Ok(())
    } else {
        Err(format!("sha256 {got}, expected {}", fetch.sha256))
    })
}

/// A local HTTP server for the tests here and the transcriber's: **no test
/// touches Hugging Face.**
#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    use super::*;

    /// A body with no repeating pattern a short read could hide in.
    pub(crate) fn body() -> Vec<u8> {
        (0..1_000_000u32).map(|i| (i % 251) as u8).collect()
    }

    /// [`body`]'s sha256, from `hashlib` and not from `glib`, so a hash the
    /// download computes wrongly can't agree with itself here.
    pub(crate) const BODY_SHA256: &str =
        "2c030d49ec131bfbbb446ad21e7a2f12cdb4f2f4f3fda3ac709dd2e68a4646c7";

    /// What the test server does with each request.
    #[derive(Clone, Copy)]
    pub(crate) enum Answer {
        /// `200 OK` and the whole body.
        Whole,
        /// `200 OK`, the headers and half the body — then nothing, with the
        /// connection held open, as a transfer stalled mid-way.
        Stall,
        /// This status line and no body.
        Status(&'static str),
    }

    /// An HTTP server on a port of its own answering every request with
    /// `answer`, and the URL to ask it. It lives as long as the test binary:
    /// nothing joins it, and a stalled connection's thread just sleeps.
    pub(crate) fn serve(answer: Answer) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/ggml-test.bin", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || respond(stream, answer));
            }
        });
        url
    }

    fn respond(mut stream: TcpStream, answer: Answer) {
        // The request's headers, whole, before any answer: a reply to half a
        // request is a reset libsoup reports as something else entirely.
        let mut request = Vec::new();
        let mut byte = [0; 1];
        while !request.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => request.push(byte[0]),
                _ => return,
            }
        }
        let body = body();
        let head = |status: &str, len: usize| {
            format!("HTTP/1.1 {status}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n")
        };
        let _ = match answer {
            Answer::Whole => stream
                .write_all(head("200 OK", body.len()).as_bytes())
                .and_then(|()| stream.write_all(&body)),
            Answer::Stall => {
                let _ = stream
                    .write_all(head("200 OK", body.len()).as_bytes())
                    .and_then(|()| stream.write_all(&body[..body.len() / 2]))
                    .and_then(|()| stream.flush());
                std::thread::sleep(Duration::from_secs(600));
                Ok(())
            }
            Answer::Status(status) => stream.write_all(head(status, 0).as_bytes()),
        };
    }

    fn fetch(url: String, sha256: &str) -> Fetch {
        Fetch {
            url,
            sha256: sha256.into(),
            bytes: body().len() as u64,
        }
    }

    fn dir() -> tempfile::TempDir {
        gst::init().unwrap();
        tempfile::tempdir().unwrap()
    }

    /// The file lands at `dest` — in a directory that didn't exist, as the
    /// cache's `models/` doesn't on a first run — byte for byte, with no
    /// `.part` beside it, and the percent climbs to 100.
    #[test]
    fn a_good_file_lands_verified() {
        let dir = dir();
        let dest = dir.path().join("models").join("ggml-test.bin");
        let mut percents = Vec::new();
        download(
            &fetch(serve(Answer::Whole), BODY_SHA256),
            &dest,
            &mut |p| percents.push(p),
            &AtomicBool::new(false),
        )
        .expect("the download succeeds");
        assert!(
            std::fs::read(&dest).unwrap() == body(),
            "the file is not the body"
        );
        assert!(!part_path(&dest).exists(), "the .part was left behind");
        assert_eq!(percents.first(), Some(&0), "{percents:?}");
        assert_eq!(percents.last(), Some(&100), "{percents:?}");
        assert!(percents.is_sorted(), "{percents:?}");
    }

    /// A file that arrives whole but isn't the one asked for is deleted, and
    /// the failure names where it was going — and nothing lands there.
    #[test]
    fn a_wrong_hash_deletes_the_part_and_names_the_path() {
        let dir = dir();
        let dest = dir.path().join("ggml-test.bin");
        let wrong = "0".repeat(64);
        let error = download(
            &fetch(serve(Answer::Whole), &wrong),
            &dest,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .expect_err("the hash does not match");
        let CompositeError::Failed(message) = error else {
            panic!("expected a failure, got {error:?}");
        };
        assert!(
            message.contains(&dest.display().to_string()) && message.contains(BODY_SHA256),
            "the message names neither the path nor what arrived: {message}"
        );
        assert!(!part_path(&dest).exists(), "the .part survived");
        assert!(!dest.exists(), "a file that failed its check was kept");
    }

    /// A cancel mid-transfer is [`CompositeError::Cancelled`], and **no file
    /// lands at `dest`** — the `.part` is left, on purpose (see
    /// [`download`]).
    #[test]
    fn a_cancel_leaves_no_final_file() {
        let dir = dir();
        let dest = dir.path().join("ggml-test.bin");
        let url = serve(Answer::Stall);
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let run = std::thread::spawn({
            let (dest, cancel) = (dest.clone(), cancel.clone());
            move || {
                download(
                    &fetch(url, BODY_SHA256),
                    &dest,
                    &mut |p| {
                        let _ = tx.send(p);
                    },
                    &cancel,
                )
            }
        });
        // Half the body has arrived, so the cancel lands mid-transfer rather
        // than before it starts.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while rx.recv_timeout(Duration::from_secs(15)).expect("progress") < 40 {
            assert!(std::time::Instant::now() < deadline, "no progress");
        }
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(run.join().unwrap(), Err(CompositeError::Cancelled));
        assert!(!dest.exists(), "a cancelled download landed");
    }

    /// A server that refuses fails the download with the server's reason and
    /// the URL — not as a file that failed its check.
    #[test]
    fn a_server_error_fails() {
        let dir = dir();
        let dest = dir.path().join("ggml-test.bin");
        let url = serve(Answer::Status("404 Not Found"));
        let error = download(
            &fetch(url.clone(), BODY_SHA256),
            &dest,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .expect_err("there is nothing there");
        let CompositeError::Failed(message) = error else {
            panic!("expected a failure, got {error:?}");
        };
        assert!(
            message.contains(&format!("could not download {url}")),
            "{message}"
        );
        assert!(!dest.exists());
    }
}
