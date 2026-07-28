# Clipboard copy, cut and paste

Status: approved, ready for implementation
Date: 2026-07-28

## Goal

`Ctrl+C` / `Ctrl+X` on the marked entries, `Ctrl+V` in another directory. The
clipboard is the system one, so the same copy pastes into Nautilus, a mail
attachment or a browser upload dialog — and files copied in those apps paste
into `fm`.

## What exists today

Measured, not assumed:

| Question | Answer |
|---|---|
| Any clipboard code? | None. `grep -rniE "clipboard\|paste\|\bcopy\b\|\bcut\b\|yank" src/` returns only two `derive(Copy)` lines. |
| Any copy operation? | None. `ops.rs` has `move_()` and nothing else; dropping a file **moves** it. |
| Do `Ctrl` keys reach the controller? | No. `app.rs:556` returns `Proceed` for anything carrying `CONTROL_MASK` or `ALT_MASK`. |
| Context-menu actions | six, none of them copy or paste. |

What can be reused: the transfer machinery — `AppMsg::Transfer`, `ops::Progress`,
and the `TransferProgress` component behind the header spinner — already drives
the drag-and-drop move.

## Verified APIs

Checked against the crate sources actually in the lockfile before designing on
top of them.

| Need | API | Where |
|---|---|---|
| Write a custom mime type | `gdk::ContentProvider::for_bytes(mime, &Bytes)` | gdk4-0.9.6 `auto/content_provider.rs:28` |
| Offer several types at once | `gdk::ContentProvider::new_union(&[..])` | same, line 50 |
| Read it back | `gdk::Clipboard::read_future(&[&str], Priority) -> (gio::InputStream, GString)` | gdk4-0.9.6 `clipboard.rs:69` |
| Copy with progress | `gio::File::copy_future(dest, flags, prio) -> (Future<Result<()>>, Stream<(i64, i64)>)` | gio-0.20.12 `file.rs:451` |
| Walk a tree | `gio::File::enumerate_children_future` | gio-0.20.12 `file.rs:339` |

`copy_future` has the same shape as the `move_future` that `ops::move_` already
consumes, so the copy path drops into the existing progress plumbing unchanged.

## The clipboard format

The de-facto freedesktop format, written and read by Nautilus, Thunar, PCManFM
and Caja:

```
mime:    x-special/gnome-copied-files
payload: copy\nfile:///home/milo/a.txt\nfile:///home/milo/b.txt
```

The first line is `copy` or `cut` — that is where the difference between
`Ctrl+C` and `Ctrl+X` lives. Published in a union with `text/uri-list`
(CRLF-separated URIs, no operation word), which apps that never heard of the
GNOME type still understand: file pickers, upload dialogs, mail composers.

Reading prefers `x-special/gnome-copied-files`. Falling back to `text/uri-list`
means the operation word is missing, so it is treated as `copy` — the safe
reading, since assuming `cut` would delete someone else's files.

## What gets copied

`Directory::selected_file_info()`: the marked entries when there are any,
otherwise the row under the cursor. The same rule trash and permanent delete
use. One selection rule for every batch operation, not a second one invented
here.

## Where it pastes

The directory listed by the cursor's panel. **Not** the directory sitting under
the cursor: standing on `projects/` and pasting drops the files beside
`projects/`, not inside it. Same as ranger, whose `paste` defaults to
`dest = self.thistab.path` (`ranger/core/actions.py:1597`).

## Keys

`Ctrl+C`, `Ctrl+X` and `Ctrl+V` are intercepted before the existing
`CONTROL_MASK | ALT_MASK` bail at `app.rs:556`; every other modified key keeps
falling through, so the `Ctrl+H` hidden-files accel is untouched.

The focus guard above it still wins. With the caret inside the rename field or
the search entry, `Ctrl+C` copies **text**, because that guard returns `Proceed`
before any of this runs. Keep that ordering.

## Collisions

Never overwrite. `notas.txt` lands as `notas (copy).txt`, then
`notas (copy 2).txt`, inserting before the final extension. Dotfiles keep their
leading dot: `.bashrc` becomes `.bashrc (copy)`. A file with several suffixes
splits at the last one only: `archive.tar.gz` becomes `archive.tar (copy).gz` —
predictable beats clever.

