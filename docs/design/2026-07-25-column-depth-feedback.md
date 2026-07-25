# Depth feedback: tapering ancestor columns and a live path title

Status: approved, ready for implementation
Date: 2026-07-25

## Goal

Make the column holding the keyboard cursor obvious at a glance, at any depth.

Two changes, one idea:

1. **Layout.** Ancestor columns get progressively thinner to the left while the
   cursor's column stays centred in the columns area and never moves. Nothing
   overflows, so horizontal scrolling disappears.
2. **Title bar.** The header shows the cursor's path instead of the constant
   string `fm`, with ancestors dimmed and the current directory emphasised, and
   shortens ancestor names to their initial (ranger-style) when space runs out.

## Evidence

Measured before designing, with PyGObject probes against the real libraries in a
nested headless sway session. These numbers drive the design; do not re-derive
them.

| Question | Answer | How |
|---|---|---|
| Does `PanelPaned` honour an exact per-child `width_request`? | Yes. 60→60, 160→160, 400→400, and a live change 400→240 applies. Identical to a `GtkBox` control. | `probe_paned.py` |
| Who absorbs leftover width? | The child with `hexpand`. It went 617→777 when another child shrank by 160. | same |
| How narrow can a real `fm` column get? | **58px minimum** with the listing shown — the row's icon + ellipsised label + arrow, propagated through a `ScrolledWindow` in `PolicyType::Never`. | `probe_floor.py` |
| Can a `Stack` page go below that? | Only with `hhomogeneous = false`. At the default `true` the Stack keeps requesting 58px on any page; at `false` a thin page requests 9px. | same |
| Where do new panels land in the `Paned`? | Left of the preview, in index order. The preview is always the last child. | screenshot at depth 2 |
| What is the columns area? | The window minus the Places sidebar (~152px), which lives in the `adw::Flap`, outside the `Paned`. | same |

The 58px floor is why the thin-column page below is a requirement rather than a
decoration: the taper is physically impossible without it.

## Geometry

Let `W` be the width of the directory panes scroller viewport — the columns
area, sidebar excluded.

```
C = clamp(CURRENT_FRACTION · W, CURRENT_MIN, CURRENT_MAX)   width of ACTUAL
A = (W − C) / 2                                              side budget
```

Equal budgets on both sides *are* the centring: no measurement or scrolling is
involved, `ACTUAL` lands in the middle by construction.

**ACTUAL** is `cursor_panel()`, falling back to the deepest panel when nothing is
selected — the same fallback `SearchOpen`, `NavFirst` and `NavLast` already use.

### Left side

The `k` ancestors share `A` with geometric weights, hugging `ACTUAL`:

```
weight(d) = TAPER_RATIO ^ d          d = distance to ACTUAL, nearest ancestor d = 0
width(d)  = floor(A · weight(d) / Σ weights)
```

Widths are floored, never rounded: rounding five ancestors up can overshoot `A`
by a pixel and evict a column for no reason.

Then clamp each width up to `SLIVER_MIN`. If the clamped widths exceed `A`, drop
the oldest ancestor — `set_visible(false)` on its root, so it stops requesting
width without being removed from the factory — and recompute, until they fit.
Dropped panels become visible again as soon as the budget allows.

The gutter is whatever the ancestors did not consume:

```
gutter = A − Σ (widths of visible ancestors)      when any ancestor is visible
gutter = 0                                        otherwise
```

The gutter is a `margin_start` on the `panel::Paned`. With ancestors on screen it
absorbs the pixels lost to flooring, keeping the left side exactly `A` and
`ACTUAL` exactly centred.

**Revised 2026-07-25, after using it.** The original rule reserved `A` even with
no ancestors, so `ACTUAL` was centred from startup and never moved again. In
practice that opens `fm` with roughly a third of the window blank to the left of
the listing, which reads as broken. With no ancestors there is nothing to centre
against, so the columns go to the left edge instead and the space goes to the
right of the cursor. The cost, accepted knowingly: `ACTUAL` shifts once, on the
first descent, and is stable from then on.

This also applies when a cramped budget evicted every ancestor — same condition,
same reason.

### Space to the right

`A` describes the left side. What the right side actually has is whatever the
left did not take:

```
right space = W − gutter − Σ (visible ancestor widths) − C
```

