# Column Depth Feedback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the column holding the keyboard cursor unmistakable — ancestors taper to the left, the cursor's column stays centred and never moves, and the header shows the live path with ranger-style abbreviation.

**Architecture:** Two pure, GTK-free modules do the thinking (`layout` solves widths, `path_title` shortens paths) and are unit-tested with `cargo test`. The GTK side is thin glue: `app.rs` calls the solvers and sends one message per panel; each `Directory` applies its own width and stack page. No component reaches into another's widgets.

**Tech Stack:** Rust 2021, relm4 0.9, gtk4-rs, libadwaita, libpanel.

## Global Constraints

- Design spec: `docs/design/2026-07-25-column-depth-feedback.md`. Its constants and formulas are authoritative; copy values verbatim.
- Comments and identifiers in English. Rustfmt defaults.
- Never run GUI tests in the operator's live session — always the nested headless sway harness built in Task 3.
- Never `pkill -f` anything; kill by PID.
- `set_title` on the window stays `"fm"`. The operator's waybar `window-rewrite` rules match on window titles; only the header's title *widget* changes.
- Preserve existing behaviour: marks, search, sort, rename, trash, vim navigation. Any regression there fails the task.
- `cargo build` takes about 100 seconds here. Every cargo command needs a
  600000 ms timeout or it fails for no reason.
- Build check for every task: `cargo build` must produce no errors and no
  warnings beyond this measured baseline of four, all from upstream:

```
2 × warning: hiding a lifetime that's elided elsewhere is confusing
1 × warning: struct `BitsetIter` is never constructed
1 × warning: trait `BitsetExt` is never used
```

---

### Task 1: The geometry solver

Pure arithmetic, no GTK. This is the whole layout brain.

