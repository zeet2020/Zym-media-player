---
name: rust-gui
description: Lessons for building Rust desktop GUI apps (GPUI). Use when writing or fixing UI in a Rust GUI app — panels, overlays, text input, layout, theming, frame stutter — when embedding a media engine such as libmpv, or when packaging/releasing a Rust desktop app for Linux.
---

Lessons paid for while building Zym Media Player (GPUI 0.2 + libmpv). Each one is a bug that shipped or nearly shipped. Apply them while writing UI, not after the user finds the bug.

Embedding libmpv (or any engine with its own thread): read [LIBMPV.md](LIBMPV.md).
CI, .deb/.rpm/AppImage, tags and releases: read [PACKAGING.md](PACKAGING.md).

## Shared components fix the whole app

Every text field, panel and overlay goes through one shared function (`text_box`, `big_panel`, `close_overlays`). A bug report names one screen; fix it in the shared function so every screen gets it, and grep every caller before editing. When an opener, field or panel is hand-rolled a second time, fold it into the shared one first.

## The UI thread does nothing that waits

GPUI re-renders the whole view on every `cx.notify()` (a 60 Hz tick here), so anything slow in `render` or `tick` stalls video *and* every panel together.

- Read engine state from a cache the engine's own thread fills (observe/subscribe), never with a synchronous getter. One getter waiting on a busy engine cost 100–370 ms frames.
- Hunt per-frame loops of lookups (one call per playlist entry, every frame, even with the panel closed). Fetch the whole collection as one value on change.
- Build lists only when the panel showing them is open.
- **Measure before guessing**: wrap `render`/`tick` in a temporary timer that logs frames over 2 ms, run the real scenario, read the numbers, then remove the timer. Judge smoothness on `--release`; debug GPUI is several times slower.

## GPUI event rules

- `cx.stop_propagation()` in an element's `on_mouse_down` also cancels that element's own `on_click`. Act in `on_mouse_down` (focus, select) when the element must also block the click from reaching what's behind it.
- A panel stops **both** left and right mouse-down, or right-clicks fall through to the window's context menu.
- Drags that must keep tracking outside the element (text selection, resize handles): register `window.on_mouse_event` (move + up) inside a `canvas` paint closure while the drag is active. Root `on_mouse_move` stops firing over `.occlude()`d elements.
- Text boxes get a right-click menu of their own (Cut/Copy/Paste/Select All/Clear), never the app menu.

## One overlay at a time

Every panel, popover, dialog and card opener calls one `close_overlays()` first, then opens itself. Click-outside and Esc call the same helper. A new overlay type is one new line in that helper — hand-written "close the others" lists drift and leave stale popovers behind panels.

## Text input is a widget you build

GPUI ships no text input. A text field is only done when it has all of:

- caret as a **char** index (convert to byte index at every `String` edit; test with multi-byte text);
- click places the caret: record text origin + font while painting (`canvas` prepaint reads `window.text_style()`), then `text_system().shape_line(..).closest_index_for_x(x)` on click; draw the caret absolutely so it never shifts the text;
- selection: drag, Shift+click, double-click word, triple-click all, Shift+arrows/Home/End, Ctrl+A/C/X/V; typing and paste replace the selection;
- focus on mouse-down; Esc leaves the field.

Keep the editing core a pure function `(text, pos, anchor, key) -> (text, pos, anchor)` with a table test.

## Layout

- CSS `min-width` beats `max-width` (taffy follows it): a `min_w(560)` panel runs off a small window. Compute widths from `window.viewport_size()` and clamp to the window.
- A flex child that holds text or a row of buttons needs `min_w_0()`, or it refuses to shrink and pushes header icons out of view. Long titles get `overflow_hidden().text_ellipsis().whitespace_nowrap()`.
- Long text in a single-line box: `justify_end` + a `flex_1` spacer keeps short text left and lets long text overflow to the left, so the end stays visible.
- Grid cards: fixed outer size and `flex_none`; images inside a fixed frame (`overflow_hidden` + placeholder bg, image `size_full` + `ObjectFit::Cover`); every text line single-line, and optional lines still reserve their height.
- A button beside a 32 px input is 32 px tall with `items_center`.
- Big panels: user-resizable from an edge with a visible grip, width persisted, double-click resets.

## Theme

- Any element with a **fixed** background colour sets a fixed text colour too. Theme foreground is dark in light mode, so a fixed dark toast with theme text is unreadable.
- Check every new surface in both light and dark before calling it done; give translucent panels the same opacity in both themes.

## Surfaces each have one job

- Right-click menu: open things + actions. Quick settings: per-video adjustments. Settings panel: saved preferences. A control living in two places with different persistence (one saved, one not) is a bug.
- Labels on one screen read distinct ("NOW PLAYING" + "Manage…", not "Playlist" + "Playlists").
- Empty launch shows nothing open; when playback ends, stale titles clear.
- A missing-setup state says what to set up and offers the button to do it; it is not an error.

## Verify with real runs

- Drive the real app through scenarios with temporary env hooks (`ZYM_TEST_*`) that print state, generate test media with `ffmpeg -f lavfi -i testsrc`, then delete the hooks.
- Reproduce with the underlying tool alone before blaming the app (yt-dlp 403 failed outside the player too).
- Ask before driving the user's real mouse/keyboard; otherwise give them a three-step manual check with the exact expected result.
- Each bit of non-trivial logic leaves one small test behind (pure helpers make that cheap).
