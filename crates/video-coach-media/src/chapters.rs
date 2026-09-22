//! Chapters in an exported MP4: a Nero `chpl` box, spliced into the finished
//! file's `moov` (match vision spec C3).
//!
//! `mp4mux` has no `GstTocSetter`, so the box is written by hand after the
//! run, into the `.part` before its rename. Export reserves `moov` at the
//! front of the file with a `free` box after it; the splice appends `chpl` to
//! `moov/udta`, grows `udta` and `moov` by its size and shrinks the `free` by
//! the same. The file's size and `mdat` are untouched, so no `stco` offset
//! moves. Only `ffprobe` (and so mpv) and VLC read `chpl`.

use std::fmt;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

/// `chpl` counts its chapters in a `u8`.
pub const MAX_CHAPTERS: usize = 255;
/// And a title's length.
const MAX_TITLE_BYTES: usize = 255;
/// A plain box header: a `u32` size and the type.
const HEADER: u64 = 8;

/// What [`splice`] did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterOutcome {
    /// This many chapters are in the file.
    Written(usize),
    /// The file is exactly as it was.
    Skipped(SkipReason),
}

/// Why a file keeps no chapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// There were none to write: a plan of fewer than two entries.
    NoChapters,
    /// The `free` box after `moov` can't take the box and still be a box
    /// (a remainder of 1–7 bytes), or there is no `free` there at all.
    NoRoom,
    /// The muxer didn't honour the reserve and wrote `moov` at the end.
    MoovAfterMdat,
}

impl fmt::Display for ChapterOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChapterOutcome::Written(n) => write!(f, "{n} chapters"),
            ChapterOutcome::Skipped(SkipReason::NoChapters) => write!(f, "no chapters"),
            ChapterOutcome::Skipped(SkipReason::NoRoom) => {
                write!(f, "no chapters (no room left in the moov reserve)")
            }
            ChapterOutcome::Skipped(SkipReason::MoovAfterMdat) => {
                write!(f, "no chapters (moov was written after mdat)")
            }
        }
    }
}

/// One box: where it starts, its header's length (8, or 16 for a 64-bit
/// `largesize`), its whole size and its type.
#[derive(Debug, Clone, Copy)]
struct Box4 {
    start: u64,
    header: u64,
    size: u64,
    kind: [u8; 4],
}

impl Box4 {
    fn end(&self) -> u64 {
        self.start + self.size
    }
}

