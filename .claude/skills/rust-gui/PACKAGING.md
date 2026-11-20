# Packaging and releasing a Rust desktop app (Linux focus)

Part of the [`rust-gui`](SKILL.md) skill.

## glibc decides where a binary runs

- A binary needs a glibc at least as new as the one it was built against; older-built binaries run on newer systems. Build Linux packages on the **oldest** supported distro and pin the runner (`ubuntu-24.04`), because `ubuntu-latest` moves (it became 26.04 and the .deb silently required glibc 2.43, uninstallable on 24.04).
- After every Linux build, read what the package demands: `dpkg-deb -f <pkg>.deb Depends`. `libc6 (>= X)` is the floor you shipped.
- One build on the oldest distro covers newer ones as long as library package names hold (libmpv2 exists on 24.04 and 26.04). Per-distro .debs are only needed when they don't.
- Going older is gated by libraries, not glibc: Ubuntu 22.04 has mpv 0.34 (`libmpv.so.1`), and libmpv2-sys needs mpv ≥ 0.35. Covering it means an AppImage built on 22.04 with mpv compiled from source.

## AppImage

- linuxdeploy, pinned by release tag **and** SHA-256 in CI; run with `APPIMAGE_EXTRACT_AND_RUN=1` (no FUSE on runners); set `OUTPUT` for the file name.
- Lay out `AppDir/usr/bin/<app>` and put bundled data where the app already looks relative to its exe (`<exe>/../share/<app>/…`), so .deb and AppImage share one lookup.
- linuxdeploy bundles libmpv and its libraries; glibc, GL and dlopen()ed Vulkan/Wayland come from the host. Inspect the result without running it: `7z l <file>.AppImage`.
- Snap/Flatpak cost far more (Flatpak: libmpv from source, vendored cargo deps) and pay off mainly through their stores; ship .deb + AppImage first.

## Renaming the product

Display name (window title, About, `.desktop` Name, installer names, README, package descriptions) can change freely. Renaming the **binary/package** too (the process name users see) means one pass over every place the name is an identifier, plus an upgrade path:

- Cargo package name (binary, Cargo.lock), app id + `.desktop` file name/`Exec`/`Icon`/`StartupWMClass`, icon file names, `/usr/share/<name>/` data paths and the code that looks there, CI paths and artifact names, installer exe names.
- Upgrades: .deb `conflicts`/`replaces`/`provides` = old name; rpm `[package.metadata.generate-rpm.obsoletes]`; Inno `[InstallDelete]` for the old exe and delete-flagged registry entries for old keys. Keep the Inno `AppId`.
- Keep the **config directory** name so users keep settings, playlists and plugins.
- Regenerate files that embed the name (license notices) and confirm the CI gate that diffs them passes; build a real .deb and read `dpkg-deb -f`/`-c`.
- Linux shows only 15 characters of a process name in `ps`/`top` (`zym-media-playe`); full command lines show all of it.

## Tags and releases

- The release workflow fires on `v*` tags. Check the last run (`gh run list`) and whether a release exists (`gh release view`) before moving a tag.
- Moving a published tag means force-pushing and rebuilding over files people may have downloaded; once a release is public, prefer the next patch version. Force-push only on the user's explicit say-so.
- Test workflow changes with `gh workflow run <wf> --ref <branch>` (artifacts only, no release) before tagging.
