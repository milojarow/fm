# Audio Preview Playback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **SUPERSEDED 2026-08-01.** Only Task 1 was executed as written, and it shipped
> a crash: `gtk::MediaControls` aborts the process on mp3. What actually shipped
> drives a plain GStreamer `playbin` instead — see the design doc's *The engine*
> section and `src/audio.rs`. Tasks 2 and 3 below still describe the
> `MediaControls` API and are kept only as the record of what was tried.

**Goal:** Play audio inside the preview with real controls, so a file can be stopped and two files can never overlap.

**Architecture:** A new `FilePreview::Audio` variant puts a `gtk::MediaControls` widget on its own Stack page. The `MediaFile` stream is owned by that widget, so changing selection destroys it — overlap becomes unrepresentable rather than defended against. `Enter` on an audio file toggles the stream instead of launching mpv.

**Tech Stack:** Rust 2021, relm4 0.9, gtk4-rs 0.9.7, GStreamer via GTK's built-in media backend.

## Global Constraints

- Design spec: `docs/design/2026-07-28-audio-preview-playback.md`. It is authoritative.
- Comments and identifiers in English. Rustfmt defaults. UI strings in English, matching the app's existing toasts.
- `cargo build` takes about 100 seconds here. Every cargo command needs a 600000 ms timeout or it fails for no reason.
- Build check for every task: no errors, and no warnings beyond this measured upstream baseline of four:

```
2 × warning: hiding a lifetime that's elided elsewhere is confusing
1 × warning: struct `BitsetIter` is never constructed
1 × warning: trait `BitsetExt` is never used
```

- GUI checks run only through `scripts/headless-test.sh`, never in the operator's live session.
- Never `pkill -f` anything; kill by PID.
- `gst-plugins-good` is already installed (done during design, with approval). Without it `GtkMediaFile` reports *"The autoaudiosink element is missing"* and plays nothing.
- Preserve existing behaviour: marks, search, sort, rename, trash, vim navigation, clipboard copy/cut/paste, the tapering column layout, the cursor glow.
- **The harness cannot hear.** Anything about actual sound — an audible blip, correct volume — is the operator's to confirm. State plainly what was verified by observation and what was not.

---

### Task 1: The audio preview page

Landing the cursor on an audio file shows transport controls, paused, with the real duration.

**Files:**
- Modify: `src/component/file_preview.rs` (enum around line 39, dispatch at line 108, `view!` Stack around line 212, `pre_view` around line 490)

**Interfaces:**
- Consumes: nothing.
- Produces: `FilePreview::Audio(gio::File)` enum variant; the `audio_container` and `audio_controls` named widgets.

- [ ] **Step 1: Add the enum variant**

In `src/component/file_preview.rs`, add to `enum FilePreview` beside `Image`:

```rust
    /// Audio file, played by [`FilePreviewWidgets::audio_controls`].
    Audio(gio::File),
```

- [ ] **Step 2: Dispatch audio to it**

In `update_single_file_preview`, add this arm immediately after the `(mime::IMAGE, _)` arm and before `(_, mime::PDF)`:

```rust
            (mime::AUDIO, _) => FilePreview::Audio(file.file.clone()),
```

Verified against the operator's real files: `gio info` reports `audio/mpeg` for
an mp3 and `audio/x-opus+ogg` for a WhatsApp voice note, so both match
`mime::AUDIO`.

- [ ] **Step 3: Add the Stack page**

In the `view!` block, after the `#[name = "picture"]` page and before
`#[name = "text_container"]`:

```rust
                    #[name = "audio_container"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_halign: gtk::Align::Center,
                        set_valign: gtk::Align::Center,
                        set_spacing: 12,

                        gtk::Image {
                            set_icon_name: Some("audio-x-generic-symbolic"),
                            set_pixel_size: 96,
                        },

                        #[name = "audio_controls"]
                        gtk::MediaControls {
                            set_hexpand: true,
                        },
                    },
```

