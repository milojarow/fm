//! A small audio player over a plain GStreamer `playbin`.
//!
//! Deliberately not `gtk::MediaFile`. That type is a convenience wrapper which
//! forces `GstPlay → playbin3 → decodebin3`, and on this machine that chain
//! aborts the whole process on any mp3:
//!
//! ```text
//! gstdecodebin3.c:3381: mq_slot_handle_stream_start: assertion failed (collection)
//! Bail out!
//! ```
//!
//! It is a GLib assertion — fatal, and nothing Rust can catch. Plain `playbin`
//! decodes the same files without complaint, which is also the conclusion the
//! operator's `peek` reached independently.

use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::*;

/// One file, playing or paused. Dropping it tears the pipeline down.
#[derive(Debug)]
pub struct Player {
    pipeline: gst::Element,
    uri: String,
}

impl Player {
    /// Builds a pipeline for `uri` and prerolls it, so the duration is known
    /// before anything is audible. `State::Paused` decodes enough to answer
    /// that question and opens no audio output.
    pub fn new(uri: &str) -> Option<Self> {
        let pipeline = gst::ElementFactory::make("playbin")
            .property("uri", uri)
            .build()
            .map_err(|e| warn!("unable to build playbin for {uri}: {e}"))
            .ok()?;

        if let Err(e) = pipeline.set_state(gst::State::Paused) {
            warn!("unable to preroll {uri}: {e}");
            return None;
        }

        Some(Player {
            pipeline,
            uri: uri.to_owned(),
        })
    }

    /// The file this player was built for.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn play(&self) {
        // A pipeline that reached the end sits in `Playing` at its final
        // position, where asking for `Playing` again changes nothing — the key
        // would look dead. Rewind first, so play always means play.
        if let (Some(position), Some(total)) = (self.position(), self.duration()) {
            if position >= total {
                self.seek(0);
            }
        }

        let _ = self.pipeline.set_state(gst::State::Playing);
    }

    /// Returns the pipeline to the start, paused, once the file has finished.
    ///
    /// GStreamer leaves a finished pipeline in `Playing` parked on the last
    /// sample, so without this the transport row keeps showing a pause button
    /// over something silent, and the next keypress reads as "pause" instead of
    /// "play again". Called from the preview's tick.
    pub fn rewind_if_finished(&self) -> bool {
        let Some(bus) = self.pipeline.bus() else {
            return false;
        };

        if bus.pop_filtered(&[gst::MessageType::Eos]).is_none() {
            return false;
        }

        self.pause();
        self.seek(0);
        true
    }

    pub fn pause(&self) {
        let _ = self.pipeline.set_state(gst::State::Paused);
    }

    /// Whether sound is actually coming out. A pipeline parked at the end of
    /// the file is still in `Playing`, but nothing is playing, and reporting
    /// that as playing makes a toggle send `pause` to something already silent.
    pub fn is_playing(&self) -> bool {
        if self.pipeline.current_state() != gst::State::Playing {
            return false;
        }

        match (self.position(), self.duration()) {
            (Some(position), Some(total)) => position < total,
            _ => true,
        }
    }

    /// Nanoseconds played so far, once the pipeline can answer.
    pub fn position(&self) -> Option<u64> {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| t.nseconds())
    }

    /// Total nanoseconds, once prerolling has finished. Unknown until then.
    pub fn duration(&self) -> Option<u64> {
        self.pipeline
            .query_duration::<gst::ClockTime>()
            .map(|t| t.nseconds())
    }

    /// Jumps to `nanos`, flushing so the new position takes effect at once.
    pub fn seek(&self, nanos: u64) {
        if let Err(e) = self
            .pipeline
            .seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::from_nseconds(nanos))
        {
            warn!("seek failed: {e}");
        }
    }

    /// The first error the pipeline has posted, if any. Polled rather than
    /// watched: the preview already ticks, and one bus check per tick is
    /// cheaper than keeping a watch alive per file the cursor passes over.
    pub fn error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;

        while let Some(message) = bus.pop_filtered(&[gst::MessageType::Error]) {
            if let gst::MessageView::Error(err) = message.view() {
                return Some(err.error().to_string());
            }
        }

        None
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Explicit teardown. This is what makes two files unable to overlap:
        // the outgoing pipeline is stopped here, before the next one exists,
        // rather than whenever the allocator gets around to it.
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Formats nanoseconds as `m:ss`, the way a transport row reads it. An unknown
/// duration shows as `--:--` rather than a confident zero.
pub fn format_clock(nanos: Option<u64>) -> String {
    match nanos {
        None => "--:--".to_owned(),
        Some(nanos) => {
            let total = nanos / 1_000_000_000;
            format!("{}:{:02}", total / 60, total % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_clock;

    #[test]
    fn formats_minutes_and_seconds() {
        assert_eq!(format_clock(Some(0)), "0:00");
        assert_eq!(format_clock(Some(9_000_000_000)), "0:09");
        assert_eq!(format_clock(Some(68_060_000_000)), "1:08");
        assert_eq!(format_clock(Some(3_723_000_000_000)), "62:03");
    }

    #[test]
    fn an_unknown_duration_is_not_reported_as_zero() {
        // Until the pipeline prerolls there is no duration, and claiming 0:00
        // would read as an empty file rather than as "still working it out".
        assert_eq!(format_clock(None), "--:--");
    }

    #[test]
    fn truncates_rather_than_rounds() {
        // 9.9 s is still 0:09 — a clock that reads 0:10 before the tenth
        // second has elapsed looks broken next to a progress bar.
        assert_eq!(format_clock(Some(9_900_000_000)), "0:09");
    }
}
