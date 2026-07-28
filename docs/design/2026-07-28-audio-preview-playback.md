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

`gst-plugins-good` was installed during design with the operator's approval:
2.78 MiB download, 8.56 MiB on disk, and `pacman -Sp` confirmed it pulled no new
dependencies. It is worth recording that this gap was not specific to `fm` — any
GTK4 application attempting media playback on this machine failed the same way.

## The widget

`gtk::MediaControls` — play/pause, a draggable progress bar, volume, and
elapsed/total time, all supplied by GTK. Constructed as
`MediaControls::new(Some(&stream))` where the stream is a
`gtk::MediaFile::for_file(&file)`. Nothing here is hand-drawn or hand-wired.

## Why overlap stops being possible

The `MediaFile` is **owned by the preview**. When the cursor moves to another
file, the preview rebuilds its content: the previous stream is paused and
dropped before the new one exists.

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

"audio" page in the preview Stack file icon, name, and the MediaControls widget
```

The model holds the `MediaFile`. Replacing the preview pauses the outgoing
stream explicitly before dropping it, rather than relying on finalisation order
to silence it.

## Keys

`Enter` on an audio file **toggles** play/pause instead of launching mpv. Three
ways to stop, then: the button, `Enter` again, or moving the cursor away.

The audio decision is made in `Directory`, which already holds the `FileInfo`
and its content type; it emits a play message instead of launching. `app.rs`
forwards it to the preview. Putting the test in `app.rs` would mean asking the
panel for data it already has.

Everything that is not audio opens exactly as before. **Video still goes to
mpv** — it was not asked for, and it needs `GtkVideo`, a different widget.

## Preparing, and the risk it carries

Showing the duration before the first `Enter` requires the stream to be
*prepared*, which builds a GStreamer pipeline. The preview prepares on selection,
so moving through a directory of fifty audio files builds and tears down fifty
pipelines.

This is expected to be fine — preparing opens no audio output, and the preview
already does comparable work loading textures and reading up to 256 KiB of text
— but it is a measurable claim and the implementation must measure it, not
assume it. If scrolling drags, the fallback is to prepare only on `Enter`:
the duration stops being visible in advance, and the scroll stays smooth.

## Errors

A file that cannot be decoded leaves `MediaStream::error()` set.

*Corrected while planning.* This section first said that produces
`FilePreview::Error`, the way a document poppler cannot open already does. It
cannot: poppler fails synchronously while the preview is being built, but
GStreamer prepares asynchronously, so the dispatch has already chosen
`FilePreview::Audio` and drawn the controls long before any error exists. A spec
cannot demand a decision be made before the information arrives.

What actually happens: the controls appear, preparation fails quietly, and the
first `Enter` finds `error()` set and hands the file to the system handler
instead of toggling a player that cannot play. The file still opens — in mpv, as
it did before this feature — rather than the key going dead.

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
