-- Example plugin. Any mpv Lua/JS script works: https://mpv.io/manual/master/#lua-scripting
-- Install: copy into ~/.config/zym-player/plugins/ (Windows: %APPDATA%\zym-player\plugins\)

-- Press "i" for file info.
mp.add_key_binding("i", "show-info", function()
    local w, h = mp.get_property("width", "?"), mp.get_property("height", "?")
    local codec = mp.get_property("video-codec", "?")
    local pos = mp.get_property_number("playlist-pos-1", 0)
    local count = mp.get_property_number("playlist-count", 0)
    mp.osd_message(string.format("%sx%s  %s\nItem %d of %d", w, h, codec, pos, count), 4)
end)
