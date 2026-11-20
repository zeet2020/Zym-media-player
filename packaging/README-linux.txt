Zym Media Player (Linux x86_64)

Run: ./zym-media-player [FILE...]

Runtime requirement: libmpv (libmpv.so.2), plus libxkbcommon, libxcb and a
Vulkan driver.
  Ubuntu/Debian: sudo apt install libmpv2 libvulkan1
  Fedora:        sudo dnf install mpv-libs vulkan-loader

The .deb / .rpm packages on the release page pull these in automatically.
The .AppImage bundles libmpv; it needs only a Vulkan driver (glibc 2.39+, e.g. Ubuntu 24.04+).
