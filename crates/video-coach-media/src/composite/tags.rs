//! Writing an export's [`FileTags`] into the MP4 header.
//!
//! The words are `video_coach_core::metadata`'s; this is only the muxer half.
//! `mp4mux` implements `GstTagSetter`, so a tag list set on it before `PLAYING`
//! is written into `moov/udta` when it lays the header out.
//!
//! **Measured on GStreamer 1.24.2** (`mp4mux` from `gst-plugins-good`), by
//! muxing a file with each tag set and reading it back with
//! `ffprobe -show_format`:
//!
//! | tag | in the file |
//! |---|---|
//! | `title`, `comment`, `keywords`, `encoder` | `moov/udta`, read back by name |
//! | `date` (a `glib::Date`) | `©day`, read back as `date=2026-9-21` |
//! | `description` | the XMP packet's `<dc:description>` only |
//! | `datetime` (a `gst::DateTime`) | **ignored** — `mp4mux` writes neither it nor `mvhd`'s creation time from it |
//!
//! So the date is a [`glib::Date`], not a `DateTime`: the day is all `©day`
//! holds anyway. `description` is written even though `ffprobe -show_format`
//! doesn't surface it — it *is* in the file, in the XMP `uuid` box that
//! `GstTagXmpWriter` writes, which is what Adobe's and Apple's readers look
//! at. Nothing else carries a description on this muxer.
//!
//! **Tags cost the `moov` reserve nothing** (measured). `mp4mux` grows the
//! reserved header to fit them rather than spending the sample-table headroom:
//! with tags, without them, and with a 40 KB tag payload,
//! `reserved-duration-remaining` came back identical and the `free` box after
//! `moov` — the one [`crate::chapters::splice`] eats into — stayed exactly 842
//! bytes. The XMP lands in its own `uuid` box after that `free`, so the splice
//! sees the layout it always saw.
//!
//! And they cost the copy path no losslessness: every one of them is a header
//! box beside the tracks, and not a byte of any sample changes.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer::tags;
use video_coach_core::metadata::{CalendarDate, FileTags};

/// Sets `tags` on `mux`, which must be a `GstTagSetter` and must still be
/// `NULL` — the header is laid out at the first buffer, and a tag that arrives
/// after that is not in it.
///
/// An empty field writes no tag, so a project with no scoreboard simply gets
/// fewer of them.
pub(super) fn apply(mux: &gst::Element, tags: &FileTags) {
    let Some(setter) = mux.dynamic_cast_ref::<gst::TagSetter>() else {
        debug_assert!(false, "the muxer implements GstTagSetter");
        return;
    };
    let mut list = gst::TagList::new();
    {
        let list = list.get_mut().expect("the list was just made");
        add::<tags::Title>(list, &tags.title);
        add::<tags::Description>(list, &tags.description);
        add::<tags::Comment>(list, &tags.comment);
        add::<tags::Encoder>(list, &tags.encoder);
        for keyword in &tags.keywords {
            add::<tags::Keywords>(list, keyword);
        }
        if let Some(date) = tags.date.and_then(glib_date) {
            list.add::<tags::Date>(&date, gst::TagMergeMode::Append);
        }
    }
    // `KEEP` is what keeps ours: the encoder sends an `ENCODER` tag of its own
    // downstream ("x264"), and this is the merge mode `mp4mux` applies to the
    // tag events it receives.
    setter.set_tag_merge_mode(gst::TagMergeMode::Keep);
    setter.merge_tags(&list, gst::TagMergeMode::Replace);
}

/// Adds one string tag, or nothing at all when it is empty.
fn add<'a, T>(list: &mut gst::TagListRef, value: &'a str)
where
    T: gst::Tag<'a, TagType = &'a str>,
{
    if !value.is_empty() {
        list.add::<T>(&value, gst::TagMergeMode::Append);
    }
}

/// A [`CalendarDate`] as glib's, or `None` for one no calendar has — which
/// only a hand-edited file or a clock far outside its range can produce.
fn glib_date(date: CalendarDate) -> Option<gst::glib::Date> {
    use gst::glib::DateMonth::*;
    let month = *[
        January, February, March, April, May, June, July, August, September, October, November,
        December,
    ]
    .get(usize::try_from(date.month.checked_sub(1)?).ok()?)?;
    gst::glib::Date::from_dmy(
        u8::try_from(date.day).ok()?,
        month,
        u16::try_from(date.year).ok()?,
    )
    .ok()
}
