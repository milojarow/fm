# Visible progress while copying

Status: approved, ready for implementation
Date: 2026-08-03

## Goal

A copy that takes a while should say so, where the eye already is, without a
click.

## What happens today

The report was "copying a file gives me no visual feedback". That has three
causes, and only the last is about the interface.

### 1. The byte-level progress is thrown away

`src/ops.rs:181`:

```rust
let (operation, _progress) = node.source.copy_future(
```

`copy_future` returns `(future, Stream<Item = (i64, i64)>)` — bytes copied and
bytes total, as they happen. That stream is discarded. `copy_tree` only reports
after a whole file lands, so **a single-file copy has no intermediate progress at
all**: it goes from nothing to done. Even a perfectly placed indicator would show
0% and then 100%.

This is a regression against the file's own existing pattern, not a gap upstream
left. `ops::move_` — written before the clipboard work — does it correctly:

```rust
let (res, mut progress) = file.move_future(...);
relm4::spawn_local(async move {
    while let Some((current, total)) = progress.next().await { ... }
});
```

`copy_tree` should have followed it and did not.

### 2. What is reported sits behind a click

The `TransferProgress` factory renders into a `gtk::ListBox` inside a
`gtk::Popover`, attached to a `MenuButton` in the header (`app.rs`, the
`transfer_progress_button`). The only ambient signal during a copy is a small
spinner appearing in the header bar; the numbers require noticing it and clicking
it.

### 3. Finished transfers are never removed

`grep` for any pop, remove or clear against `self.progress` returns nothing. Once
`transfer_progress_button.set_visible(true)` runs it is never set back, so after
the first copy of a session the header shows a spinner forever — visible in the
operator's own screenshot, after a paste that had already completed. The popover
also accumulates one row per copy for the life of the process.

## Measured context

400 MB copies in 1.1 s on this machine (~360 MB/s). Small copies are effectively
instantaneous; the indicator matters for multi-gigabyte files, slow media and
network mounts. That measurement is what justifies the threshold below rather
than showing an indicator for every copy.

## The engine

`copy_tree` consumes each file's progress stream and folds it into a running
total for the **whole paste**, not the current file. `walk` already sums the tree's
bytes, so the fraction is `(bytes finished + current file's bytes) / tree total`.
Copying a folder of forty files then advances evenly instead of resetting forty
times.

The stream and the copy future must be driven together: awaiting the future
alone leaves the stream unread.

## The interface

The corner `Revealer` — bottom-left, click-through, already the channel for every
message `fm` shows — gains a progress bar under its label. One widget, two modes:

```
while copying   Copying 'pelicula.mkv'
                ████████░░░░░░  1.9 / 4.2 GB

when done       1 file pasted            ← the existing toast
```

No jump in position and no click, because it is the same widget the operator
already watches. The bar is hidden in toast mode.

## The threshold

A 400 ms timeout armed when a transfer starts. If the copy finished first, the
timeout does nothing and only the completion toast appears.

The appearance of the bar is itself the message: it means this one is going to
take a moment.

## What gets fixed alongside

A new `Transfer::Done { id }`, emitted when a transfer ends — whether it
succeeded or failed, and by **both** `copy_tree` and `move_`, so a cut-and-paste
does not leave a hanging row either. The app removes that entry; when none
remain, the header button hides again. Today a finished or failed transfer of
either kind leaves the indicator up for the life of the process.

The popover stays. It already exists and is the only place to see several
transfers at once; what it needed was a lifecycle, not removal.

## Several transfers at once

The corner shows the aggregate: summed bytes over summed totals, described by
name when there is one transfer and by count when there are more.

```
one     Copying 'pelicula.mkv'
many    Copying 3 files
```

## The part with sharp edges

Turning the set of active transfers into *(what text, what fraction)* is a pure
function and is where the tests go:

- one transfer names its file; several report a count
- a total of zero must not produce `NaN` — an unknown total is not a division
- an empty set means the indicator is hidden, not a zero-length bar

Everything else is GTK wiring with no logic to isolate.

## Out of scope

- Cancelling a transfer in flight.
- Transfer speed or time remaining.
- Progress for trash, permanent delete or rename — they are effectively
  instantaneous and were not reported as a problem.
- Replacing the header popover.

## Verification

In the nested headless sway harness, with a file large enough to take longer than
the threshold:

1. Paste a multi-hundred-megabyte file and screenshot mid-copy: the corner shows
   a named description, a partially filled bar, and byte counts that are neither
   zero nor complete.
2. The same paste, screenshotted after: the corner shows the completion toast and
   **the header button is gone**.
3. Paste a small file: no bar appears at all, only the toast.
4. Paste a directory of many files: the bar advances monotonically rather than
   resetting per file.
5. Two pastes in a row: the header button does not accumulate rows, and hides
   between them.
6. Exercise the primary flow — select, navigate, mark, copy, play audio — since a
   past regression in this fork shipped from touching the models without doing so.
