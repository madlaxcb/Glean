# Debug Session: list-blank-space

Status: [OPEN]

## Symptom

The Pixiv list contains all entries and can scroll, but a large blank region remains below the visible rows. The list scrollbar and separator appear misaligned.

## Hypotheses

- H1: The outer list container height is fixed while the ScrollArea only occupies content height. — REJECTED: logs show `list-contents-after` height=0, ScrollArea fills container.
- H2: `show_rows` `row_height=38.0` is larger than the actual rendered row height (~32px), creating phantom virtual content height that shows as blank when scrolled to bottom. — PENDING VERIFICATION.
- H3: Extra height allocation inside the list column creates the blank region. — REJECTED: `list-contents-after` shows no remaining space.
- H4: The scrollbar and separator use different rectangles in the parent layout. — REJECTED: geometry logs show consistent rects.

## Evidence Collected (Round 1)

From `glean-debug.log`:
```
[list-geometry] outer=(206.0,78.0,726.0,1355.3) available=(520.0,1277.3) list_width=520.0 full_height=1277.3
[list-contents] before=(206.0,112.0,726.0,1355.3) available=(520.0,1243.3) rows=687 row_height=38.0
[list-contents-after] after=(206.0,1357.3,726.0,1357.3) available=(520.0,0.0)
```

Analysis:
- List column: y 78→1355.3 (height 1277.3). Header offset 34px (78→112).
- ScrollArea: y 112→1355.3 (height 1243.3). Fills container (after height=0).
- Virtual content height = 687 × 38 = 26106px (much larger than viewport).
- The 34px header offset is from `column_contents` (title + separator + spacing).

## Instrumentation (Round 2)

Added `[list-row-height]` and `[list-scroll-out]` logs to measure:
- `actual_first`: measured height of first rendered row (image 30px + label)
- `content_size`: ScrollArea output content_size (actual total content height)
- `offset_y`: current scroll offset
- `virtual_total`: row_height × num_rows (what egui uses for scrollbar)

## Root Cause Hypothesis (H2 detail)

Row rendering (ui.rs L1884-1891):
```rust
ui.horizontal(|ui| {
    ui.allocate_space(Vec2::splat(30.0));  // 30px image/space
    ui.selectable_label(selected, rich)     // ~22px with 15.5px font + padding
});
```
Expected actual row height ≈ max(30, 22) + item_spacing.y(2) ≈ 32px.
But `row_height=38.0` is passed to `show_rows`. Discrepancy = 6px/row × 687 rows ≈ 4122px phantom space.

## Reproduction

Run the Windows build, open a feed with many entries, scroll to bottom, capture `glean-debug.log`.

