# Clipboard Copy, Cut and Paste Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `Ctrl+C` / `Ctrl+X` on the marked entries and `Ctrl+V` in another directory, through the system clipboard, so the same copy also pastes into Nautilus, a mail attachment or a browser upload dialog.

**Architecture:** A pure `clipboard` module owns the freedesktop wire format and collision-free naming, unit-tested with `cargo test`. `ops.rs` grows a directory walk and a copy that reuses the progress plumbing the existing move already feeds. The GTK side is thin: three key bindings, three factory messages, and the panel doing the work it already does for trash.

**Tech Stack:** Rust 2021, relm4 0.9, gtk4-rs 0.9.6, gdk4 0.9.6, gio 0.20.12, libadwaita, libpanel.

## Global Constraints

- Design spec: `docs/design/2026-07-28-clipboard-copy-paste.md`. It is authoritative; copy its values verbatim.
- Comments and identifiers in English. Rustfmt defaults. UI strings in English, matching the app's existing toasts.
- `cargo build` takes about 100 seconds here. Every cargo command needs a 600000 ms timeout or it fails for no reason.
- Build check for every task: no errors, and no warnings beyond this measured upstream baseline of four:

```
2 × warning: hiding a lifetime that's elided elsewhere is confusing
1 × warning: struct `BitsetIter` is never constructed
1 × warning: trait `BitsetExt` is never used
```

- Never run GUI tests in the operator's live session — always the nested headless sway harness from Task 3.
- Never `pkill -f` anything; kill by PID.
- Preserve existing behaviour: marks, search, sort, rename, trash, vim navigation, the tapering column layout, the cursor glow. Any regression there fails the task.
- **Never overwrite a file.** Every destination name goes through `clipboard::free_name` first. There is no code path in this feature that passes `FileCopyFlags::OVERWRITE`.

---

### Task 1: The wire format and collision-free naming

Pure string handling, no GTK. This is the part with the sharp edges, so it is the part with tests.

**Files:**
- Create: `src/clipboard.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `clipboard::ClipboardOp` (`Copy` | `Cut`, derives `Debug, Clone, Copy, PartialEq, Eq`); `clipboard::GNOME_MIME`; `clipboard::URI_LIST_MIME`; `clipboard::encode(ClipboardOp, &[String]) -> String`; `clipboard::decode(&str) -> Option<(ClipboardOp, Vec<String>)>`; `clipboard::encode_uri_list(&[String]) -> String`; `clipboard::decode_uri_list(&str) -> Vec<String>`; `clipboard::free_name(&str, impl Fn(&str) -> bool) -> String`.

- [ ] **Step 1: Write the failing tests**

Create `src/clipboard.rs` with only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn uris(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn encodes_a_copy_the_way_nautilus_reads_it() {
        let payload = encode(ClipboardOp::Copy, &uris(&["file:///a.txt", "file:///b.txt"]));
        assert_eq!(payload, "copy\nfile:///a.txt\nfile:///b.txt");
    }

    #[test]
    fn the_operation_word_is_the_only_difference_for_a_cut() {
        let payload = encode(ClipboardOp::Cut, &uris(&["file:///a.txt"]));
        assert_eq!(payload, "cut\nfile:///a.txt");
    }

    #[test]
    fn decodes_what_it_encodes() {
        let files = uris(&["file:///a.txt", "file:///b.txt"]);
        for op in [ClipboardOp::Copy, ClipboardOp::Cut] {
            let round_trip = decode(&encode(op, &files));
            assert_eq!(round_trip, Some((op, files.clone())));
        }
    }

    #[test]
    fn tolerates_the_trailing_newline_other_apps_add() {
        assert_eq!(
            decode("copy\nfile:///a.txt\n"),
            Some((ClipboardOp::Copy, uris(&["file:///a.txt"])))
        );
    }

    #[test]
    fn rejects_a_payload_that_is_not_a_file_clipboard() {
        // Plain text on the clipboard must never be mistaken for files.
        assert_eq!(decode("just some copied text"), None);
        assert_eq!(decode(""), None);
        // An operation word with nothing to operate on is not actionable.
        assert_eq!(decode("copy"), None);
    }

    #[test]
    fn a_uri_list_is_crlf_terminated() {
        assert_eq!(
            encode_uri_list(&uris(&["file:///a.txt", "file:///b.txt"])),
            "file:///a.txt\r\nfile:///b.txt\r\n"
        );
    }

    #[test]
    fn a_uri_list_drops_its_comment_lines() {
        // Comments starting with '#' are part of RFC 2483, and some apps send them.
        let parsed = decode_uri_list("# a comment\r\nfile:///a.txt\r\n\r\nfile:///b.txt\r\n");
        assert_eq!(parsed, uris(&["file:///a.txt", "file:///b.txt"]));
    }

    #[test]
    fn a_free_name_is_left_alone() {
        assert_eq!(free_name("notas.txt", |_| false), "notas.txt");
    }

    #[test]
    fn a_taken_name_gets_the_copy_suffix_before_its_extension() {
        assert_eq!(free_name("notas.txt", |n| n == "notas.txt"), "notas (copy).txt");
    }

    #[test]
    fn the_suffix_counts_up_while_names_stay_taken() {
        let taken = |n: &str| matches!(n, "notas.txt" | "notas (copy).txt");
        assert_eq!(free_name("notas.txt", taken), "notas (copy 2).txt");
    }

    #[test]
    fn a_name_without_an_extension_keeps_the_suffix_at_the_end() {
        assert_eq!(free_name("README", |n| n == "README"), "README (copy)");
    }

    #[test]
    fn a_dotfiles_leading_dot_is_not_an_extension() {
        assert_eq!(free_name(".bashrc", |n| n == ".bashrc"), ".bashrc (copy)");
    }

    #[test]
    fn only_the_final_suffix_counts_as_the_extension() {
        assert_eq!(
            free_name("archive.tar.gz", |n| n == "archive.tar.gz"),
            "archive.tar (copy).gz"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib clipboard 2>&1 | tail -20`
