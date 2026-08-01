# Audio playback controls in the preview

Status: approved, ready for implementation
Date: 2026-07-28

## Goal

Play audio inside `fm`, with controls. Two bugs disappear with it:

- Audio started with `Enter` could not be stopped — it played to the end.
- Starting a second file layered it on top of the first.

## What happens today

`Enter` on any file calls `AppInfo::launch_default_for_uri`. For audio the
system handler is `~/.local/share/applications/mpv-audio.desktop`, which runs
`mpv --no-video --really-quiet %U` detached. `fm` keeps no handle on that
process, so it cannot stop it, and a second `Enter` starts a second mpv. Both
reported symptoms follow directly.

The preview has no audio branch at all: `file_preview.rs:108` dispatches on
`(mime.type_(), mime.subtype())` with arms for `IMAGE` and `PDF`, and audio
falls through to the generic icon.

## Evidence

Measured before designing, not assumed.

| Question | Answer |
|---|---|
| Does gtk4-rs expose the media API? | Yes — `media_file.rs`, `media_controls.rs`, `media_stream.rs` in gtk4 0.9.7. |
| Is a GTK4 media backend installed? | `/usr/lib/gtk-4.0/4.0.0/` is empty and no `libmedia-*` exists anywhere — but the probe reported `GtkGstMediaFileBuiltin`, so on this build the GStreamer backend is **compiled in**, not a loadable module. |
| Could it actually play? | Not at first: `Your GStreamer installation is missing a plug-in. The autoaudiosink element is missing.` |
| Why? | `gst-plugins-good` was not installed. Decoders were fine (196 plugins, 6 mp3 decoders via `gst-libav` and `gst-plugins-ugly`); the missing piece was the output sink selector. |
| After installing it? | `prepared: true`, `has_audio: true`, `seekable: true`, duration 68.1 s against `ffprobe`'s 68.06, timestamp advancing. |

**Revised 2026-08-01, after shipping the first attempt and watching it crash.**
`gtk::MediaControls` aborts the process on any mp3:

```
gstdecodebin3.c:3381: mq_slot_handle_stream_start: assertion failed (collection)
Bail out!
```

It is a GLib assertion — fatal, and uncatchable from Rust. Reproduced in 25
lines of Python with no `fm` involved, and bisected to the exact call:
constructing the `MediaFile` is fine, `set_media_stream` is what dies. Every
other format tested (ogg, opus, flac, wav, m4a) was unaffected.

The operator's own `peek` had already hit this and solved it, and its source
says so in a comment: `Gtk.MediaFile` is not GStreamer, it is a convenience
wrapper that forces `GstPlay → playbin3 → decodebin3`, and that chain is what
breaks. **Plain `playbin` decodes the same files without complaint.** Measured
from Rust against the very mp3 that kills `MediaControls`:

| file | duration reported | position | pause | seek | outcome |
|---|---|---|---|---|---|
| 9 s mp3 | 9.038 s | advances | holds | yes | survives, tears down clean |
| 68 s opus | 68.060 s (ffprobe: 68.062) | advances | holds | yes | survives, tears down clean |

So the engine is one layer lower than first designed, and no format is lost.

`gst-plugins-good` was installed during design with the operator's approval:
2.78 MiB download, 8.56 MiB on disk, and `pacman -Sp` confirmed it pulled no new
dependencies. It is worth recording that this gap was not specific to `fm` — any
GTK4 application attempting media playback on this machine failed the same way.

## The engine

A plain GStreamer `playbin`, built through the `gstreamer` crate. The C library
is already installed and is already a hard dependency of gtk4; only the Rust
bindings are added.

The whole surface is six calls, the same ones `peek` uses:

```
ElementFactory::make("playbin").property("uri", uri)
set_state(Playing)  set_state(Paused)  set_state(Null)
query_position::<ClockTime>()          query_duration::<ClockTime>()
```

