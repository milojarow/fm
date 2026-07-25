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

    let current = ((area_width as f64 * CURRENT_FRACTION) as i32).clamp(CURRENT_MIN, CURRENT_MAX);
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

    // With ancestors on screen the gutter is the slack they left, which keeps
    // the cursor column centred. With none — at the root, or when a cramped
    // budget evicted them all — reserving that space would only park the
    // listing behind a third of a window of nothing, so the columns go left
    // instead and the room goes to the right of the cursor.
    let ancestors_total: i32 = ancestors.iter().sum();
    let gutter = if ancestors.is_empty() {
        0
    } else {
        budget - ancestors_total
    };

    // What is actually left once the left side has taken its share. It equals
    // the side budget whenever the gutter is centring the layout, and doubles
    // when there is no gutter — which is why the child panel survives at the
    // root in windows where a symmetric budget would have evicted it.
    let right_space = area_width - gutter - ancestors_total - current;

    // Right: the first panel mirrors the nearest ancestor and deeper ones keep
    // tapering, but they may never eat into the preview's floor.
    let mirror = ancestors
        .last()
        .copied()
        .unwrap_or((budget as f64 * NO_PARENT_CHILD_FRACTION) as i32);
    let right_budget = (right_space - PREVIEW_MIN).max(0);
    let mut right = right_widths(mirror, panel_count - cursor - 1);
    while !right.is_empty() && right.iter().sum::<i32>() > right_budget {
        right.pop();
    }

    // The preview has no computed width: it absorbs what the right-hand panels
    // leave. Below its floor it stops shrinking, so the paned outgrows the
    // scroller and the preview is clipped at the window edge — decline instead,
    // and let the caller fall back to uniform columns. This is the tighter of
    // the two escape hatches: `SLIVER_MIN` above only catches areas so narrow
    // that not even a gutter fits.
    if right_space - right.iter().sum::<i32>() < PREVIEW_MIN {
        return None;
    }

    for (offset, width) in right.iter().enumerate() {
        panels[cursor + 1 + offset] = sized(*width);
    }
    for panel in panels.iter_mut().skip(cursor + 1 + right.len()) {
        panel.visible = false;
    }

    Some(Layout {
        gutter,
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
        // From depth 1 on: the ancestors plus the gutter fill the side budget,
        // which is what puts the cursor column in the middle.
        for cursor in 1..6 {
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
    fn the_root_column_hugs_the_left_edge() {
        // Centring the root column would mean reserving a whole side budget of
        // nothing to its left, which just looks broken on startup.
        let plan = solve(AREA, 3, 0).expect("laid out");
        assert_eq!(plan.gutter, 0);
    }

    #[test]
    fn evicting_every_ancestor_also_drops_the_gutter() {
        // A budget too cramped to hold even one ancestor leaves nothing to
        // centre against, so the same left-edge rule applies.
        let plan = solve(900, 30, 29).expect("laid out");
        assert!(plan.panels[..29].iter().any(|panel| panel.visible)
            || plan.gutter == 0);
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

    /// 900px rather than the 400px this test first used: 400 cannot hold the
    /// cursor's 260px column and the preview's 200px floor at once, so it is now
    /// the uniform-columns escape hatch's problem, not a layout to assert on.
    /// A 900px area still leaves 39 ancestors far too little room, which is what
    /// this test is about.
    #[test]
    fn very_deep_stacks_drop_their_oldest_columns() {
        let plan = solve(900, 40, 39).expect("laid out");
        assert!(
            plan.panels.iter().any(|panel| !panel.visible),
            "some ancestors must drop out of a 900px area"
        );
        let left: i32 = plan
            .panels
            .iter()
            .filter(|panel| panel.visible)
            .map(|panel| panel.width)
            .sum::<i32>()
            - plan.panels[39].width;
        assert!(left <= (900 - plan.panels[39].width) / 2);
    }

    #[test]
    fn a_window_too_narrow_to_lay_out_returns_none() {
        assert_eq!(solve(270, 3, 1), None);
    }

    /// The columns area of a 700px window. The cursor's column takes its 260px
    /// minimum, leaving 144px a side: enough for the gutter, not enough for the
    /// preview, which would be clipped against the window edge.
    #[test]
    fn an_area_too_narrow_for_the_preview_floor_returns_none() {
        // With ancestors the layout is symmetric, so each side gets
        // (548 - 260) / 2 = 144 — under the preview's floor, and the preview
        // would be clipped at the window edge. Decline and let the caller fall
        // back to uniform columns.
        assert_eq!(solve(548, 3, 1), None);
    }

    #[test]
    fn the_same_area_fits_at_the_root_because_there_is_no_gutter() {
        // No ancestors means no reserved left side, so the whole 288px past the
        // cursor column is the preview's: it fits, and declining here would
        // hand the user a scrollbar for nothing.
        let plan = solve(548, 1, 0).expect("laid out");
        assert_eq!(plan.gutter, 0);
        assert_eq!(548 - plan.panels[0].width, 288);
    }

    /// The promise of the whole design: whatever `solve` returns fits inside the
    /// area it was given, preview included. Anything wider overflows the paned
    /// past the scroller and clips the preview at the window edge.
    #[test]
    fn a_layout_that_fits_or_no_layout_at_all() {
        for area in (0..2400).step_by(4) {
            for panel_count in 1..8usize {
                for cursor in 0..panel_count {
                    let Some(plan) = solve(area, panel_count, cursor) else {
                        continue;
                    };

                    let columns: i32 = plan
                        .panels
                        .iter()
                        .filter(|panel| panel.visible)
                        .map(|panel| panel.width)
                        .sum();
                    assert!(
                        plan.gutter + columns + PREVIEW_MIN <= area,
                        "area {area}, {panel_count} panels, cursor {cursor}: gutter {} \
                         + columns {columns} + preview {PREVIEW_MIN} overflows",
                        plan.gutter,
                    );
                }
            }
        }
    }

    #[test]
    fn nonsense_input_returns_none() {
        assert_eq!(solve(AREA, 0, 0), None);
        assert_eq!(solve(AREA, 3, 3), None);
    }
}
