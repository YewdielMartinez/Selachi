Selachi fork of gpui 0.2.2 (Apache-2.0). Patch: ../../bench/patches/gpui-selachi.patch
To update: copy new gpui from ~/.cargo/registry, rm .cargo-ok, patch -p1 < bench/patches/gpui-selachi.patch

## What the patch changes

1. Windows vsync thread wake-ups (`wake_frame_clock`, `src/platform/**`, `src/window.rs`): 0 % CPU at rest.
2. `src/elements/text.rs` (`TextLayout`, the engine of `StyledText`, `&str` and `String` elements):
   text that painted at a width other than its box's (one letter per line, descriptions running
   under the controls, no ellipsis).
   * Upstream kept ONE measurement and answered every later query with it when the query had no
     width (`wrap_width.is_none()`). Taffy asks a flex row's text for `Definite(0)`, MaxContent and
     MinContent in a row, so a max-content query got the size shaped for width 0 (a tall, 0 px wide
     column). The same early return made `nowrap + ellipsis` text keep its first unconstrained
     measure forever (never truncated), and `truncate_line` rewrote the shared `runs` on every call.
   * Now: measures are cached by the exact key (wrap width, truncate width); each shaping clones
     the runs; the element keeps what it needs to shape again (`Shaping`).
   * Min-content (CSS): a wrapping text answers `MinContent` with the width of its widest
     unbreakable word (`min_content::widest_word`: the segments are the ones `LineWrapper` can
     break at, i.e. after a space and before a non-word character), so a flex item stays inside
     its row without `min_w_0()` AND a pill, a field's error text or a label squeezed in a row
     never collapses into a column of letters. (An earlier revision answered 0 here: wrong, it
     let taffy shrink such items to nothing.) A word wider than the box is only broken when the
     final width is definite and smaller (the `prepaint` reshape). The widths are cached per
     text + font size + run fonts (thread local, one hash lookup per query, emptied at 4096
     entries); nothing is shaped again while the cache holds. Nowrap text keeps max-content.
   * `TextLayout::prepaint` now takes `window` and `cx`. Taffy may answer the final layout from its
     own cache, so the lines left by the last measure call can belong to another width than the
     final bounds; `ShapeFit::is_stale_for` decides (1 px tolerance for DPI rounding: a box 0.5 px
     narrower than its text does not wrap the last word; widening reflows wrapped text; nowrap text
     without an ellipsis never changes) and the text is shaped again for the final width. The line
     layout cache of gpui makes this free when nothing changed.
   * `gpui::text_probe` (feature `test-support` / tests only): records what each text painted
     (rows, widest row, bounds) so layouts can be asserted without pixels.
   Tests: `crates/ui_kit/src/text_layout_tests.rs`, the segment rule in `min_content::tests`, and the
   real-shell window test `crates/app/src/app_shell/tests/window/settings_layout.rs`.

`bench/patches/gpui-selachi.patch` holds both changes (one hunk set per file, `patch -p1`).
It was regenerated with `diff -u` against a pristine gpui 0.2.2 (`~/.cargo/registry/src/*/gpui-0.2.2`)
and checked: applying it to a fresh copy of that `src/` gives a tree identical (`diff -r`) to
`vendor/gpui/src`.