That equals `A` whenever the gutter is centring the layout, and doubles to
`W − C` when there is no gutter. The right-hand panels and the preview floor are
measured against *this*, not against `A` — otherwise the root view would evict
its child panel in windows where it comfortably fits.

### Right side

Panels can exist to the right of `ACTUAL` — pressing `h` unselects the cursor's
panel without popping deeper ones, so more than one may be present.

- The first panel right of `ACTUAL` takes the same width as the first ancestor
  to its left, mirroring the layout around `ACTUAL`. With no ancestors (`k = 0`)
  it falls back to `NO_PARENT_CHILD_FRACTION · A`.
- Deeper right-hand panels continue the same `TAPER_RATIO` progression.
- The preview gets **no computed width**. It carries `hexpand` and absorbs
  whatever is left, which is `A −` (right-hand panels) by construction.
- The right-hand panels may claim at most `A − PREVIEW_MIN`. Repeated `h`
  presses can leave a long tail of panels to the right of the cursor, and
  without this cap they would starve the preview into a sliver of its own. When
  the tapered widths exceed the cap, hide the deepest panel and recompute.

Letting the preview absorb the remainder makes the arithmetic rounding-proof:
if the computed widths are off by a few pixels, the preview swallows the error
instead of producing a scrollbar.

`adw::Clamp` (the preview root) stays visible even when empty — only its inner
box is hidden (`file_preview.rs:210`) — so it can always carry the `hexpand`
without a special case.

### Degenerate windows

The layout is declined whenever the preview would end up under its floor:

```
A − Σ (widths of the right-hand panels) < PREVIEW_MIN
```

The right-hand panels are already capped at `A − PREVIEW_MIN`, so that cap
absorbs any tail of them and the condition reduces to `A < PREVIEW_MIN` — a
columns area under **660px** (`C` is pinned at `CURRENT_MIN` below `W = 866`, so
`A = (W − 260) / 2 < 200 ⟺ W < 660`), which is roughly a 810px window with the
Places sidebar showing and a 660px one once the flap has folded it away. The
`A < SLIVER_MIN` check that guards the taper sits well inside that band and
never decides anything on its own.