Expected: compilation errors — `cannot find function encode in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `src/clipboard.rs`, above the test module:

```rust
//! The freedesktop file-clipboard wire format, and collision-free naming.
//!
//! Nautilus, Thunar, PCManFM and Caja all speak `x-special/gnome-copied-files`:
//! an operation word on the first line, then one URI per line. Everything here
//! is plain string handling, so it is testable without a display.

/// What a clipboard payload asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardOp {
    Copy,
    Cut,
}

/// The mime type that carries the operation word alongside the URIs.
pub const GNOME_MIME: &str = "x-special/gnome-copied-files";

/// The mime type nearly everything else understands. It has no operation word,
/// so a paste sourced from it is always treated as a copy.
pub const URI_LIST_MIME: &str = "text/uri-list";

/// Builds an `x-special/gnome-copied-files` payload.
pub fn encode(op: ClipboardOp, uris: &[String]) -> String {
    let word = match op {
        ClipboardOp::Copy => "copy",
        ClipboardOp::Cut => "cut",
    };

    std::iter::once(word.to_owned())
        .chain(uris.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parses an `x-special/gnome-copied-files` payload.
///
/// Returns `None` when the operation word is missing or unknown, or when no URI
/// follows it — which is also how plain text on the clipboard gets rejected
/// instead of being mistaken for a file list.
pub fn decode(payload: &str) -> Option<(ClipboardOp, Vec<String>)> {
    let mut lines = payload
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty());

    let op = match lines.next()? {
        "copy" => ClipboardOp::Copy,
        "cut" => ClipboardOp::Cut,
        _ => return None,
    };

    let uris: Vec<String> = lines.map(str::to_owned).collect();
    (!uris.is_empty()).then_some((op, uris))
}

/// Builds a `text/uri-list` payload: CRLF separated with a trailing CRLF, per
/// RFC 2483.
pub fn encode_uri_list(uris: &[String]) -> String {
    uris.iter().map(|uri| format!("{uri}\r\n")).collect()
}

/// Parses a `text/uri-list`. Comment lines starting with `#` are part of the
/// format and are dropped.
pub fn decode_uri_list(payload: &str) -> Vec<String> {
    payload
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// Returns a name `taken` rejects, derived from `name`.
///
/// `notas.txt` becomes `notas (copy).txt`, then `notas (copy 2).txt`. The
/// suffix goes before the final extension so the file keeps its type. Nothing
/// in this feature ever overwrites; this is how that promise is kept.
pub fn free_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }

    let (stem, extension) = split_extension(name);
    let mut attempt = 1usize;

    loop {
        let suffix = if attempt == 1 {
            "(copy)".to_owned()
        } else {
            format!("(copy {attempt})")
        };

        let candidate = if extension.is_empty() {
            format!("{stem} {suffix}")
        } else {
            format!("{stem} {suffix}.{extension}")
        };

        if !taken(&candidate) {
            return candidate;
        }

        attempt += 1;
    }
}

/// Splits a file name into stem and extension. A leading dot belongs to the
/// stem, so `.bashrc` has no extension, and only the final suffix counts, so
/// `archive.tar.gz` splits into `archive.tar` and `gz`.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(index) => (&name[..index], &name[index + 1..]),
    }
}
```

- [ ] **Step 4: Register the module**

In `src/lib.rs`, alongside the existing declarations:

```rust
pub mod clipboard;
```

`pub mod`, not `mod`: nothing consumes it until Task 3, and a private module makes its public items unreachable from the crate root, which floods the build with dead-code warnings.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib clipboard 2>&1 | tail -5`
Expected: `test result: ok. 13 passed`.

- [ ] **Step 6: Commit**

```bash
git add src/clipboard.rs src/lib.rs
git commit -m "add the freedesktop file-clipboard format and safe naming"
```

---

### Task 2: Walking and copying a tree

`gio` refuses to copy a directory — `copy` fails with `WOULD_RECURSE` — so a tree is walked first, then copied entry by entry.