fn invalid(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

/// The boxes in `[from, to)`, reading each header with `read(at, buf)`. A
/// size of 0 runs to `to`.
fn walk(
    from: u64,
    to: u64,
    mut read: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<Vec<Box4>> {
    let mut boxes = Vec::new();
    let mut at = from;
    while at < to {
        let room = |need: u64| {
            if to - at < need {
                Err(invalid(format!("a box header at {at} is cut short")))
            } else {
                Ok(())
            }
        };
        let mut head = [0u8; 16];
        room(HEADER)?;
        read(at, &mut head[..8])?;
        let kind = head[4..8].try_into().expect("four bytes");
        let (header, size) = match u32::from_be_bytes(head[..4].try_into().expect("four")) {
            0 => (HEADER, to - at),
            1 => {
                room(16)?;
                read(at + 8, &mut head[8..16])?;
                (16, u64::from_be_bytes(head[8..].try_into().expect("eight")))
            }
            n => (HEADER, u64::from(n)),
        };
        if size < header || size > to - at {
            return Err(invalid(format!(
                "a box at {at} of {size} bytes doesn't fit"
            )));
        }
        boxes.push(Box4 {
            start: at,
            header,
            size,
            kind,
        });
        at += size;
    }
    Ok(boxes)
}

/// Overwrites the size field of `b`, whose bytes start at `buf[0]`, with
/// `size`.
fn set_size(buf: &mut [u8], b: &Box4, size: u64) -> io::Result<()> {
    if b.header == 16 {
        buf[8..16].copy_from_slice(&size.to_be_bytes());
    } else {
        let size = u32::try_from(size).map_err(|_| invalid(format!("{size} bytes in a u32")))?;
        buf[..4].copy_from_slice(&size.to_be_bytes());
    }
    Ok(())
}

/// A box of `kind` around `payload`.
fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// `chpl` version 1: 4 reserved bytes, a `u8` count, and per chapter a `u64`
/// start in 100 ns units, a `u8` length and the UTF-8 title.
fn chpl(chapters: &[(f64, &str)]) -> Vec<u8> {
    let mut payload = vec![1, 0, 0, 0, 0, 0, 0, 0, chapters.len() as u8];
    for &(at, title) in chapters {
        let title = truncate(title, MAX_TITLE_BYTES);
        payload.extend_from_slice(&((at * 1e7).round() as u64).to_be_bytes());
        payload.push(title.len() as u8);
        payload.extend_from_slice(title.as_bytes());
    }
    boxed(b"chpl", &payload)
}

/// `s`'s longest prefix of at most `max` bytes that ends on a `char`.
fn truncate(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Writes `chapters` into the MP4 at `path` in place, keeping the first
/// [`MAX_CHAPTERS`]. An error means the file may be half-written: `moov` is
/// one positioned write, but a failed write can still leave part of it.
pub fn splice(path: &Path, chapters: &[(f64, &str)]) -> io::Result<ChapterOutcome> {
    if chapters.is_empty() {
        return Ok(ChapterOutcome::Skipped(SkipReason::NoChapters));
    }
    if chapters.len() > MAX_CHAPTERS {
        eprintln!(
            "chapters: kept the first {MAX_CHAPTERS} of {}; dropped {}",
            chapters.len(),
            chapters.len() - MAX_CHAPTERS
        );
    }
    let chapters = &chapters[..chapters.len().min(MAX_CHAPTERS)];

    let file = File::options().read(true).write(true).open(path)?;
    let len = file.metadata()?.len();
    let top = walk(0, len, |at, buf| file.read_exact_at(buf, at))?;
    let Some(m) = top.iter().position(|b| &b.kind == b"moov") else {
        return Err(invalid("no moov box".into()));
    };
    if top[..m].iter().any(|b| &b.kind == b"mdat") {
        return Ok(ChapterOutcome::Skipped(SkipReason::MoovAfterMdat));
    }
    let moov = top[m];
    let Some(free) = top.get(m + 1).filter(|b| &b.kind == b"free").copied() else {
        return Ok(ChapterOutcome::Skipped(SkipReason::NoRoom));
    };

    let mut buf = vec![0u8; moov.size as usize];
    file.read_exact_at(&mut buf, moov.start)?;
    let children = walk(moov.header, moov.size, |at, out| {
        out.copy_from_slice(&buf[at as usize..at as usize + out.len()]);
        Ok(())
    })?;
    let udta = children.iter().find(|b| &b.kind == b"udta").copied();
    let mut insert = chpl(chapters);
    if udta.is_none() {
        insert = boxed(b"udta", &insert);
    }
    let grow = insert.len() as u64;
    let rest = match free.size.checked_sub(grow) {
        Some(rest) if rest == 0 || rest >= HEADER => rest,
        _ => return Ok(ChapterOutcome::Skipped(SkipReason::NoRoom)),
    };

    // `chpl` goes at the end of `udta`, or a new `udta` at the end of `moov`.
    let at = udta.map_or(moov.size, |u| u.end()) as usize;
    if let Some(u) = udta {
        let start = u.start as usize;
        set_size(&mut buf[start..], &u, u.size + grow)?;
    }
    buf.splice(at..at, insert);
    set_size(&mut buf, &moov, moov.size + grow)?;
    // What is left of the `free` needs only its header: its payload is
    // never read.
    if rest > 0 {
        let rest = u32::try_from(rest).map_err(|_| invalid(format!("a free of {rest} bytes")))?;
        buf.extend_from_slice(&rest.to_be_bytes());
        buf.extend_from_slice(b"free");
    }
    file.write_all_at(&buf, moov.start)?;
    Ok(ChapterOutcome::Written(chapters.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box of `kind` around `payload`, with a 64-bit `largesize` header.
    fn large(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = 1u32.to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(&(payload.len() as u64 + 16).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// A `free` box of `size` bytes in all.
    fn free(size: usize) -> Vec<u8> {
        boxed(b"free", &vec![0; size - 8])
    }

    fn moov(udta: bool) -> Vec<u8> {
        let mut payload = boxed(b"mvhd", &[7; 20]);
        if udta {
            payload.extend(boxed(b"udta", &boxed(b"meta", &[9; 12])));
        }
        payload.extend(boxed(b"trak", &[5; 30]));
        boxed(b"moov", &payload)
    }

    fn mdat() -> Vec<u8> {
        boxed(b"mdat", &[0xaa; 64])
    }

    /// Writes the boxes to a file of their own, keeping the directory alive.
    fn file(parts: &[Vec<u8>]) -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.mp4.part");
        let bytes = parts.concat();
        std::fs::write(&path, &bytes).unwrap();
        (dir, path, bytes)
    }

    /// The boxes in `bytes[from..to]`, as (type, start, size).
    fn boxes(bytes: &[u8], from: usize, to: usize) -> Vec<(String, usize, usize)> {
        walk(from as u64, to as u64, |at, out| {
            out.copy_from_slice(&bytes[at as usize..at as usize + out.len()]);
            Ok(())
        })
        .unwrap()
        .into_iter()
        .map(|b| {
            let kind = String::from_utf8_lossy(&b.kind).into_owned();
            (kind, b.start as usize, b.size as usize)
        })
        .collect()
    }

    /// The `chpl` payload in `bytes`' `moov/udta`, decoded as
    /// `(start in 100 ns, title)`, and asserts every size on the way agrees.
    fn read_chpl(bytes: &[u8]) -> Vec<(u64, String)> {
        let top = boxes(bytes, 0, bytes.len());
        let (_, moov, size) = top.iter().find(|b| b.0 == "moov").unwrap().clone();
        let (_, udta, size) = boxes(bytes, moov + 8, moov + size)
            .into_iter()
            .find(|b| b.0 == "udta")
            .unwrap();
        let (_, chpl, size) = boxes(bytes, udta + 8, udta + size)
            .into_iter()
            .find(|b| b.0 == "chpl")
            .unwrap();
        let body = &bytes[chpl + 8..chpl + size];
        assert_eq!(body[..8], [1, 0, 0, 0, 0, 0, 0, 0], "version 1, reserved");
        let mut at = 9;
        let chapters = (0..body[8])
            .map(|_| {
                let start = u64::from_be_bytes(body[at..at + 8].try_into().unwrap());
                let len = usize::from(body[at + 8]);
                let title = String::from_utf8(body[at + 9..at + 9 + len].to_vec()).unwrap();
                at += 9 + len;
                (start, title)
            })
            .collect();
        assert_eq!(at, body.len(), "the box holds exactly its chapters");
        chapters
    }

    const TWO: [(f64, &str); 2] = [(0.0, "1 / 2 | a"), (5.1, "2 / 2 | b")];

    /// The chapters land in the existing `udta`, beside its `meta`, and the
    /// file keeps its size and its `mdat` byte for byte.
    #[test]
    fn chapters_join_the_existing_udta() {
        let (_dir, path, before) = file(&[boxed(b"ftyp", b"isom"), moov(true), free(400), mdat()]);
        assert_eq!(splice(&path, &TWO).unwrap(), ChapterOutcome::Written(2));
        let after = std::fs::read(&path).unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after[after.len() - 72..], before[before.len() - 72..]);
        assert_eq!(
            read_chpl(&after),
            [(0, "1 / 2 | a".into()), (51_000_000, "2 / 2 | b".into())]
        );
        let kinds: Vec<_> = boxes(&after, 0, after.len())
            .into_iter()
            .map(|b| b.0)
            .collect();
        assert_eq!(kinds, ["ftyp", "moov", "free", "mdat"]);
    }

    #[test]
    fn a_moov_without_udta_gets_one() {
        let (_dir, path, _) = file(&[boxed(b"ftyp", b"isom"), moov(false), free(400), mdat()]);
        assert_eq!(splice(&path, &TWO).unwrap(), ChapterOutcome::Written(2));
        assert_eq!(read_chpl(&std::fs::read(&path).unwrap()).len(), 2);
    }

    /// A `free` exactly the box's size is consumed whole: `mdat` follows
    /// `moov` directly.
    #[test]
    fn a_free_exactly_the_right_size_is_consumed() {
        let need = chpl(&TWO).len();
        let (_dir, path, _) = file(&[moov(true), free(need), mdat()]);
        assert_eq!(splice(&path, &TWO).unwrap(), ChapterOutcome::Written(2));
        let after = std::fs::read(&path).unwrap();
        let kinds: Vec<_> = boxes(&after, 0, after.len())
            .into_iter()
            .map(|b| b.0)
            .collect();
        assert_eq!(kinds, ["moov", "mdat"]);
        assert_eq!(read_chpl(&after).len(), 2);
    }

    /// Too small, or leaving 1–7 bytes that can't be a box: nothing changes.
    #[test]
    fn no_room_leaves_the_file_as_it_was() {
        let need = chpl(&TWO).len();
        for size in [need - 8, need + 1, need + 7] {
            let (_dir, path, before) = file(&[moov(true), free(size), mdat()]);
            assert_eq!(
                splice(&path, &TWO).unwrap(),
                ChapterOutcome::Skipped(SkipReason::NoRoom),
                "a free of {size} for {need}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        let (_dir, path, before) = file(&[moov(true), mdat()]);
        assert_eq!(
            splice(&path, &TWO).unwrap(),
            ChapterOutcome::Skipped(SkipReason::NoRoom)
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_moov_after_mdat_is_left_alone() {
        let (_dir, path, before) = file(&[boxed(b"ftyp", b"isom"), mdat(), moov(true), free(400)]);
        assert_eq!(
            splice(&path, &TWO).unwrap(),
            ChapterOutcome::Skipped(SkipReason::MoovAfterMdat)
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// A 64-bit `mdat` header, and a size-0 box running to the end, are
    /// walked over, not misread as a cut-short file.
    #[test]
    fn a_64_bit_mdat_and_a_box_to_the_end_are_walked() {
        let mut to_end = boxed(b"free", &[0; 8]);
        to_end[..4].copy_from_slice(&0u32.to_be_bytes());
        let (_dir, path, _) = file(&[moov(true), free(400), large(b"mdat", &[0xaa; 40]), to_end]);
        assert_eq!(splice(&path, &TWO).unwrap(), ChapterOutcome::Written(2));
        let after = std::fs::read(&path).unwrap();
        let kinds: Vec<_> = boxes(&after, 0, after.len())
            .into_iter()
            .map(|b| b.0)
            .collect();
        assert_eq!(kinds, ["moov", "free", "mdat", "free"]);
    }

    #[test]
    fn no_chapters_touch_nothing() {
        let (_dir, path, before) = file(&[moov(true), free(400), mdat()]);
        assert_eq!(
            splice(&path, &[]).unwrap(),
            ChapterOutcome::Skipped(SkipReason::NoChapters)
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// `chpl` counts in a `u8`: the first 255 are kept.
    #[test]
    fn at_most_255_chapters_are_written() {
        let titles: Vec<String> = (0..300).map(|n| n.to_string()).collect();
        let chapters: Vec<(f64, &str)> = titles
            .iter()
            .enumerate()
            .map(|(n, t)| (n as f64, t.as_str()))
            .collect();
        let (_dir, path, _) = file(&[moov(true), free(8000), mdat()]);
        assert_eq!(
            splice(&path, &chapters).unwrap(),
            ChapterOutcome::Written(MAX_CHAPTERS)
        );
        let read = read_chpl(&std::fs::read(&path).unwrap());
        assert_eq!(read.len(), MAX_CHAPTERS);
        assert_eq!(read.last().unwrap(), &(254 * 10_000_000, "254".into()));
    }

    /// A title longer than 255 bytes is cut on a character, never inside
    /// one: 100 three-byte characters keep 85 of them, 255 bytes.
    #[test]
    fn a_long_title_is_cut_on_a_character() {
        let title = "€".repeat(100);
        let (_dir, path, _) = file(&[moov(true), free(1000), mdat()]);
        splice(&path, &[(0.0, "a"), (1.0, &title)]).unwrap();
        let read = read_chpl(&std::fs::read(&path).unwrap());
        assert_eq!(read[1].1, "€".repeat(85));
        // Two-byte characters: 255 is odd, so the last one would straddle.
        assert_eq!(truncate(&"é".repeat(200), 255), "é".repeat(127));
    }
}