- [ ] **Step 4: Build the stream when the file changes**

In `pre_view`, add this arm to the `match &self.preview`:

```rust
            Some(FilePreview::Audio(file)) => {
                // `pre_view` runs on every view update, not only when the
                // selection changes. Rebuilding unconditionally would restart
                // playback on every keystroke, so the stream is replaced only
                // when it is actually a different file.
                let current = widgets
                    .audio_controls
                    .media_stream()
                    .and_downcast::<gtk::MediaFile>()
                    .and_then(|media| media.file());

                if current.as_ref() != Some(file) {
                    // Silence the outgoing stream explicitly rather than
                    // trusting finalisation order to do it. This is the line
                    // that makes two files unable to overlap.
                    if let Some(previous) = widgets.audio_controls.media_stream() {
                        previous.pause();
                    }

                    let stream = gtk::MediaFile::for_file(file);

                    // Preparing is what lets the controls show a duration
                    // before the first play. Nothing starts a pipeline until
                    // `play`, so it is paused again immediately.
                    stream.play();
                    stream.pause();

                    widgets.audio_controls.set_media_stream(Some(&stream));
                }

                widgets.stack.set_visible_child(&widgets.audio_container);
            }
```

- [ ] **Step 5: Build**

Run: `cargo build 2>&1 | grep -E "^error" -A 5`
Expected: no output. If the `view!` macro rejects `gtk::MediaControls`, try
`#[name = "audio_controls"]` before any setter line — the macro is order-sensitive.

- [ ] **Step 6: Verify the controls appear with the right duration**

```bash
cd ~/.local/src/fm
mkdir -p target/headless/audio
cp ~/downloads/*.ogg target/headless/audio/voice.ogg
cp ~/downloads/ook.mp3 target/headless/audio/
echo "expected duration:"; ffprobe -v error -show_entries format=duration -of default=nw=1 target/headless/audio/voice.ogg
START_DIR=$PWD/target/headless/audio ./scripts/headless-test.sh audio-idle
```

Read `target/headless/audio-idle.png`. Expected: a music-note icon, a transport
row with a play button and a progress bar, and a total time that matches
`ffprobe` to the second. Expected: `no panics`.

If the total reads `0:00`, preparing did not happen — check the `play()`/`pause()`
pair survived, and that `gst-plugins-good` is installed
(`gst-inspect-1.0 | grep -c autoaudiosink` must print at least 1).

- [ ] **Step 7: Commit**

```bash
git add src/component/file_preview.rs
git commit -m "preview audio files with transport controls"
```

---

### Task 2: Enter plays and pauses

**Files:**
- Modify: `src/component/file_preview.rs` (`FilePreviewMsg`, `update_with_view`)
- Modify: `src/component/directory_list.rs` (`OpenSelected` arm)
- Modify: `src/component/app.rs` (`AppMsg`, its match)

**Interfaces:**
- Consumes: `FilePreview::Audio` and the `audio_controls` widget from Task 1.
- Produces: `FilePreviewMsg::ToggleAudio`; `AppMsg::ToggleAudioPreview`.

- [ ] **Step 1: Add the preview message**

In `src/component/file_preview.rs`, add to `enum FilePreviewMsg`:

```rust
    /// Start or stop the audio currently in the preview.
    ToggleAudio,
```

- [ ] **Step 2: Handle it**

Add this arm to the preview's `update_with_view` match:

```rust
            FilePreviewMsg::ToggleAudio => {
                let Some(stream) = widgets.audio_controls.media_stream() else {
                    return;
                };

                if stream.error().is_some() {
                    // A codec this build cannot decode. Toggling a player that
                    // cannot play would make the key look dead, so hand the file
                    // to the system handler — which is what opened it before
                    // this feature existed.
                    if let Some(FilePreview::Audio(file)) = &self.preview {
                        if let Err(e) = gio::AppInfo::launch_default_for_uri(
                            file.uri().as_str(),
                            None::<&gio::AppLaunchContext>,
                        ) {
                            error!("unable to open audio externally: {}", e);
                        }
                    }
                    return;
                }

                if stream.is_playing() {
                    stream.pause();
                } else {
                    stream.play();
                }
            }
```

