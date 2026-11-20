# Embedding libmpv

Lessons from rendering libmpv into a GPUI window with the **software** render API (`vo=libmpv`, `MPV_RENDER_API_TYPE_SW`). Part of the [`rust-gui`](SKILL.md) skill.

## Threads and state

- `mpv_get_property` waits for mpv's core. While the core is busy (opening a stream, switching YouTube formats, heavy decode) a read from the UI thread blocks the whole window. Observe every property the UI reads per frame (`mpv_observe_property`, format STRING) on the event thread into a shared cache; getters consult the cache first. Node properties such as `playlist` arrive as JSON in STRING format — one observed value replaces N `playlist/i/title` reads.
- One-shot reads (menus, popovers opened by a click) may stay synchronous.
- Per-file follow-ups (subtitles, seek, jump to playlist index) go in an "after load" slot applied on `MPV_EVENT_FILE_LOADED`. `loadfile x.m3u` expands the playlist on load, so "play saved playlist from item N" = load the .m3u, then `playlist-play-index N` on FILE_LOADED.

## Software renderer limits

- The sw renderer ignores rotation, yet libmpv advertises `VO_CAP_ROTATE90` for every backend, so mpv passes rotated crop rects and aborts (`mp_image_crop: Assertion x1 <= img->w`). Set `video-rotate=no` at init and rotate with a labelled filter (`vf add @rot:lavfi=[transpose=clock]`, `hflip,vflip`, `transpose=cclock`), adding the file's own rotation from `current-tracks/video/demux-rotation` on FILE_LOADED. Filters change `dwidth`/`dheight`, so the renderer gets the right size.
- Render at the window's pixel size, not the video's: a 4K frame drawn into an 1100 px window wastes ~33 MB per frame.

## Hardware decoding

- `hwdec=auto-copy-safe` still tries Vulkan first on NVIDIA. On old cards (GTX 970, driver 580) Vulkan decode faults the GPU copy engine (kernel `NVRM: Xid 31 … ENGINE CE2 … FAULT_PTE`, or `VK_ERROR_DEVICE_LOST`). Give an explicit priority list instead: `nvdec-copy,vaapi-copy,d3d11va-copy,videotoolbox-copy` (CPU fallback is automatic).
- `Device does not support decoding vp9` / `av1` lines are mpv probing decoders before falling back to CPU — noise, not the failure.
- Expose hardware decoding as one saved setting; a second unsaved toggle elsewhere disagrees with it.

## Playback state

- `keep-open=yes` holds the last frame at the end, so `idle-active` stays false; use `eof-reached` to know playback finished (clear the title there).
- `Stop` that clears the playlist: remember `playlist-pos` first if Play should resume there.

## Streams from plugins or users

- Pass headers, user-agent and titles through the **property** API (`http-header-fields` as a string list), never by joining text into an option string; copy plugin fields one by one and allow only network URL schemes (no `file://`, `edl://`, `lavfi://`).
- YouTube via `ytdl_hook` depends on the `yt-dlp` found on PATH being current and on a JS runtime (deno). On a 403, run `yt-dlp -f <format> <url>` alone first: if it fails there, the fix is updating yt-dlp, not the player.