In the declined band: restore the uniform `WIDTH` request on every panel, clear
the margin, and let the scroller behave as it did before this feature — the
panels do overflow there, so the view follows them, keeping the newest column
in sight (`hadjustment.value = upper`, applied on relayout and on the
adjustment's `upper` changing). An escape hatch, not a supported layout: it only
has to stay usable.

### Worked example

Window 1600px, sidebar 152px, so `W = 1448`, `C = 434`, `A = 507`, five
ancestors and one child panel:

| | gutter | abu+4 | abu+3 | abu+2 | abuelo | padre | **ACTUAL** | hija | preview |
|---|---|---|---|---|---|---|---|---|---|
| px | 4 | 43 | 62 | 89 | 127 | 182 | **434** | 182 | 325 |
| page | — | sliver | sliver | listing | listing | listing | listing | listing | — |

Left side: `4 + 43 + 62 + 89 + 127 + 182 = 507 = A`. Right side:
`182 + 325 = 507 = A`. `ACTUAL` is centred.

## Column states

The `gtk::Stack` at `directory_list.rs:295` gains a third page.

| page | when | contents |
|---|---|---|
| `spinner` | loading | existing |
| `listing` | computed width ≥ `SLIVER_THRESHOLD` | existing |
| `sliver` | computed width < `SLIVER_THRESHOLD` | **new** — muted strip, directory initial at the top |

The initial is the first character of the directory's display name, keeping a
leading dot for hidden directories (`.config` → `.c`), matching the abbreviation
used in the title bar so both read as the same breadcrumb.

`set_hhomogeneous(false)` on that Stack is load-bearing: without it the Stack
keeps requesting the listing's 58px on the sliver page and no column can taper.

The page is currently chosen by a property binding from the directory list's
`loading` flag (`directory_list.rs:497`), which only knows two pages. That
binding is replaced by a `loading` notify handler that consults both the loading
flag and the panel's sliver state, so a reload cannot silently drop a sliver back
to the listing page.

## Title bar

Replace `set_title: Some("fm")` with a `set_title_widget` holding a `gtk::Label`
using Pango markup: ancestor segments in a dimmed colour, the current directory
in bold. The path is the **cursor panel's** directory, not the deepest panel's.

A location with no local path — `trash:///` from the Places sidebar, an `smb://`
share, a phone over MTP — is titled from its URI with the same dimming, last
component emphasised. Keeping the previous directory's path on screen there
would state the user is somewhere they are not.

Shortening, applied in order until the rendered width fits the label's allocation:

1. Replace a `$HOME` prefix with `~`.
2. Abbreviate the leftmost still-full segment to its initial, preserving a
   leading dot. Repeat.

The last segment is never abbreviated, and neither is the leading `~` or `/`.
Fit is tested by measuring candidate strings with a `pango::Layout` built from
the label's own context, so the check matches what will actually be drawn.

If everything is abbreviated and it still does not fit — a window narrow enough
that even `~/p/s/d/fm/src` overflows — the label's own
`EllipsizeMode::Middle` handles the remainder. No hand-rolled ellipsis.

```
~/projects/software/dev/fm/src
~/p/software/dev/fm/src
~/p/s/dev/fm/src
~/p/s/d/fm/src
```

## Code layout

```
app.rs
  relayout(widgets)      reads scroller.width(), computes the table above,
                         sends one message per panel, sets the paned margin,
                         sets hexpand on the preview root
  title markup           built from the cursor panel's path, same trigger
                         points; a location with no local path (trash:///,
                         smb://…) is titled from its URI instead
  scroll follow          kept only for the declined band, where uniform columns
                         still overflow: the `upper` notify hook scrolls to the
                         end while that fallback is in force, and is silent
                         under a computed layout

directory_list.rs
  DirectoryMessage::SetLayout { width, sliver }
                         each panel applies set_width_request to its own root
                         and switches the Stack page
  sliver page + set_hhomogeneous(false)

styles.css
  .column-sliver         muted strip and initial
```

`app.rs` computes, each `Directory` applies its own width through a message.
No component reaches into another's widgets — the same relm4 pattern the rest of
the fork already follows.

### Triggers

Relayout and retitle run on:

- a change of cursor panel (`NewSelection`, `NavParent`, `NavInto`, `NewRoot`),
- a panel push or pop,
- a width change of the directory panes scroller, which also covers window
  resizes, maximise, and the Places sidebar being collapsed.

Width changes are instant, not animated. Relayout only happens when depth
changes, never on `j`/`k` inside a panel, so there is nothing to smooth over.

## Constants

| name | value |
|---|---|
| `CURRENT_FRACTION` | 0.30 |
| `CURRENT_MIN` | 260 |
| `CURRENT_MAX` | 520 |
| `TAPER_RATIO` | 0.7 |
| `SLIVER_THRESHOLD` | 72 |
| `SLIVER_MIN` | 12 |
| `NO_PARENT_CHILD_FRACTION` | 0.45 |
| `PREVIEW_MIN` | 200 |

## Out of scope

- Animating width changes.
- Any extra highlight on the current *column* (accent border, dimming the
  others). The taper and the fixed position already carry the signal.

The cursor *row* did get one, added 2026-07-25 on request: a cyan glow keyed to
the desktop accent `#00ecec`. It cannot ride the GTK selection — a provider rule
on `listview > row` never reaches those nodes, measured with a magenta test rule
that painted nothing while a class rule on our own widget painted fine — so it
wears a `.cursor-row` class on the row's box, the same mechanism marks already
use. Only the panel that owns the cursor lights up; ancestors keep their
selection as a quiet breadcrumb, or every column would glow at once.
- Persisting anything new in `state.json`. The layout is derived, not stored.

## Verification

In the nested headless sway harness, with `RUST_BACKTRACE=1` and stderr checked
for panics:

1. Startup at root: `ACTUAL` centred, left budget empty, no horizontal scrollbar.
2. Descend five levels: ancestors taper, the two oldest render as slivers,
   `ACTUAL` occupies the same pixels as in step 1, still no scrollbar.
3. `h` back up: panels to the right of the cursor keep their taper.
4. Resize the window narrow and wide: widths recompute, no scrollbar appears.
5. Collapse the Places sidebar: the columns area recentres.
6. Title bar: full path at depth 2, abbreviated left-to-right at depth 8, current
   directory never abbreviated.
7. Exercise the primary flow — select, navigate, open — since a past regression
   in this fork came from touching the models without doing so.