- [ ] **Step 3: Route Enter on audio away from mpv**

In `src/component/directory_list.rs`, replace the `OpenSelected` arm with:

```rust
            DirectoryMessage::OpenSelected => {
                if let Some(info) = self.cursor_file_info().as_ref() {
                    // The panel already holds the content type, so it decides
                    // here rather than making the app ask for data it has.
                    let is_audio = info
                        .content_type()
                        .and_then(|content_type| gio::content_type_get_mime_type(&content_type))
                        .is_some_and(|mime| mime.starts_with("audio/"));

                    if is_audio {
                        sender.output(AppMsg::ToggleAudioPreview).unwrap();
                    } else {
                        open_application_for_file(&info.file().unwrap(), &sender);
                    }
                }
            }
```

- [ ] **Step 4: Forward it**

In `src/component/app.rs`, add to `AppMsg`:

```rust
    /// Start or stop the audio in the preview (`Enter` on an audio file).
    ToggleAudioPreview,
```

and this arm to `update_with_view`, beside `AppMsg::ClipboardPaste`:

```rust
            AppMsg::ToggleAudioPreview => {
                self.file_preview.emit(FilePreviewMsg::ToggleAudio);
            }
```

- [ ] **Step 5: Build**

Run: `cargo build 2>&1 | grep -E "^error" -A 5`
Expected: no output.

- [ ] **Step 6: Verify the elapsed time advances, then holds**

```bash
cd ~/.local/src/fm
KEYS_ARGS="-k Return" START_DIR=$PWD/target/headless/audio ./scripts/headless-test.sh audio-playing
```

The harness screenshots 2.5 s after the keystroke. Read
`target/headless/audio-playing.png`: the elapsed time must be non-zero and the
progress bar must have moved off its left edge.

Then confirm a second `Enter` stops it:

```bash
cd ~/.local/src/fm
KEYS_ARGS="-k Return -k Return" START_DIR=$PWD/target/headless/audio ./scripts/headless-test.sh audio-paused
```

Read that screenshot: the button must show play rather than pause, and the
elapsed time must be frozen partway through rather than at `0:00`.

- [ ] **Step 7: Verify a non-audio file still opens normally**

```bash
cd ~/.local/src/fm
echo hola > target/headless/audio/nota.txt
KEYS_ARGS="-k Return" START_DIR=$PWD/target/headless/audio ./scripts/headless-test.sh audio-passthrough
grep -i "opening.*in external application" target/headless/fm.log && echo "external handler still used"
```

Sort order decides which row `Enter` lands on; if it lands on an audio file,
add `-k j` before `-k Return` until the log line names `nota.txt`.

- [ ] **Step 8: Commit**

```bash
git add src/component/file_preview.rs src/component/directory_list.rs src/component/app.rs
git commit -m "toggle preview audio with Enter instead of launching mpv"
```

---

### Task 3: The guarantees, and what preparing costs

Verification. Code changes only if a check fails.

**Files:** none expected.

**Interfaces:**
- Consumes: everything from Tasks 1 and 2.
- Produces: nothing.

- [ ] **Step 1: Moving the cursor stops playback**

```bash
cd ~/.local/src/fm
KEYS_ARGS="-k Return -k j" START_DIR=$PWD/target/headless/audio ./scripts/headless-test.sh audio-moved
```

Read the screenshot. Expected: the preview now shows whatever the new row is,
and if that row is the second audio file its controls read `0:00` — a fresh
stream, not the previous one still running. Expected: `no panics`.

- [ ] **Step 2: Two audio files cannot overlap**

```bash
cd ~/.local/src/fm
KEYS_ARGS="-k Return -k j -k Return" START_DIR=$PWD/target/headless/audio ./scripts/headless-test.sh audio-second
```

