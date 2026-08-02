# Debug Session: list-blank-space

Status: [OPEN]

## Symptom

The Pixiv list contains all entries and can scroll, but a large blank region remains below the visible rows. The list scrollbar and separator appear misaligned.

## Hypotheses

- H1: The outer list container height is fixed while the ScrollArea only occupies content height.
- H2: `show_rows` calculates a virtual content height inconsistent with thumbnail row height.
- H3: Extra height allocation inside the list column creates the blank region.
- H4: The scrollbar and separator use different rectangles in the parent layout.

## Evidence Plan

Instrument the list column and ScrollArea geometry without changing layout behavior. Compare the logged rectangles and available heights with the screenshot.

## Reproduction

Run the Windows build, open a feed with many entries, and capture `glean-debug.log` after the list is visible and scrolled.