Useful side effect: pasting into the directory you copied from duplicates the
file.

The suffix is English, matching the rest of the app's UI ("2 files moved to
trash", "Showing hidden files").

## Copying directories

The bulk of the work. `gio` refuses to copy a directory — `copy` fails with
`WOULD_RECURSE` — so a tree is handled in two phases:

1. **Walk** with `enumerate_children_future`, collecting every file and the
   total byte count.
2. **Copy** file by file, recreating the directory structure, feeding the total
   from phase 1 into the existing progress bar.

The extra metadata pass is what lets the bar have a real total instead of one
that grows as it goes.

Symlinks are copied as links, not followed: `FileCopyFlags::NOFOLLOW_SYMLINKS`.
Following them would silently duplicate whole trees and can loop. (Not in the
design as presented — added during review, for the same reason as the guard
below.)

**A directory is never pasted inside itself.** Before starting, every destination
path is checked against every source: if the destination is the source or sits
under it, that entry is skipped and reported.

*Corrected after implementing.* This section first claimed such a paste would
have the walk feeding itself and fill the disk. It would not: the walk completes
before a single byte is copied, so the entry list is finite by construction and
the two-phase design prevents runaway recursion on its own. The guard is still
right — pasting a directory into itself is nonsense and produces a bizarre nested
copy — but it protects against confusion, not against a full disk. Leaving an
overstated warning in an approved document is its own kind of lie.

## Cut

On paste, a `cut` payload routes to `ops::move_()` instead of the copy path —
already written, already reports progress.

- Cutting and pasting into the same directory is detected and skipped.
- After a successful cut-paste the clipboard is cleared, because the source
  files no longer exist and a second paste would fail on every one of them.

## Code layout

```
src/clipboard.rs   NEW. Pure: serialise and parse the payload, and generate a
                   collision-free name. Text in, text out — unit-tested with
                   cargo test, no display needed.
src/ops.rs         copy_recursive() and paste()
src/component/app.rs
                   Ctrl+C / Ctrl+X / Ctrl+V in the key controller, the messages
                   they send, and reading the clipboard on paste
src/component/directory_list.rs
                   hand the app the marked files and the panel's directory
```

The two parts with sharp edges — the wire format and the renaming — stay pure
and tested, the same way `layout.rs` and `path_title.rs` are.

## Feedback and errors

A toast on copy ("2 files copied") and on paste completion, matching the
existing toast style. Per-file failures are collected and reported through the
alert the trash operation already uses; one failure does not abort the rest of
the batch.

Marks survive a copy, as they do in ranger. After a cut-paste the sources are
gone; marks are URI-keyed, so the stale ones simply stop matching anything.

Empty cases, all of them toasts rather than silence — a keypress that appears to
do nothing is the thing that makes a user press it again:

- `Ctrl+C` with nothing marked and no cursor row: "Nothing to copy".
- `Ctrl+V` with an empty clipboard, or one holding text rather than files:
  "Nothing to paste".
- Every source skipped by a guard above: the toast says how many and why.

## Out of scope

- A progress dialog richer than the existing header spinner and its popover.
- Undo.
- Copying to or from remote gvfs locations. Local paths only; a paste whose
  source has no local path is reported and skipped.
- Duplicating the whole thing onto the context menu. Keyboard only for now.

## Verification

In the nested headless sway harness, with `RUST_BACKTRACE=1`:

1. Mark two files, `Ctrl+C`, navigate elsewhere, `Ctrl+V` — both arrive, sources
   remain, toast counts two.
2. Nothing marked: `Ctrl+C` takes the row under the cursor alone.
3. Paste into the source directory — `(copy)` suffixes appear, originals intact.
4. Paste twice into the same place — `(copy)` then `(copy 2)`.
5. Copy a directory with nested subdirectories — structure and contents arrive
   whole.
6. `Ctrl+X` then paste — sources gone from the origin, present at the
   destination, clipboard cleared afterwards.
7. `Ctrl+X` and paste into the same directory — nothing happens, nothing lost.
8. Interop, the point of the whole exercise: copy in `fm`, paste in another file
   manager; copy there, paste in `fm`.
9. `Ctrl+C` with the caret in the search entry still copies text.
10. Exercise the primary flow afterwards — select, navigate, open — since a past
    regression in this fork shipped from touching the models without doing so.