`gtk::MediaControls` is deliberately **not** used, and neither is
`gtk::MediaFile` beneath it. See the evidence above: that path aborts the
process on mp3, and mp3 is too common to design around.

The cost of dropping a layer is that the transport row is drawn by hand — a
play/pause button, a draggable progress bar, and an elapsed/total label, ticked
by a 200 ms timeout that queries position and duration. That is the work
`MediaControls` used to donate.

## Why overlap stops being possible

The `playbin` is **owned by the preview**. When the cursor moves to another
file, the preview rebuilds its content: the previous pipeline is driven to
`State::Null` before the new one exists — an explicit teardown, not a hope
about drop order.

There is no "stop the previous player" code path, because two streams never
exist at once. The bug is not fixed so much as made unrepresentable.

The consequence, accepted deliberately: moving the cursor stops playback. That
is also a third way to stop a file, alongside the pause button and `Enter`.
Listening while browsing elsewhere would need a player that outlives the
preview — a persistent bar — which is a different feature and out of scope.

## Shape

```
FilePreview::Audio(gio::File)     new enum variant, beside Image / Pdf / Text
  dispatched by (mime::AUDIO, _)  in file_preview.rs:108

"audio" page in the preview Stack file icon, name, and a hand-drawn transport
                                 row: play/pause, progress bar, elapsed/total
```

The model holds the `playbin` and the id of its tick timeout. Replacing the
preview drives the outgoing pipeline to `State::Null` and cancels its tick,
rather than relying on drop order to silence it.

## Keys

`Enter` on an audio file **toggles** play/pause instead of launching mpv. Three
ways to stop, then: the button, `Enter` again, or moving the cursor away.

The audio decision is made in `Directory`, which already holds the `FileInfo`
and its content type; it emits a play message instead of launching. `app.rs`
forwards it to the preview. Putting the test in `app.rs` would mean asking the
panel for data it already has.

Everything that is not audio opens exactly as before. **Video still goes to
mpv** — it was not asked for, and it needs `GtkVideo`, a different widget.

## Prerolling, and the risk it carries

The duration is only known once the pipeline has prerolled, which is what
`State::Paused` does. The preview prerolls on selection, so moving through a
directory of fifty audio files builds and tears down fifty pipelines.

Prerolling opens no audio output — that is the difference between `Paused` and
`Playing`, and it is why this is safe where the old eager `play()`/`pause()`
pair was not. It is still a measurable claim about cost, and the implementation
must measure it rather than assume it. If scrolling drags, the fallback is to
build the pipeline only on `Enter`: the duration stops being visible in advance,
and the scroll stays smooth.

## Errors

A file GStreamer cannot decode posts an error on the `playbin` bus. The preview
watches that bus and, on an error, swaps the transport row for the message —
the same place a poppler failure already lands.

`Enter` on a pipeline that errored hands the file to the system handler rather
than toggling a player that cannot play. The file still opens — in mpv, as it
did before this feature — rather than the key going dead.

## Out of scope

- Video playback in the preview.
- A player that survives navigation.
- Playlists, repeat, shuffle, or gapless playback.
- Changing what non-audio files do on `Enter`.

## Verification

In the nested headless sway harness, with `RUST_BACKTRACE=1`:

1. Land on an audio file: the preview shows controls, paused, with the correct
   duration — compared against `ffprobe` for the same file.
2. `Enter`: the timestamp advances.
3. `Enter` again: it stops advancing and the position holds.
4. Move the cursor to another audio file and press `Enter`: exactly one stream
   is audible. Overlap is checked by asserting the first stream was dropped, not
   by listening.
5. Move the cursor away mid-playback: playback stops.
6. A non-audio file still opens through its usual handler.
7. Scroll `j` through a directory of many audio files and time it against the
   same scroll over non-audio files — this is the preparing risk, measured.
8. A corrupt or codec-less file shows an error rather than a dead player.
9. Exercise the primary flow — select, navigate, mark, copy — since a past
   regression in this fork shipped from touching the models without doing so.