**Files:**
- Modify: `src/ops.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `ops::Node { pub source: gio::File, pub relative: std::path::PathBuf, pub is_dir: bool, pub size: i64 }`; `ops::walk(root: &gio::File) -> Result<Vec<Node>, glib::Error>` (async); `ops::copy_tree(source: gio::File, destination: gio::File, sender: relm4::Sender<AppMsg>)` (async).

- [ ] **Step 1: Write the failing test**

Append to `src/ops.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Builds a small tree under a unique directory and returns its path.
    /// `/tmp` is fine here: copying works on tmpfs, only trashing does not.
    fn fixture(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("fm-ops-test-{name}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("top.txt"), b"top").unwrap();
        fs::write(root.join("sub/middle.txt"), b"middle").unwrap();
        fs::write(root.join("sub/deeper/bottom.txt"), b"bottom!").unwrap();
        root
    }

    fn relatives(nodes: &[Node]) -> Vec<String> {
        let mut out: Vec<String> = nodes
            .iter()
            .map(|node| node.relative.to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn walking_a_tree_finds_every_file_and_directory() {
        let root = fixture("walk");
        let nodes = glib::MainContext::default()
            .block_on(walk(&gio::File::for_path(&root)))
            .expect("the fixture is readable");

        assert_eq!(
            relatives(&nodes),
            vec![
                "empty",
                "sub",
                "sub/deeper",
                "sub/deeper/bottom.txt",
                "sub/middle.txt",
                "top.txt",
            ]
        );

        // Empty directories are nodes too, or they would vanish in the copy.
        let empty = nodes.iter().find(|n| n.relative.ends_with("empty")).unwrap();
        assert!(empty.is_dir);

        let bottom = nodes
            .iter()
            .find(|n| n.relative.ends_with("bottom.txt"))
            .unwrap();
        assert!(!bottom.is_dir);
        assert_eq!(bottom.size, 7);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn walking_a_plain_file_yields_that_file_alone() {
        let root = fixture("walk-file");
        let nodes = glib::MainContext::default()
            .block_on(walk(&gio::File::for_path(root.join("top.txt"))))
            .expect("the fixture is readable");

        assert_eq!(nodes.len(), 1);
        assert!(!nodes[0].is_dir);
        assert_eq!(nodes[0].relative, std::path::Path::new(""));

        let _ = fs::remove_dir_all(&root);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib ops 2>&1 | tail -20`
Expected: compilation errors — `cannot find function walk in this scope`.

- [ ] **Step 3: Implement the walk**

Add to `src/ops.rs`, above the test module:

```rust
use std::path::PathBuf;

/// One entry found under a copy source, and where it lands relative to the
/// destination root. A plain file yields a single node with an empty relative
/// path.
#[derive(Debug)]
pub struct Node {
    pub source: gio::File,
    pub relative: PathBuf,
    pub is_dir: bool,
    pub size: i64,
}

/// Lists everything under `root`, directories included, breadth first.
///
/// Directories are listed in their own right rather than implied by the files
/// inside them: an empty directory has no files to imply it and would otherwise
/// be dropped from the copy.
///
/// Symlinks are reported as the links they are, never followed — following them
/// duplicates whole trees silently and can loop forever.
pub async fn walk(root: &gio::File) -> Result<Vec<Node>, glib::Error> {
    const ATTRIBUTES: &str = "standard::name,standard::type,standard::size";

    let info = root
        .query_info_future(
            ATTRIBUTES,
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::DEFAULT,
        )
        .await?;

    if info.file_type() != gio::FileType::Directory {
        return Ok(vec![Node {
            source: root.clone(),
            relative: PathBuf::new(),
            is_dir: false,
            size: info.size(),
        }]);
    }

    let mut nodes = Vec::new();
    let mut pending = vec![(root.clone(), PathBuf::new())];

    while let Some((directory, prefix)) = pending.pop() {
        let enumerator = directory
            .enumerate_children_future(
                ATTRIBUTES,
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                glib::Priority::DEFAULT,
            )
            .await?;

        loop {
            let batch = enumerator
                .next_files_future(32, glib::Priority::DEFAULT)
                .await?;

            if batch.is_empty() {
                break;
            }

            for info in batch {
                let name = info.name();
                let child = directory.child(&name);
                let relative = prefix.join(&name);
                let is_dir = info.file_type() == gio::FileType::Directory;

                nodes.push(Node {
                    source: child.clone(),
                    relative: relative.clone(),
                    is_dir,
                    size: info.size(),
                });

                if is_dir {
                    pending.push((child, relative));
                }
            }
        }
    }

    Ok(nodes)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib ops 2>&1 | tail -5`
Expected: `test result: ok. 2 passed`.

If `next_files_future` does not resolve, check the enumerator API in
`~/.cargo/registry/src/index.crates.io-*/gio-0.20.12/src/file_enumerator.rs`
and use the async method it actually exposes; the surrounding logic is unchanged.

- [ ] **Step 5: Implement the copy**

Add to `src/ops.rs`, below `walk`:

```rust
/// Copies `source` — a file or a whole tree — to `destination`, reporting
/// progress through the same transfer UI the drag-and-drop move feeds.
///
/// `destination` is the final path, collision-free name already applied by the
/// caller. Nothing here overwrites: `FileCopyFlags::NONE` fails rather than
/// clobber, which is the correct outcome if a name slipped through.
pub async fn copy_tree(source: gio::File, destination: gio::File, sender: Sender<AppMsg>) {
    info!("copying {} to {}", source.uri(), destination.uri());

    let nodes = match walk(&source).await {
        Ok(nodes) => nodes,
        Err(err) => {
            let _ = sender.send(AppMsg::Error(Box::new(err)));
            return;
        }
    };

    let id = ID.fetch_add(1, Ordering::SeqCst);
    let total: i64 = nodes.iter().filter(|n| !n.is_dir).map(|n| n.size).sum();
    let description = format!(
        "Copying '{}'",
        source
            .basename()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_owned()),
    );

    let _ = sender.send(AppMsg::Transfer(Transfer::New { id, description }));

    // A single file walks to one node with an empty relative path, so the same
    // loop handles both shapes.
    let mut done: i64 = 0;

    for node in &nodes {
        let target = if node.relative.as_os_str().is_empty() {
            destination.clone()
        } else {
            destination.child(&node.relative)
        };

        let result = if node.is_dir {
            target
                .make_directory_future(glib::Priority::DEFAULT)
                .await
                .or_else(|err| {
                    // A directory created earlier in this same walk is not an error.
                    if err.matches(gio::IOErrorEnum::Exists) {
                        Ok(())
                    } else {
                        Err(err)
                    }
                })
        } else {
            let (fut, _progress) = node.source.copy_future(
                &target,
                gio::FileCopyFlags::NOFOLLOW_SYMLINKS,
                glib::Priority::DEFAULT,
            );
            fut.await
        };

        if let Err(err) = result {
            let _ = sender.send(AppMsg::Error(Box::new(err)));
            continue;
        }

        if !node.is_dir {
            done += node.size;
            let _ = sender.send(AppMsg::Transfer(Transfer::Progress(Progress {
                id,
                current: done,
                total,
            })));
        }
    }

    // An empty or all-directory copy never reported progress; close the
    // transfer so its spinner does not hang around.
    let _ = sender.send(AppMsg::Transfer(Transfer::Progress(Progress {
        id,
        current: total,
        total,
    })));
}
```

The walk yields parents before children because `pending` is seeded with the
root and each directory pushes its own children, so a directory is always
created before anything inside it.

- [ ] **Step 6: Build**

Run: `cargo build 2>&1 | grep -E "^error" -A 5`
Expected: no output.

- [ ] **Step 7: Commit**

```bash
git add src/ops.rs
git commit -m "walk and copy a directory tree with progress"
```

---

### Task 3: Ctrl+C and Ctrl+X write the system clipboard

**Files:**
- Modify: `src/component/app.rs`
- Modify: `src/component/directory_list.rs`
- Create: `/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad/harness.sh` if it is missing

**Interfaces:**
- Consumes: `clipboard::{ClipboardOp, GNOME_MIME, URI_LIST_MIME, encode, encode_uri_list}` from Task 1.
- Produces: `AppMsg::ClipboardCopy(clipboard::ClipboardOp)`; `DirectoryMessage::ClipboardCopy(clipboard::ClipboardOp)`.

- [ ] **Step 1: Build the headless harness if it is gone**

Write this to the scratchpad path above, `chmod +x` it, and never commit it. If it
already exists, skip to Step 2.

```bash
#!/usr/bin/env bash
# Runs fm in a nested headless sway and screenshots it.
# Usage:  harness.sh <shot-name> [typed-keys]
# Env:    RES=1600x900          starting output resolution
#         START_DIR=<path>      directory fm opens
#         KEYS_ARGS="-k space"  raw wtype arguments, for named keys
set -u
SCRATCH="$(dirname "$0")"
SHOT="${1:?shot name}"
KEYS="${2:-}"
RES="${RES:-1600x900}"
START_DIR="${START_DIR:-$HOME/.local/src/fm}"

printf 'output HEADLESS-1 resolution %s\n' "$RES" > "$SCRATCH/sway-probe.conf"
ls "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | sort > "$SCRATCH/before.txt"

WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
WLR_RENDER_DRM_DEVICE=/dev/dri/renderD128 \
  sway -c "$SCRATCH/sway-probe.conf" > "$SCRATCH/sway.log" 2>&1 &
SWAY_PID=$!

for _ in $(seq 1 40); do
  ls "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | sort > "$SCRATCH/after.txt"
  comm -13 "$SCRATCH/before.txt" "$SCRATCH/after.txt" | grep -qv '\.lock$' && break
  sleep 0.25
done
DISPLAY_NAME=$(basename "$(comm -13 "$SCRATCH/before.txt" "$SCRATCH/after.txt" | grep -v '\.lock$' | head -1)")

# Without this guard an empty display name lets GTK fall back and open the
# window in the operator's live session.
if [ -z "$DISPLAY_NAME" ]; then
  echo "FATAL: no nested wayland socket appeared; not launching fm"
  kill "$SWAY_PID" 2>/dev/null
  exit 1
fi
echo "display: $DISPLAY_NAME"

# Prime the nested clipboard from OUTSIDE fm, so the read path is exercised the
# way it would be from Nautilus rather than from fm's own write path.
if [ -n "${SEED_CLIPBOARD:-}" ]; then
  printf '%s' "$SEED_CLIPBOARD" |
    WAYLAND_DISPLAY="$DISPLAY_NAME" wl-copy --type x-special/gnome-copied-files
fi

RUST_BACKTRACE=1 env -u DISPLAY GDK_BACKEND=wayland WAYLAND_DISPLAY="$DISPLAY_NAME" \
  dbus-run-session -- ./target/debug/fm "$START_DIR" > "$SCRATCH/fm.log" 2>&1 &
FM_PID=$!

for _ in $(seq 1 40); do
  WAYLAND_DISPLAY="$DISPLAY_NAME" grim "$SCRATCH/$SHOT.png" 2>/dev/null &&
    [ "$(stat -c%s "$SCRATCH/$SHOT.png")" -gt 20000 ] && break
  sleep 0.5
done

if [ -n "$KEYS" ]; then
  WAYLAND_DISPLAY="$DISPLAY_NAME" wtype -s 600 -d 250 "$KEYS"
  sleep 1.5
  WAYLAND_DISPLAY="$DISPLAY_NAME" grim "$SCRATCH/$SHOT.png"
fi

# Deliberately unquoted: this carries wtype flags such as `-k space -k Delete`.
# One wtype invocation per sequence — separate calls race and drop keys.
if [ -n "${KEYS_ARGS:-}" ]; then
  WAYLAND_DISPLAY="$DISPLAY_NAME" wtype -s 600 -d 300 $KEYS_ARGS
  sleep 2.5
  WAYLAND_DISPLAY="$DISPLAY_NAME" grim "$SCRATCH/$SHOT.png"
fi

# Let the caller inspect the clipboard before the compositor dies with it.
if [ -n "${AFTER_CMD:-}" ]; then
  WAYLAND_DISPLAY="$DISPLAY_NAME" bash -c "$AFTER_CMD"
fi

grep -i "panicked" "$SCRATCH/fm.log" && echo "PANIC DETECTED" || echo "no panics"

kill "$FM_PID" 2>/dev/null
kill "$SWAY_PID" 2>/dev/null
for _ in $(seq 1 20); do
  [ -e "$XDG_RUNTIME_DIR/$DISPLAY_NAME" ] || break
  sleep 0.25
done
```

- [ ] **Step 2: Add the panel message**

In `src/component/directory_list.rs`, add to the `DirectoryMessage` enum:

```rust
    /// Put this panel's operation set on the system clipboard.
    ClipboardCopy(crate::clipboard::ClipboardOp),
```

- [ ] **Step 3: Handle it**

Add this arm to that file's `update_with_view` match:

```rust
            DirectoryMessage::ClipboardCopy(op) => {
                let uris: Vec<String> = self
                    .selected_file_info()
                    .iter()
                    .filter_map(|info| info.file().map(|file| file.uri().to_string()))
                    .collect();

                if uris.is_empty() {
                    sender.output(AppMsg::Toast("Nothing to copy".to_owned())).unwrap();
                    return;
                }

                let count = uris.len();

                // Both types at once: the GNOME one carries copy-vs-cut, the
                // uri-list is what file pickers and upload dialogs understand.
                let gnome = gdk::ContentProvider::for_bytes(
                    crate::clipboard::GNOME_MIME,
                    &glib::Bytes::from_owned(crate::clipboard::encode(op, &uris).into_bytes()),
                );
                let uri_list = gdk::ContentProvider::for_bytes(
                    crate::clipboard::URI_LIST_MIME,
                    &glib::Bytes::from_owned(
                        crate::clipboard::encode_uri_list(&uris).into_bytes(),
                    ),
                );

                widgets
                    .root
                    .clipboard()
                    .set_content(Some(&gdk::ContentProvider::new_union(&[gnome, uri_list])))
                    .unwrap_or_else(|err| warn!("unable to set the clipboard: {}", err));

                let verb = match op {
                    crate::clipboard::ClipboardOp::Copy => "copied",
                    crate::clipboard::ClipboardOp::Cut => "cut",
                };
                sender
                    .output(AppMsg::Toast(match count {
                        1 => format!("1 file {verb}"),
                        n => format!("{n} files {verb}"),
                    }))
                    .unwrap();
            }
```

- [ ] **Step 4: Add the app message**

In `src/component/app.rs`, add to `AppMsg`:

```rust
    /// Put the cursor panel's operation set on the clipboard (`Ctrl+C`, `Ctrl+X`).
    ClipboardCopy(crate::clipboard::ClipboardOp),
```

And the arm in `update_with_view`, beside `AppMsg::TrashSelected`:

```rust
            AppMsg::ClipboardCopy(op) => {
                if let Some(idx) = self.cursor_panel() {
                    self.directories
                        .send(idx, DirectoryMessage::ClipboardCopy(op));
                }
            }
```

- [ ] **Step 5: Bind the keys**

In `src/component/app.rs`, immediately **before** the existing bail:

```rust
            if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }
```

insert:

```rust
            // Ctrl+C / Ctrl+X / Ctrl+V, ahead of the modifier bail below so the
            // rest of the accelerators (Ctrl+H for hidden files) still pass
            // through untouched. The focus guard above already ran, so these
            // never fire while a text entry has the caret.
            if state.contains(gdk::ModifierType::CONTROL_MASK)
                && !state.contains(gdk::ModifierType::ALT_MASK)
            {
                match keyval {
                    gdk::Key::c | gdk::Key::C => {
                        key_sender.input(AppMsg::ClipboardCopy(crate::clipboard::ClipboardOp::Copy));
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::x | gdk::Key::X => {
                        key_sender.input(AppMsg::ClipboardCopy(crate::clipboard::ClipboardOp::Cut));
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
```

`Ctrl+V` joins this match in Task 4.

- [ ] **Step 6: Build**

Run: `cargo build 2>&1 | grep -E "^error" -A 5`
Expected: no output.

- [ ] **Step 7: Verify the clipboard really carries the files**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
mkdir -p $S/copy-src && cd $S/copy-src && rm -rf ./* 
for f in 05-e 04-d 03-c 02-b 01-a; do echo "$f" > $f.txt; sleep 0.15; done
cd ~/.local/src/fm
AFTER_CMD='echo "--- GNOME ---"; wl-paste --type x-special/gnome-copied-files; echo "--- URI LIST ---"; wl-paste --type text/uri-list' \
KEYS_ARGS="-k space -k space -M ctrl -k c -m ctrl" START_DIR=$S/copy-src $S/harness.sh copy-check
```

The listing sorts newest first, so the fixture is created in reverse and rows
read `01-a.txt`, `02-b.txt`, … Two spaces mark the first two rows and leave the
cursor on the third.

Expected output: the GNOME payload is exactly

```
copy
file:///.../copy-src/01-a.txt
file:///.../copy-src/02-b.txt
```

and the uri-list holds the same two URIs. **`03-c.txt` must not appear** — marks
win over the cursor, the same rule the trash operation follows. Also expected:
`no panics`, and a screenshot whose toast reads "2 files copied".

- [ ] **Step 8: Verify Ctrl+X differs only in the operation word**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
AFTER_CMD='wl-paste --type x-special/gnome-copied-files' \
KEYS_ARGS="-k space -M ctrl -k x -m ctrl" START_DIR=$S/copy-src $S/harness.sh cut-check
```

Expected: first line `cut`, one URI, toast "1 file cut", no files moved yet —
cutting only records the intent.

- [ ] **Step 9: Commit**

```bash
git add src/component/app.rs src/component/directory_list.rs
git commit -m "put marked entries on the system clipboard"
```

---

### Task 4: Ctrl+V pastes

**Files:**
- Modify: `src/component/app.rs`
- Modify: `src/component/directory_list.rs`

**Interfaces:**
- Consumes: `clipboard::{decode, decode_uri_list, free_name, ClipboardOp, GNOME_MIME, URI_LIST_MIME}` from Task 1; `ops::copy_tree` from Task 2; `AppMsg::ClipboardCopy` from Task 3.
- Produces: `AppMsg::ClipboardPaste`; `DirectoryMessage::ClipboardPaste`.

- [ ] **Step 1: Add the messages and the key**

In `src/component/directory_list.rs`, add to `DirectoryMessage`:

```rust
    /// Paste the clipboard into this panel's directory.
    ClipboardPaste,
```

In `src/component/app.rs`, add to `AppMsg`:

```rust
    /// Paste the clipboard into the cursor panel's directory (`Ctrl+V`).
    ClipboardPaste,
```

its arm in `update_with_view`:

```rust
            AppMsg::ClipboardPaste => {
                if let Some(idx) = self.cursor_panel() {
                    self.directories.send(idx, DirectoryMessage::ClipboardPaste);
                }
            }
```

and a third arm in the Ctrl match added in Task 3, beside `c` and `x`:

```rust
                    gdk::Key::v | gdk::Key::V => {
                        key_sender.input(AppMsg::ClipboardPaste);
                        return glib::Propagation::Stop;
                    }
```

- [ ] **Step 2: Implement the paste**

Add this arm to `update_with_view` in `src/component/directory_list.rs`:

```rust
            DirectoryMessage::ClipboardPaste => {
                let destination_dir = self.dir();
                let clipboard = widgets.root.clipboard();
                let sender = sender.clone();

                relm4::spawn_local(async move {
                    let Some((op, uris)) = read_file_clipboard(&clipboard).await else {
                        sender
                            .output(AppMsg::Toast("Nothing to paste".to_owned()))
                            .unwrap();
                        return;
                    };

                    let mut pasted = 0usize;
                    let mut skipped = 0usize;

                    for uri in uris {
                        let source = gio::File::for_uri(&uri);

                        let Some(name) = source.basename() else {
                            skipped += 1;
                            continue;
                        };

                        // Never paste a directory into itself or below itself:
                        // the walk would feed itself and fill the disk.
                        if destination_dir.equal(&source)
                            || destination_dir.has_prefix(&source)
                        {
                            skipped += 1;
                            continue;
                        }

                        // A cut landing where it already lives is a no-op, not
                        // an error.
                        if op == crate::clipboard::ClipboardOp::Cut
                            && source.parent().as_ref() == Some(&destination_dir)
                        {
                            skipped += 1;
                            continue;
                        }

                        let name = name.to_string_lossy().into_owned();
                        let target_name = crate::clipboard::free_name(&name, |candidate| {
                            destination_dir.child(candidate).query_exists(gio::Cancellable::NONE)
                        });
                        let target = destination_dir.child(&target_name);

                        match op {
                            crate::clipboard::ClipboardOp::Copy => {
                                ops::copy_tree(source, target, sender.output_sender().clone())
                                    .await;
                            }
                            crate::clipboard::ClipboardOp::Cut => {
                                ops::move_(source, target, sender.output_sender().clone()).await;
                            }
                        }

                        pasted += 1;
                    }

                    // A cut clipboard is spent: its sources have moved, and a
                    // second paste would fail on every one of them.
                    if op == crate::clipboard::ClipboardOp::Cut && pasted > 0 {
                        clipboard.set_content(None::<&gdk::ContentProvider>).ok();
                    }

                    let message = match (pasted, skipped) {
                        (0, 0) => "Nothing to paste".to_owned(),
                        (0, n) => format!("{n} skipped"),
                        (1, 0) => "1 file pasted".to_owned(),
                        (n, 0) => format!("{n} files pasted"),
                        (n, s) => format!("{n} pasted, {s} skipped"),
                    };
                    sender.output(AppMsg::Toast(message)).unwrap();
                });
            }
```

- [ ] **Step 3: Implement the clipboard read**

Add this free function near the other helpers at the bottom of
`src/component/directory_list.rs`:

```rust
/// Reads a file list off the clipboard.
///
/// The GNOME type is preferred because it carries the operation word. A bare
/// `text/uri-list` — what a browser or a file picker offers — has no such word,
/// so it is read as a copy: assuming a cut would delete another app's files.
async fn read_file_clipboard(
    clipboard: &gdk::Clipboard,
) -> Option<(crate::clipboard::ClipboardOp, Vec<String>)> {
    if let Ok((stream, _)) = clipboard
        .read_future(&[crate::clipboard::GNOME_MIME], glib::Priority::DEFAULT)
        .await
    {
        if let Some(payload) = read_stream_to_string(stream).await {
            if let Some(parsed) = crate::clipboard::decode(&payload) {
                return Some(parsed);
            }
        }
    }

    let (stream, _) = clipboard
        .read_future(&[crate::clipboard::URI_LIST_MIME], glib::Priority::DEFAULT)
        .await
        .ok()?;
    let payload = read_stream_to_string(stream).await?;
    let uris = crate::clipboard::decode_uri_list(&payload);

    (!uris.is_empty()).then_some((crate::clipboard::ClipboardOp::Copy, uris))
}

/// Drains a clipboard stream into a string, discarding anything that is not
/// valid UTF-8 — a clipboard payload that is not text is not a file list.
async fn read_stream_to_string(stream: gio::InputStream) -> Option<String> {
    let mut collected: Vec<u8> = Vec::new();

    loop {
        let buffer = vec![0u8; 4096];
        let (buffer, read) = stream
            .read_future(buffer, glib::Priority::DEFAULT)
            .await
            .ok()?;

        if read == 0 {
            break;
        }

        collected.extend_from_slice(&buffer[..read]);
    }

    String::from_utf8(collected).ok()
}
```

- [ ] **Step 4: Build**

Run: `cargo build 2>&1 | grep -E "^error" -A 5`
Expected: no output.

If `read_future` on `gio::InputStream` does not resolve with that signature,
check `~/.cargo/registry/src/index.crates.io-*/gio-0.20.12/src/input_stream.rs`
and adapt the call; the surrounding logic is unchanged.

- [ ] **Step 5: Verify a copy-paste end to end**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
rm -rf $S/paste-dest && mkdir -p $S/paste-dest
echo "=== SOURCE BEFORE ==="; ls -1 $S/copy-src
AFTER_CMD="ls -1 $S/paste-dest" \
KEYS_ARGS="-k space -k space -M ctrl -k c -m ctrl" START_DIR=$S/copy-src $S/harness.sh paste-step1
```

Each harness run tears its compositor down, and the clipboard dies with it. So
the paste runs in its own session with the clipboard seeded by `wl-copy` —
which is the stronger test anyway: the payload is written by another program,
exactly as Nautilus would.

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
SEED_CLIPBOARD="copy
file://$S/copy-src/01-a.txt
file://$S/copy-src/02-b.txt" \
AFTER_CMD="ls -1 $S/paste-dest" \
KEYS_ARGS="-M ctrl -k v -m ctrl" START_DIR=$S/paste-dest $S/harness.sh paste-step2
```

Expected: `paste-dest` lists `01-a.txt` and `02-b.txt`, `copy-src` still holds
all five, the toast reads "2 files pasted", and `no panics`. This is spec
verification item 8 — interop — settled without needing a running Nautilus.

- [ ] **Step 6: Verify the collision suffix**

Run the same paste twice more into the same destination:

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
for round in 1 2; do
  SEED_CLIPBOARD="copy
file://$S/copy-src/01-a.txt" \
  AFTER_CMD="ls -1 $S/paste-dest" \
  KEYS_ARGS="-M ctrl -k v -m ctrl" START_DIR=$S/paste-dest $S/harness.sh collide-$round
done
```

Expected after both rounds: `01-a.txt`, `01-a (copy).txt`, `01-a (copy 2).txt`,
with the original's contents intact in all three.

- [ ] **Step 7: Commit**

```bash
git add src/component/app.rs src/component/directory_list.rs
git commit -m "paste the clipboard into the cursor panel's directory"
```

---

### Task 5: The guards and the rest of the spec's checklist

Verification, plus fixes if a check fails.

**Files:** none expected. Fixes land where the failure is.

**Interfaces:**
- Consumes: everything from Tasks 1–4.
- Produces: nothing.

- [ ] **Step 1: A directory is never pasted inside itself**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
rm -rf $S/self-paste && mkdir -p $S/self-paste/inner/deeper
echo hi > $S/self-paste/inner/file.txt
SEED_CLIPBOARD="copy
file://$S/self-paste/inner" \
AFTER_CMD="find $S/self-paste | head -20; echo '--- count ---'; find $S/self-paste | wc -l" \
KEYS_ARGS="-M ctrl -k v -m ctrl" START_DIR=$S/self-paste/inner/deeper $S/harness.sh self-paste
```

Expected: the tree is **unchanged** (5 entries), the toast says something was
skipped, and `no panics`. A growing tree or a hang here is the worst outcome in
this feature — it fills the disk. If it happens, the guard in the paste arm is
wrong; fix it before anything else.

- [ ] **Step 2: A cut moves rather than copies, and spends the clipboard**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
rm -rf $S/cut-src $S/cut-dest && mkdir -p $S/cut-src $S/cut-dest
echo moved > $S/cut-src/movable.txt
SEED_CLIPBOARD="cut
file://$S/cut-src/movable.txt" \
AFTER_CMD="echo SRC:; ls -1 $S/cut-src; echo DEST:; ls -1 $S/cut-dest; echo CLIP:; wl-paste --type x-special/gnome-copied-files || echo '(empty)'" \
KEYS_ARGS="-M ctrl -k v -m ctrl" START_DIR=$S/cut-dest $S/harness.sh cut-paste
```

Expected: `cut-src` empty, `cut-dest` holds `movable.txt`, clipboard reports
empty, `no panics`.

- [ ] **Step 3: A nested tree survives the round trip**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
rm -rf $S/tree-src $S/tree-dest && mkdir -p $S/tree-src/a/b/c $S/tree-src/empty $S/tree-dest
echo one > $S/tree-src/top.txt
echo two > $S/tree-src/a/mid.txt
echo three > $S/tree-src/a/b/c/bottom.txt
SEED_CLIPBOARD="copy
file://$S/tree-src" \
AFTER_CMD="find $S/tree-dest | sort" \
KEYS_ARGS="-M ctrl -k v -m ctrl" START_DIR=$S/tree-dest $S/harness.sh tree-copy
```

Expected: the destination mirrors the source exactly, **including the empty
directory**, and the three files carry their contents. Verify with
`diff -r $S/tree-src $S/tree-dest/tree-src`.

- [ ] **Step 4: Ctrl+C in the search entry still copies text**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
AFTER_CMD='echo "CLIP TEXT:"; wl-paste --type text/plain; echo "CLIP FILES:"; wl-paste --type x-special/gnome-copied-files || echo "(no file payload — correct)"' \
KEYS_ARGS="-M ctrl -k a -m ctrl -M ctrl -k c -m ctrl" \
START_DIR=$S/copy-src $S/harness.sh entry-copy "/hello"
```

The positional `"/hello"` opens the search bar and types into it; `KEYS_ARGS`
then sends Ctrl+A and Ctrl+C while the caret is still in that entry.

Expected: the clipboard holds the text `hello`, and **no** file payload. The
focus guard in the key controller is what makes this true; if a file payload
appears, the Ctrl branch was inserted above the focus guard instead of below it.

- [ ] **Step 5: The empty cases speak**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
rm -rf $S/empty-dir && mkdir -p $S/empty-dir
SEED_CLIPBOARD="" AFTER_CMD="true" \
KEYS_ARGS="-M ctrl -k v -m ctrl" START_DIR=$S/empty-dir $S/harness.sh empty-paste
```

Read the screenshot: the toast must read "Nothing to paste". A keypress that
appears to do nothing is the thing that makes a user press it again.

- [ ] **Step 6: The primary flow still works**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
$S/harness.sh regression "jjjlkjhjjonjj"
```

Descends, moves the cursor, goes back up with `h`, re-sorts by name with `on`,
moves again. Expected: `no panics`, a coherent screenshot with the tapering
columns and the cyan cursor glow intact. A past regression in this fork shipped
because only the new feature was exercised and never a plain selection.

- [ ] **Step 7: Commit any fixes**

Only if Steps 1–6 forced a change:

```bash
git add -u
git commit -m "fix the clipboard guards"
```

---

### Task 6: Install and hand over

**Files:** none modified.

- [ ] **Step 1: Run the whole unit suite**

Run: `cargo test --lib 2>&1 | tail -3`
Expected: `test result: ok. 46 passed` — the 31 that existed before, plus 13 from
Task 1 and 2 from Task 2.

- [ ] **Step 2: Check for new warnings**

Run: `cargo build 2>&1 | grep -E "^warning" | sort | uniq -c`
Expected: exactly the four-line upstream baseline in Global Constraints.

- [ ] **Step 3: Clean up the fixtures**

```bash
S=/tmp/claude-1000/-home-milo/8a050480-dd93-4e10-a8d6-87498d4b9c1f/scratchpad
rm -rf $S/copy-src $S/paste-dest $S/self-paste $S/cut-src $S/cut-dest \
       $S/tree-src $S/tree-dest $S/empty-dir
```

- [ ] **Step 4: Push and install from the fork**

Install from the **fork**, never from the local path. `cargo install --path`
rewrites the registered source to a local directory, and `cargo install-update`
(topgrade's cargo step) updates from whatever source is registered — a path
install quietly ends the update-proofing the fork exists to provide.

```bash
git push origin HEAD:master
cargo install --git https://github.com/milojarow/fm fm --force
```

- [ ] **Step 5: Hand over**

Tell the operator to launch `fm` themselves — never open a GUI window into their
live session from this agent. Report what was verified, and state plainly that
cross-application interop was proven against `wl-copy`/`wl-paste` rather than
against a running Nautilus, since that is what the harness can reach.