**Files:**
- Create: `src/layout.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `layout::solve(area_width: i32, panel_count: usize, cursor: usize) -> Option<layout::Layout>`; `layout::Layout { gutter: i32, panels: Vec<layout::PanelLayout> }`; `layout::PanelLayout { width: i32, sliver: bool, visible: bool }` (`Copy`).

- [ ] **Step 1: Write the failing tests**

Create `src/layout.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's worked example: 1600px window minus a 152px sidebar.
    const AREA: i32 = 1448;

    #[test]
    fn reproduces_the_worked_example() {
        let plan = solve(AREA, 7, 5).expect("a 1448px area is laid out");

        let widths: Vec<i32> = plan.panels.iter().map(|panel| panel.width).collect();
        assert_eq!(widths, vec![43, 62, 89, 127, 182, 434, 182]);
        assert_eq!(plan.gutter, 4);
    }

    #[test]
    fn centres_the_cursor_column() {
        for cursor in 0..6 {
            let plan = solve(AREA, 7, cursor).expect("laid out");
            let left: i32 = plan.panels[..cursor]
                .iter()
                .filter(|panel| panel.visible)
                .map(|panel| panel.width)
                .sum();
            let budget = (AREA - plan.panels[cursor].width) / 2;
            assert_eq!(
                plan.gutter + left,
                budget,
                "left side must equal the budget at depth {cursor}"
            );
        }
    }

    #[test]
    fn the_root_column_gets_the_whole_gutter() {
        let plan = solve(AREA, 3, 0).expect("laid out");
        assert_eq!(plan.gutter, (AREA - plan.panels[0].width) / 2);
    }

    #[test]
    fn ancestors_thin_out_towards_the_left() {
        let plan = solve(AREA, 7, 5).expect("laid out");
        let widths: Vec<i32> = plan.panels[..5].iter().map(|panel| panel.width).collect();
        for pair in widths.windows(2) {
            assert!(pair[0] < pair[1], "{:?} must increase rightwards", widths);
        }
    }

    #[test]
    fn narrow_ancestors_are_flagged_as_slivers() {
        let plan = solve(AREA, 7, 5).expect("laid out");
        let slivers: Vec<bool> = plan.panels[..5].iter().map(|panel| panel.sliver).collect();
        assert_eq!(slivers, vec![true, true, false, false, false]);
    }

    #[test]
    fn the_child_panel_mirrors_the_nearest_ancestor() {
        let plan = solve(AREA, 7, 5).expect("laid out");
        assert_eq!(plan.panels[6].width, plan.panels[4].width);
    }

    #[test]
    fn a_long_tail_of_right_panels_never_starves_the_preview() {
        let plan = solve(AREA, 12, 2).expect("laid out");
        let budget = (AREA - plan.panels[2].width) / 2;
        let right: i32 = plan.panels[3..]
            .iter()
            .filter(|panel| panel.visible)
            .map(|panel| panel.width)
            .sum();
        assert!(
            budget - right >= 200,
            "the preview kept {} of its 200px floor",
            budget - right
        );
    }

    #[test]
    fn very_deep_stacks_drop_their_oldest_columns() {
        let plan = solve(400, 40, 39).expect("laid out");
        assert!(
            plan.panels.iter().any(|panel| !panel.visible),
            "some ancestors must drop out of a 400px area"
        );
        let left: i32 = plan
            .panels
            .iter()
            .filter(|panel| panel.visible)
            .map(|panel| panel.width)
            .sum::<i32>()
            - plan.panels[39].width;
        assert!(left <= (400 - plan.panels[39].width) / 2);
    }

    #[test]
    fn a_window_too_narrow_to_lay_out_returns_none() {
        assert_eq!(solve(270, 3, 1), None);
    }

    #[test]
    fn nonsense_input_returns_none() {
        assert_eq!(solve(AREA, 0, 0), None);
        assert_eq!(solve(AREA, 3, 3), None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib layout 2>&1 | tail -20`
Expected: compilation errors — `cannot find function solve in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `src/layout.rs`, above the test module:

```rust
//! Pure geometry for the directory columns.
//!
//! The cursor's column keeps a fixed width and stays centred in the columns
//! area: the ancestors to its left and everything to its right each get the
//! same budget, so the centring falls out of the arithmetic rather than being
//! measured. Ancestors taper geometrically towards the left edge.

/// Fraction of the columns area given to the cursor's column.
const CURRENT_FRACTION: f64 = 0.30;

/// Bounds on the cursor column's width, in pixels.
const CURRENT_MIN: i32 = 260;
const CURRENT_MAX: i32 = 520;

/// Each ancestor is this fraction of the column to its right.
const TAPER_RATIO: f64 = 0.7;

/// A column narrower than this cannot show a listing and renders as a sliver.
const SLIVER_THRESHOLD: i32 = 72;

/// A sliver is never thinner than this.
const SLIVER_MIN: i32 = 12;

/// Width of the panel right of the cursor when there is no ancestor to mirror,
/// as a fraction of the side budget.
const NO_PARENT_CHILD_FRACTION: f64 = 0.45;

/// Width the preview keeps for itself, whatever the panels left of it want.
const PREVIEW_MIN: i32 = 200;

/// What one directory panel should look like after a relayout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelLayout {
    /// Requested width in pixels. Meaningless when `visible` is false.
    pub width: i32,
    /// Render the thin strip instead of the listing.
    pub sliver: bool,
    /// False for panels squeezed out of the budget entirely.
    pub visible: bool,
}

/// The plan for one relayout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Left margin holding the cursor column centred, and soaking up the
    /// pixels lost to flooring the ancestor widths.
    pub gutter: i32,
    /// One entry per directory panel, in panel order.
    pub panels: Vec<PanelLayout>,
}

/// Plans the widths of `panel_count` panels when the cursor sits on `cursor`.
///
/// Returns `None` when the area is too narrow to be worth laying out; the
/// caller should fall back to uniform columns and let the view scroll.
pub fn solve(area_width: i32, panel_count: usize, cursor: usize) -> Option<Layout> {
    if panel_count == 0 || cursor >= panel_count {
        return None;
    }

    let current =
        ((area_width as f64 * CURRENT_FRACTION) as i32).clamp(CURRENT_MIN, CURRENT_MAX);
    let budget = (area_width - current) / 2;
    if budget < SLIVER_MIN {
        return None;
    }

    let mut panels = vec![
        PanelLayout {
            width: current,
            sliver: false,
            visible: true,
        };
        panel_count
    ];

    // Left: the ancestors nearest the cursor keep their share, and the oldest
    // drop out once the budget can no longer hold them all.
    let mut kept = cursor;
    let ancestors = loop {
        let widths = taper(budget, kept);
        if widths.iter().sum::<i32>() <= budget || kept == 0 {
            break widths;
        }
        kept -= 1;
    };

    let dropped = cursor - kept;
    for panel in panels.iter_mut().take(dropped) {
        panel.visible = false;
    }
    for (offset, width) in ancestors.iter().enumerate() {
        panels[dropped + offset] = sized(*width);
    }

    // Right: the first panel mirrors the nearest ancestor and deeper ones keep
    // tapering, but they may never eat into the preview's floor.
    let mirror = ancestors
        .last()
        .copied()
        .unwrap_or((budget as f64 * NO_PARENT_CHILD_FRACTION) as i32);
    let right_budget = (budget - PREVIEW_MIN).max(0);
    let mut right = right_widths(mirror, panel_count - cursor - 1);
    while !right.is_empty() && right.iter().sum::<i32>() > right_budget {
        right.pop();
    }

    for (offset, width) in right.iter().enumerate() {
        panels[cursor + 1 + offset] = sized(*width);
    }
    for panel in panels.iter_mut().skip(cursor + 1 + right.len()) {
        panel.visible = false;
    }

    Some(Layout {
        gutter: budget - ancestors.iter().sum::<i32>(),
        panels,
    })
}

/// A visible panel, showing its listing or a sliver depending on how much room
/// it ended up with.
fn sized(width: i32) -> PanelLayout {
    PanelLayout {
        width,
        sliver: width < SLIVER_THRESHOLD,
        visible: true,
    }
}

/// Splits `budget` between `count` ancestors, tapering towards the left.
/// Index 0 is the leftmost and thinnest.
///
/// Widths are floored rather than rounded: rounding several ancestors up can
/// overshoot the budget by a pixel and evict a column for no reason. The
/// leftover pixels go to the gutter.
fn taper(budget: i32, count: usize) -> Vec<i32> {
    if count == 0 {
        return Vec::new();
    }

    let weights: Vec<f64> = (0..count)
        .map(|index| TAPER_RATIO.powi((count - 1 - index) as i32))
        .collect();
    let total: f64 = weights.iter().sum();

    weights
        .iter()
        .map(|weight| ((budget as f64 * weight / total) as i32).max(SLIVER_MIN))
        .collect()
}

/// Widths for the `count` panels right of the cursor: the first matches
/// `mirror`, the rest taper away from it.
fn right_widths(mirror: i32, count: usize) -> Vec<i32> {
    (0..count)
        .map(|step| ((mirror as f64 * TAPER_RATIO.powi(step as i32)) as i32).max(SLIVER_MIN))
        .collect()
}
```

- [ ] **Step 4: Register the module**

In `src/lib.rs`, add alongside the existing `mod` declarations:

```rust
mod layout;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib layout 2>&1 | tail -20`
Expected: `test result: ok. 10 passed`.

- [ ] **Step 6: Commit**

```bash
git add src/layout.rs src/lib.rs
git commit -m "add the tapering column geometry solver"
```

---

### Task 2: Path shortening for the title

Also pure — no GTK, no Pango. Width measurement is injected as a closure so the
ladder can be tested without a display.

**Files:**
- Create: `src/path_title.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `path_title::segments(path: &Path, home: Option<&Path>) -> Vec<String>`; `path_title::shorten(segments: &[String], fits: impl Fn(&str) -> bool) -> String`; `path_title::initial(name: &str) -> String`; `path_title::markup(path: &str) -> String`.

- [ ] **Step 1: Write the failing tests**

Create `src/path_title.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn owned(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    /// Stands in for Pango: accepts anything up to `limit` characters.
    fn width_limit(limit: usize) -> impl Fn(&str) -> bool {
        move |candidate: &str| candidate.chars().count() <= limit
    }

    #[test]
    fn replaces_the_home_prefix_with_a_tilde() {
        let segments = segments(
            Path::new("/home/milo/projects/fm"),
            Some(Path::new("/home/milo")),
        );
        assert_eq!(segments, owned(&["~", "projects", "fm"]));
    }

    #[test]
    fn keeps_the_root_separator_as_its_own_segment() {
        let segments = segments(Path::new("/etc/systemd"), Some(Path::new("/home/milo")));
        assert_eq!(segments, owned(&["/", "etc", "systemd"]));
    }

    #[test]
    fn leaves_a_path_that_already_fits_alone() {
        let segments = owned(&["~", "projects", "fm"]);
        assert_eq!(shorten(&segments, width_limit(80)), "~/projects/fm");
    }

    #[test]
    fn abbreviates_from_the_left_one_segment_at_a_time() {
        let segments = owned(&["~", "projects", "software", "dev", "fm", "src"]);
        assert_eq!(shorten(&segments, width_limit(24)), "~/p/software/dev/fm/src");
        assert_eq!(shorten(&segments, width_limit(18)), "~/p/s/dev/fm/src");
        assert_eq!(shorten(&segments, width_limit(14)), "~/p/s/d/fm/src");
    }

    #[test]
    fn never_abbreviates_the_current_directory() {
        let segments = owned(&["~", "projects", "software", "dev", "fm", "src"]);
        assert!(shorten(&segments, width_limit(1)).ends_with("/src"));
    }

    #[test]
    fn never_abbreviates_the_leading_root() {
        let segments = owned(&["/", "etc", "systemd", "user"]);
        assert_eq!(shorten(&segments, width_limit(1)), "/e/s/user");
    }

    #[test]
    fn keeps_the_dot_of_hidden_directories() {
        assert_eq!(initial(".config"), ".c");
        assert_eq!(initial("projects"), "p");
        assert_eq!(initial("."), ".");
        assert_eq!(initial(""), "");
    }

    #[test]
    fn dims_ancestors_and_emphasises_the_current_directory() {
        assert_eq!(
            markup("~/p/src"),
            "<span alpha=\"55%\">~/p/</span><b>src</b>"
        );
    }

    #[test]
    fn a_bare_root_has_no_ancestors_to_dim() {
        assert_eq!(markup("~"), "<b>~</b>");
    }

    #[test]
    fn escapes_pango_markup_syntax() {
        assert_eq!(
            markup("~/a&b/<c>"),
            "<span alpha=\"55%\">~/a&amp;b/</span><b>&lt;c&gt;</b>"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib path_title 2>&1 | tail -20`
Expected: compilation errors — `cannot find function segments in this scope`.

- [ ] **Step 3: Write the implementation**

Prepend to `src/path_title.rs`, above the test module:

```rust
//! Pure path shortening and markup for the window title.
//!
//! Ranger's trick: when the path outgrows the bar, ancestor names collapse to
//! their initial from the left, so `~/projects/software/dev/fm/src` becomes
//! `~/p/s/d/fm/src`. The directory the cursor is in is never abbreviated.

use std::path::Path;

/// Splits `path` into the segments the title shows, replacing a `home` prefix
/// with `~`.
pub fn segments(path: &Path, home: Option<&Path>) -> Vec<String> {
    if let Some(home) = home {
        if let Ok(rest) = path.strip_prefix(home) {
            let mut out = vec!["~".to_owned()];
            out.extend(rest.iter().map(|part| part.to_string_lossy().into_owned()));
            return out;
        }
    }

    path.iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect()
}

/// Shortens from the left until `fits` accepts the result. The last segment and
/// a leading `~` or `/` are never abbreviated; if nothing fits even fully
/// abbreviated, the caller's label ellipsises what is left.
pub fn shorten(segments: &[String], fits: impl Fn(&str) -> bool) -> String {
    let mut shortened = segments.to_vec();
    let mut candidate = join(&shortened);
    if fits(&candidate) {
        return candidate;
    }

    let last = shortened.len().saturating_sub(1);
    for index in 0..last {
        if index == 0 && (shortened[0] == "/" || shortened[0] == "~") {
            continue;
        }

        shortened[index] = initial(&shortened[index]);
        candidate = join(&shortened);
        if fits(&candidate) {
            return candidate;
        }
    }

    candidate
}

/// First character of a directory name, keeping the leading dot of hidden
/// directories: `.config` becomes `.c`.
pub fn initial(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        None => String::new(),
        Some('.') => match chars.next() {
            Some(second) => format!(".{second}"),
            None => ".".to_owned(),
        },
        Some(first) => first.to_string(),
    }
}

/// Pango markup for `path`: ancestors dimmed, the current directory emphasised.
pub fn markup(path: &str) -> String {
    match path.rsplit_once('/') {
        None => format!("<b>{}</b>", escape(path)),
        Some((ancestors, current)) => format!(
            "<span alpha=\"55%\">{}/</span><b>{}</b>",
            escape(ancestors),
            escape(current)
        ),
    }
}

/// Joins segments back into a path without doubling the leading separator.
fn join(segments: &[String]) -> String {
    match segments.split_first() {
        None => String::new(),
        Some((first, rest)) if first == "/" => format!("/{}", rest.join("/")),
        Some((first, rest)) if rest.is_empty() => first.clone(),
        Some((first, rest)) => format!("{}/{}", first, rest.join("/")),
    }
}

/// Escapes the three characters Pango markup treats as syntax.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
```

- [ ] **Step 4: Register the module**

In `src/lib.rs`, next to `mod layout;`:

```rust
mod path_title;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib path_title 2>&1 | tail -20`
Expected: `test result: ok. 10 passed`.

- [ ] **Step 6: Commit**

```bash
git add src/path_title.rs src/lib.rs
git commit -m "add ranger-style path shortening for the title"
```

---

### Task 3: Wire the layout into the panels

The columns start obeying the solver. Slivers come in Task 4, so a column below
the threshold still shows its listing and GTK floors it at 58px — expected at
this stage, not a bug.

**Files:**
- Modify: `src/component/directory_list.rs` (message enum around line 283, `update_with_view` around line 522)
- Modify: `src/component/app.rs` (model, `init`, `update_with_view`, `post_view`)
- Create: `/tmp/claude-1000/-home-milo/fd168cf3-cf4e-4ae8-8f68-4b8e917c7777/scratchpad/harness.sh`

**Interfaces:**
- Consumes: `layout::solve`, `layout::PanelLayout` from Task 1.
- Produces: `DirectoryMessage::SetLayout(layout::PanelLayout)` and `DirectoryMessage::ResetLayout`; `AppMsg::Relayout`; `AppModel::relayout(&self, widgets: &AppWidgets)`.

- [ ] **Step 1: Build the headless test harness**

Write `harness.sh` in the scratchpad. It is session tooling, not repo content —
do not commit it.

```bash
#!/usr/bin/env bash
# Runs fm in a nested headless sway and screenshots it.
# Usage:  harness.sh <shot-name> [keystrokes]
# Env:    RES=1600x900      starting output resolution
#         RESIZE_TO=900x700 resize the output after the keystrokes, then reshoot
set -u
SCRATCH="$(dirname "$0")"
SHOT="${1:?shot name}"
KEYS="${2:-}"
RES="${RES:-1600x900}"
RESIZE_TO="${RESIZE_TO:-}"

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
echo "display: $DISPLAY_NAME"

RUST_BACKTRACE=1 WAYLAND_DISPLAY="$DISPLAY_NAME" dbus-run-session -- \
  ./target/debug/fm "$HOME/.local/src/fm" > "$SCRATCH/fm.log" 2>&1 &

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

if [ -n "$RESIZE_TO" ]; then
  NESTED_SOCK="$XDG_RUNTIME_DIR/sway-ipc.$(id -u).$SWAY_PID.sock"
  SWAYSOCK="$NESTED_SOCK" swaymsg output HEADLESS-1 resolution "$RESIZE_TO"
  sleep 2
  WAYLAND_DISPLAY="$DISPLAY_NAME" grim "$SCRATCH/$SHOT-resized.png"
fi

grep -i "panicked" "$SCRATCH/fm.log" && echo "PANIC DETECTED" || echo "no panics"
kill "$SWAY_PID" 2>/dev/null
```

Then `chmod +x` it.

- [ ] **Step 2: Add the panel messages**

In `src/component/directory_list.rs`, add to the `DirectoryMessage` enum
(after `InvalidateCursor`):

```rust
    /// Take the width and page the app's column layout computed for this panel.
    SetLayout(crate::layout::PanelLayout),

    /// Fall back to the uniform column width; the window is too narrow to lay
    /// out and the view scrolls instead.
    ResetLayout,
```

- [ ] **Step 3: Handle them**

In the same file's `update_with_view` match, add two arms:

```rust
            DirectoryMessage::SetLayout(plan) => {
                widgets.root.set_visible(plan.visible);
                if plan.visible {
                    widgets.root.set_width_request(plan.width);
                }
            }
            DirectoryMessage::ResetLayout => {
                widgets.root.set_visible(true);
                widgets.root.set_width_request(WIDTH);
            }
```

- [ ] **Step 4: Let the preview absorb the remainder**

In `src/component/app.rs`, immediately after `let file_preview = FilePreviewModel::builder().launch(()).detach();`:

```rust
        // Every column has a computed width; the preview takes whatever is
        // left, so rounding can never produce a scrollbar.
        file_preview.widget().set_hexpand(true);
```

- [ ] **Step 5: Add the relayout message and method**

Add to `AppMsg`:

```rust
    /// The columns area changed size; recompute the column widths.
    Relayout,
```

Add to `impl AppModel`, next to `cursor_panel`:

```rust
    /// Applies the tapering column layout: ancestors thin out to the left, the
    /// cursor's column stays centred, and the preview absorbs the remainder.
    fn relayout(&self, widgets: &AppWidgets) {
        let area = widgets.directory_panes_scroller.width();
        let cursor = self
            .cursor_panel()
            .unwrap_or_else(|| self.directories.len().saturating_sub(1));

        match crate::layout::solve(area, self.directories.len(), cursor) {
            Some(plan) => {
                widgets.directory_panes.set_margin_start(plan.gutter);
                for (index, panel) in plan.panels.iter().enumerate() {
                    self.directories
                        .send(index, DirectoryMessage::SetLayout(*panel));
                }
            }
            None => {
                widgets.directory_panes.set_margin_start(0);
                for index in 0..self.directories.len() {
                    self.directories.send(index, DirectoryMessage::ResetLayout);
                }
            }
        }
    }
```

Add the message arm in `update_with_view`, next to `AppMsg::Toast`:

```rust
            // Handled by the relayout at the end of this function.
            AppMsg::Relayout => {}
```

And as the last statement of `update_with_view`, after the `match`:

```rust
        self.relayout(widgets);
```

- [ ] **Step 6: Replace the scroll-to-the-right behaviour**

Nothing overflows any more, so the auto-scroll must go. In `src/component/app.rs`:

Find every site first:

```bash
grep -n "update_directory_scroll_position\|set_adjustment_to_upper_bound" src/component/app.rs
```

That lists the field, its initialiser, four assignments, the `post_view` block,
the function itself, and the `hadjustment` hook. Then:

1. Delete the `update_directory_scroll_position` field from `AppModel` and its
   initialiser in `init`.
2. Delete every `self.update_directory_scroll_position = ...` assignment,
   including the `= false` reset at the top of `update_with_view`.
3. Delete the whole `if self.update_directory_scroll_position { ... }` block
   from `post_view`, keeping the `is_maximized` block above it.
4. Delete the `set_adjustment_to_upper_bound` function at the bottom of the file
   and its doc comment.
5. Replace the `hadjustment` hook in `init` with one that reacts to the viewport
   changing size:

```rust
        // page-size is the viewport width: it changes on window resize and when
        // the places sidebar is folded away.
        widgets
            .directory_panes_scroller
            .hadjustment()
            .connect_notify_local(Some("page-size"), {
                let sender = sender.clone();
                move |_, _| sender.input(AppMsg::Relayout)
            });
```

- [ ] **Step 7: Build**

Run: `cargo build 2>&1 | grep -E "^error" -A 5`
Expected: no output. Fix any compile error before continuing.

- [ ] **Step 8: Verify at depth 0 and depth 3**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/fd168cf3-cf4e-4ae8-8f68-4b8e917c7777/scratchpad
$S/harness.sh task3-root
$S/harness.sh task3-deep jjjjjjjljjjljl
```

Read both PNGs. Expected in `task3-root.png`: one listing column, a wide empty
gutter to its left, the preview filling the right, **no horizontal scrollbar**.
Expected in `task3-deep.png`: three columns of visibly decreasing width to the
left, the cursor's column at the same horizontal position as in the root shot,
still no scrollbar. Expected in both: `no panics`.

- [ ] **Step 9: Commit**

```bash
git add src/component/app.rs src/component/directory_list.rs
git commit -m "taper the ancestor columns and centre the cursor column"
```

---

### Task 4: Slivers

A column below `SLIVER_THRESHOLD` cannot show a listing — measured floor 58px —
so it switches to a thin strip carrying the directory's initial.

**Files:**
- Modify: `src/component/directory_list.rs` (struct, `view!`, `init_model`, `init_widgets`, `update_with_view`)
- Modify: `src/styles.css`

**Interfaces:**
- Consumes: `layout::PanelLayout.sliver` from Task 1, `path_title::initial` from Task 2, `DirectoryMessage::SetLayout` from Task 3.
- Produces: nothing new for later tasks.

- [ ] **Step 1: Give the panel a sliver flag**

In `src/component/directory_list.rs`, add to the `Directory` struct:

```rust
    /// True while this panel is too narrow to show its listing. Shared with the
    /// loading handler so a refresh cannot bounce a sliver back to the listing.
    sliver: std::rc::Rc<std::cell::Cell<bool>>,
```

And in `init_model`'s constructor, next to `cursor: Default::default(),`:

```rust
            sliver: Default::default(),
```

- [ ] **Step 2: Add the sliver page**

In the `view!` block, add the homogeneity opt-out under `set_width_request`:

```rust
            // Only the visible page's width is requested, so a sliver can go
            // below the 58px a listing needs.
            set_hhomogeneous: false,
```

and a third page between the spinner and the scroller:

```rust
            #[name = "sliver_label"]
            add_child = &gtk::Label {
                add_css_class: "column-sliver",
                set_vexpand: true,
                set_yalign: 0.0,
                set_margin_top: 8,
            } -> { set_name: "sliver" },
```

- [ ] **Step 3: Choose the page from both loading and sliver state**

Add this free function near the bottom of the file, beside the other helpers:

```rust
/// Chooses a panel's stack page: the spinner while the listing loads, then
/// either the listing or the thin sliver strip.
fn apply_page(stack: &gtk::Stack, loading: bool, sliver: bool) {
    stack.set_visible_child_name(if loading {
        "spinner"
    } else if sliver {
        "sliver"
    } else {
        "listing"
    });
}
```

In `init_widgets`, replace the existing property binding —

```rust
        self.directory_list()
            .bind_property("loading", &widgets.root, "visible-child-name")
            .transform_to(|_, loading| Some(if loading { "spinner" } else { "listing" }))
            .sync_create()
            .build();
```

— with a handler that knows about all three pages, plus the strip's letter:

```rust
        let name = self
            .dir()
            .basename()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        widgets.sliver_label.set_text(&crate::path_title::initial(&name));

        let directory_list = self.directory_list();
        directory_list.connect_loading_notify(clone!(
            #[weak(rename_to = stack)]
            widgets.root,
            #[strong(rename_to = sliver)]
            self.sliver,
            move |list| apply_page(&stack, list.is_loading(), sliver.get())
        ));
        apply_page(
            &widgets.root,
            directory_list.is_loading(),
            self.sliver.get(),
        );
```

If `connect_loading_notify` does not resolve, use
`connect_notify_local(Some("loading"), move |list, _| ...)` on the same object;
the property is the same.

- [ ] **Step 4: Switch pages on relayout**

Replace the `SetLayout` arm written in Task 3 with:

```rust
            DirectoryMessage::SetLayout(plan) => {
                widgets.root.set_visible(plan.visible);
                if plan.visible {
                    widgets.root.set_width_request(plan.width);
                    self.sliver.set(plan.sliver);
                    apply_page(
                        &widgets.root,
                        self.directory_list().is_loading(),
                        plan.sliver,
                    );
                }
            }
```

and the `ResetLayout` arm with:

```rust
            DirectoryMessage::ResetLayout => {
                widgets.root.set_visible(true);
                widgets.root.set_width_request(WIDTH);
                self.sliver.set(false);
                apply_page(&widgets.root, self.directory_list().is_loading(), false);
            }
```

- [ ] **Step 5: Style the strip**

Append to `src/styles.css`:

```css
.column-sliver {
  background-color: alpha(@theme_fg_color, 0.05);
  color: alpha(@theme_fg_color, 0.5);
  font-weight: bold;
}
```

- [ ] **Step 6: Build**

Run: `cargo build 2>&1 | grep -E "^error" -A 5`
Expected: no output.

- [ ] **Step 7: Verify at depth 6**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/fd168cf3-cf4e-4ae8-8f68-4b8e917c7777/scratchpad
$S/harness.sh task4-slivers jjjjjjjljjjljljljl
```

Read the PNG. Expected: the two or three leftmost columns render as narrow
tinted strips with a single letter at the top rather than squeezed listings; the
columns closer to the cursor still show their listings; no horizontal scrollbar;
`no panics`.

- [ ] **Step 8: Verify the primary flow still works**

```bash
S=/tmp/claude-1000/-home-milo/fd168cf3-cf4e-4ae8-8f68-4b8e917c7777/scratchpad
$S/harness.sh task4-flow jjjlkjhjjonjj
```

That descends, moves the cursor, goes back up with `h`, re-sorts by name with
`on`, and moves again. Expected: `no panics`, and a coherent screenshot — a past
regression in this fork shipped because only the new shortcut was exercised and
never a plain selection.

- [ ] **Step 9: Commit**

```bash
git add src/component/directory_list.rs src/styles.css
git commit -m "render columns too narrow for a listing as initial strips"
```

---

### Task 5: The path in the title bar

**Files:**
- Modify: `src/component/app.rs` (imports, `view!` header, `impl AppModel`, `update_with_view`)

**Interfaces:**
- Consumes: `path_title::{segments, shorten, markup}` from Task 2.
- Produces: `AppModel::retitle(&self, widgets: &AppWidgets)`.

- [ ] **Step 1: Import pango**

In `src/component/app.rs`, change the gtk import to:

```rust
use gtk::{gdk, gio, glib, pango, prelude::*};
```

- [ ] **Step 2: Add the title widget**

In the `view!` block, as the first entry inside `adw::HeaderBar` (before the
existing `pack_end` entries):

```rust
                        #[wrap(Some)]
                        #[name = "path_title"]
                        set_title_widget = &gtk::Label {
                            add_css_class: "title",
                            set_hexpand: true,
                            set_single_line_mode: true,
                            // Last resort when even the fully abbreviated path
                            // overflows a very narrow window.
                            set_ellipsize: pango::EllipsizeMode::Middle,
                        },
```

Leave `set_title: Some("fm")` on the window untouched: the operator's waybar
rules match on the window title.

- [ ] **Step 3: Add the retitle method**

In `impl AppModel`, after `relayout`:

```rust
    /// Retitles the header with the path the cursor is in, abbreviating
    /// ancestor names from the left until the label's width accepts it.
    fn retitle(&self, widgets: &AppWidgets) {
        let cursor = self
            .cursor_panel()
            .unwrap_or_else(|| self.directories.len().saturating_sub(1));

        let Some(path) = self
            .directories
            .get(cursor)
            .and_then(|panel| panel.dir().path())
        else {
            return;
        };

        let segments = crate::path_title::segments(&path, Some(&glib::home_dir()));

        let label = &widgets.path_title;
        let available = label.width();
        let layout = label.create_pango_layout(None);
        let fits = |candidate: &str| {
            // Before the first allocation there is nothing to measure against;
            // show the full path and let the next relayout shorten it.
            if available <= 0 {
                return true;
            }
            layout.set_text(candidate);
            layout.pixel_size().0 <= available
        };

        label.set_markup(&crate::path_title::markup(&crate::path_title::shorten(
            &segments, fits,
        )));
    }
```

- [ ] **Step 4: Call it**

In `update_with_view`, next to the `self.relayout(widgets);` added in Task 3:

```rust
        self.retitle(widgets);
```

- [ ] **Step 5: Build**

Run: `cargo build 2>&1 | grep -E "^error" -A 5`
Expected: no output.

- [ ] **Step 6: Verify shallow and deep**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/fd168cf3-cf4e-4ae8-8f68-4b8e917c7777/scratchpad
$S/harness.sh task5-shallow jjjjjjjl
$S/harness.sh task5-deep jjjjjjjljjjljljljljl
```

Read both PNGs. Expected in `task5-shallow.png`: the header reads
`~/.local/src/fm/src` or similar, ancestors visibly dimmer than the last
segment. Expected in `task5-deep.png`: leading ancestors collapsed to single
letters, the last segment still spelled out in full. Expected in both:
`no panics`.

- [ ] **Step 7: Commit**

```bash
git add src/component/app.rs
git commit -m "show the cursor path in the header, abbreviated to fit"
```

---

### Task 6: Live resize, folded sidebar, and the degenerate window

No code unless something breaks. This exercises the `page-size` hook — the
riskiest new wiring, since a relayout that changed the viewport would loop —
and the narrow-window escape hatch.

**Files:** none expected. Fixes land in `src/component/app.rs` or `src/layout.rs`
if a check fails.

**Interfaces:**
- Consumes: everything from Tasks 1–5.
- Produces: nothing.

- [ ] **Step 1: Resize from wide to narrow while running**

```bash
cd ~/.local/src/fm
S=/tmp/claude-1000/-home-milo/fd168cf3-cf4e-4ae8-8f68-4b8e917c7777/scratchpad
RESIZE_TO=900x700 $S/harness.sh task6-resize jjjjjjjljjjljl
```

Read `task6-resize.png` and `task6-resize-resized.png`. Expected: after the
resize the columns are narrower but still tapered, the cursor's column is still
centred in the columns area, and no horizontal scrollbar appeared. Expected:
`no panics`.

The screenshot is also the loop check. A feedback loop between the relayout and
the `page-size` notify would spin the main loop and the second `grim` would come
back blank or stale — if either shot is empty, instrument
`AppModel::relayout` with a `tracing::debug!` and confirm it settles instead of
firing continuously.

- [ ] **Step 2: Confirm the folded sidebar is handled**

At 900x700 the `adw::Flap` folds the places sidebar away on its own, which
widens the columns area without the window changing size. In
`task6-resize-resized.png`, verify the columns area starts at the left window
edge and the layout used that extra width — the gutter or the ancestors grew,
and the cursor's column is centred on the *new* area, not the old one.

- [ ] **Step 3: Confirm the escape hatch at an absurd width**

```bash
S=/tmp/claude-1000/-home-milo/fd168cf3-cf4e-4ae8-8f68-4b8e917c7777/scratchpad
RES=420x700 $S/harness.sh task6-narrow jjjl
```

Expected: `layout::solve` returns `None`, every column falls back to the uniform
200px, and the view scrolls horizontally as it did before this work. This is a
survival mode, not a supported layout — it only has to not crash and not look
broken. Expected: `no panics`.

- [ ] **Step 4: Commit any fixes**

Only if Steps 1–3 forced a change:

```bash
git add -u
git commit -m "fix the column layout under live resize"
```

---

### Task 7: Install and hand over

**Files:** none modified.

- [ ] **Step 1: Run the whole unit suite**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: `test result: ok. 20 passed`.

- [ ] **Step 2: Check for new warnings**

Run: `cargo build 2>&1 | grep -c "^warning"`
Expected: the same count as before the work — 2 pre-existing upstream warnings.

- [ ] **Step 3: Push the branch**

```bash
git push origin HEAD:master
```

- [ ] **Step 4: Install the release build**

```bash
cargo install --path ~/.local/src/fm --force
```

Then tell the operator to launch `fm` themselves — never open a GUI window into
their live session from this agent.