Read the screenshot: exactly one transport row exists, and its elapsed time
belongs to the second file — its total duration must match the second file, not
the first. Overlap is proven impossible structurally: the widget holds one
stream and Task 1 pauses the outgoing one before replacing it. This step
confirms the structure behaves as designed; it cannot listen.

- [ ] **Step 3: Measure what preparing costs**

This is the risk the spec named and refused to assume away.

```bash
cd ~/.local/src/fm
mkdir -p target/headless/manyaudio target/headless/manytext
for i in $(seq 1 40); do
  cp target/headless/audio/ook.mp3 target/headless/manyaudio/track-$i.mp3
  echo "plain text $i" > target/headless/manytext/note-$i.txt
done

echo "=== 20 rows of audio ==="
time (KEYS_ARGS="$(for i in $(seq 1 20); do printf -- '-k j '; done)" \
  START_DIR=$PWD/target/headless/manyaudio ./scripts/headless-test.sh scroll-audio) 2>&1 | tail -4

echo "=== 20 rows of text, same shape ==="
time (KEYS_ARGS="$(for i in $(seq 1 20); do printf -- '-k j '; done)" \
  START_DIR=$PWD/target/headless/manytext ./scripts/headless-test.sh scroll-text) 2>&1 | tail -4
```

The harness paces keystrokes itself, so the wall clock is dominated by that, not
by preparing. What matters is the comparison and the screenshots: if the audio
run takes materially longer than the text run, or its final screenshot shows a
stale or half-drawn preview, preparing is too expensive.

If it is: drop the `stream.play(); stream.pause();` pair from Task 1 Step 4 and
build the stream unprepared. The duration then reads `0:00` until `Enter`,
which the spec already names as the accepted fallback. Record which way it went.

- [ ] **Step 4: A file it cannot decode falls back instead of going dead**

```bash
cd ~/.local/src/fm
head -c 4096 /dev/urandom > target/headless/audio/broken.mp3
KEYS_ARGS="-k Return" START_DIR=$PWD/target/headless/audio ./scripts/headless-test.sh audio-broken
grep -iE "opening.*external|error" target/headless/fm.log | head -3
```

Navigate to `broken.mp3` with `-k j` as needed. Expected: no panic, and either
the error page or the external-handler log line — never a silent player that
looks functional.

- [ ] **Step 5: The primary flow still works**

```bash
cd ~/.local/src/fm
./scripts/headless-test.sh regression "jjjlkjhjjonjj"
```

Expected: `no panics`, tapering columns and the cyan cursor glow intact. A past
regression in this fork shipped because only the new feature was exercised.

- [ ] **Step 6: Full suite and warnings**

Run: `cargo test --lib 2>&1 | tail -2`
Expected: `test result: ok. 46 passed` — this feature adds no unit tests, because
every part of it is GTK widget wiring with no pure logic to isolate.

Run: `cargo build 2>&1 | grep -E "^warning" | sort | uniq -c`
Expected: exactly the four-line upstream baseline.

- [ ] **Step 7: Clean up and install**

```bash
cd ~/.local/src/fm
rm -rf target/headless/audio target/headless/manyaudio target/headless/manytext
git add -u && git commit -m "verify audio preview behaviour" || echo "nothing to commit"
git push origin HEAD:master
cargo install --git https://github.com/milojarow/fm fm --force
```

Install from the **fork**, never `--path`: `cargo install --path` rewrites the
registered source to a local directory, and `cargo install-update` updates from
whatever is registered, which quietly ends the fork's update-proofing.

- [ ] **Step 8: Hand over honestly**

Tell the operator to launch `fm` themselves. Report what was observed in
screenshots, and state plainly that **no step in this plan could hear anything** —
whether audio is audible, at the right volume, and free of a blip when the
cursor lands on a file, is theirs to confirm. Name the blip specifically: Task 1
prepares by calling `play()` then `pause()`, and if a short sound escapes
between the two, the fix is the deferred-preparation fallback in Step 3.
