-- Kumi's changes to a device's settings (M4): a preset chosen, preset data written, the device's name and whether it shows
-- maximized; and the Doofer, Splitter and Notepad edits Kumi's host composes as preset data (crates/daw-mcp-server/src/
-- presets.rs), which this tool only writes. A device is found by its ref when a change is made, and by its place when the
-- change is put back: its track carried through the layout (the number under `track`), the device followed through that
-- track's device log (places.lua). So an undo outlives the devices made, deleted and moved since, here or elsewhere.
--
-- Renoise 3.5.4 (lab): preset data of another device type crashes Renoise, as does a Doofer macro mapping to chain -1; text
-- that isn't XML, an empty text and a newer document version are refused with an error and change nothing. Writing preset
-- data is one undo step; `active_preset` reads 0 after it.

local import = ...
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")
local material_ = import("changes_material")
local devices_ = import("changes_devices")

local bad, fail, q = base.bad, base.fail, base.q
local M = {}

-- The most preset data a change writes or keeps (a plug-in's state can be megabytes).
M.MAX_PRESET_BYTES = base.MAX_MATERIAL_BYTES
-- The most parameters of one device watched for the producer's undo of a preset change.
M.MOST_WATCHED = 64

-- A text's print: two 32-bit sums of its bytes and its length. (Kumi's host computes the same: presets.rs `print_of`.)
function M.print_of(text)
  local a, b = 5381, 0
  for i = 1, #text do
    local c = text:byte(i)
    a = (a * 33 + c) % 4294967296
    b = (b * 65599 + c) % 4294967296
  end
  return string.format("%08x%08x:%d", a, b, #text)
end

-- Preset data with what moves by itself left out: parameter values (automation, meta devices and macros move them all the
-- time) and plug-in state chunks (which follow the plug-in's parameters). Two reads of a device nobody edited give the same
-- shape. (presets.rs `shape_of` is the same.)
function M.shape_of(xml)
  local shape = xml:gsub("<Value>[^<]*</Value>", "<Value/>"):gsub("<ParameterChunk>.-</ParameterChunk>", "<ParameterChunk/>")
  return M.print_of(shape)
end

-- What preset data says it is: its slot's device type, its document version, and a plug-in's identifier.
local function preset_kind(xml)
  if type(xml) ~= "string" then xml = "" end
  return {
    slot = xml:match('<DeviceSlot%s+type="([%w_]+)"'), version = tonumber(xml:match('<FilterDevicePreset%s+doc_version="(%d+)"')),
    plugin = xml:match("<PluginIdentifier>([^<]*)</PluginIdentifier>"),
  }
end

-- Preset data that may go to this device: the same device type (another crashes Renoise), not a newer document, the same
-- plug-in, within the bound. Refused before anything is written.
local function check_preset_data(device, xml, label)
  if type(xml) ~= "string" or xml == "" then bad("presetData: the device's preset data (XML, as Renoise gives it)") end
  if #xml > M.MAX_PRESET_BYTES then
    bad(string.format("presetData is %.1f MB, more than Kumi writes (%d MB)", #xml / 1048576, M.MAX_PRESET_BYTES / 1048576))
  end
  local now, given = preset_kind(device.active_preset_data), preset_kind(xml)
  if not given.slot or not given.version then
    bad("presetData isn't a device preset (<FilterDevicePreset doc_version=…><DeviceSlot type=…>)")
  end
  if now.slot and given.slot ~= now.slot then
    bad(string.format("that preset data is for a %s, and %s is a %s: Renoise would crash on it", given.slot, q(label), now.slot))
  end
  if now.version and given.version > now.version then
    bad(string.format("that preset data is of a newer Renoise (document version %d; this one reads %d)", given.version, now.version))
  end
  if now.plugin and given.plugin ~= now.plugin then
    bad(string.format("that preset data is for the plug-in %s, and %s is %s", tostring(given.plugin), q(label), now.plugin))
  end
end

-- The device a change names: its ref (a device, not the track's mixer device, which is its fader and pan).
local function device_by_ref(song, env, reference, what)
  if type(reference) ~= "string" then bad(what .. ": ref must be a device ref") end
  local numbers = refs.expect(reference, "device", 2, env.epoch())
  local t, d = numbers[1], numbers[2]
  local track = base.track_at(song, t)
  if d < 1 or d > #track.devices then fail("gone", "that device isn't on the track any more") end
  if d == 1 then bad("a track's first device is its mixer device: change its volume and pan with track-volume and track-pan") end
  return t, d, track, track:device(d)
end
M.device_by_ref = device_by_ref

-- The device a record keeps, where it is now: on the track its `track` number was carried to, at its place followed through
-- that track's device log. A gap in the log refuses `stale`, a device deleted `gone`, another one at its place `changed`;
-- nothing is written then.
local function device_by_place(song, env, kept, what)
  if type(kept) ~= "table" or type(kept.track) ~= "number" or type(kept.dev) ~= "table" then bad("material: where the device was") end
  local label = type(kept.label) == "string" and kept.label or "the device"
  local t = kept.track
  if t < 1 or t > rows.track_count(song) then fail("gone", string.format("the track %s was on isn't in the song any more", q(label))) end
  local track = song:track(t)
  local d, why = material_.device_now(song, env, t, kept.dev)
  if not d then
    if why == "gone" then fail("gone", string.format("%s was deleted from %s since", q(label), q(base.named(t, track)))) end
    if why == "changed" then fail("changed", string.format("the device at %s's place on %s is another one now", q(label), q(base.named(t, track)))) end
    fail("stale", string.format("the devices on %s changed in a way Kumi couldn't follow since %s, so it can't tell where %s is: nothing was written",
      q(base.named(t, track)), what or "then", q(label)))
  end
  return t, d, track, track:device(d)
end
M.device_by_place = device_by_place

local function label_of(t, track, device)
  return devices_.label(device) .. " on " .. base.named(t, track)
end

local function identity(track, device) return base.track_id(track) .. "|" .. device.device_path end

-- Where a record finds the device again, and what it was called then (for the words of a refusal).
local function keep(env, t, d, track, device)
  return { track = t, dev = material_.device_place(env, t, d, device), label = devices_.label(device) }
end
M.keep = keep

-- (a change's target: the device's track and place, as numbers: refs of devices go stale on any device change)
local function target_of(t, d) return { kind = "device", track = t, device = d } end

-- The producer's undo of a preset change moves the device's parameters: those are watched for it (the first ones).
local function watch_all(env, t, d, device)
  if not env.watch_param then return end
  for k = 1, math.min(#device.parameters, M.MOST_WATCHED) do env.watch_param(t, d, k) end
end

local function reread(song, place)
  if place.track > rows.track_count(song) then return nil end
  local track = song:track(place.track)
  if place.device > #track.devices then return nil end
  return track:device(place.device)
end

-- device-preset: one of the device's presets (its own or the user library's), by number or name. The parameters it replaced
-- are kept as preset data, and the undo writes them back.

local function preset_names(device)
  local names = {}
  for k = 1, #device.presets do names[k] = device:preset(k) end
  return names
end

local function preset_number(device, wanted)
  local names = preset_names(device)
  if #names == 0 then bad("this device has no presets") end
  if type(wanted) == "number" then
    if wanted ~= math.floor(wanted) or wanted < 1 or wanted > #names then bad(string.format("preset is a number from 1 to %d", #names)) end
    return wanted, names[wanted]
  end
  if type(wanted) ~= "string" or wanted == "" then bad("preset: a preset's number (from 1) or its name") end
  local lower = wanted:lower()
  for _, exact in ipairs({ true, false }) do
    local found
    for k, name in ipairs(names) do
      local n = name:lower()
      -- (a name as written, or its last part: "Dynamics/Hard Clipper" is "Hard Clipper" too, or the start of one)
      if (exact and (n == lower or n:match("([^/]+)$") == lower)) or (not exact and (n:sub(1, #lower) == lower or (n:match("([^/]+)$") or ""):sub(1, #lower) == lower)) then
        if found then bad(string.format("%q names more than one preset of this device: give its number", wanted)) end
        found = k
      end
    end
    if found then return found, names[found] end
  end
  local listed = {}
  for k = 1, math.min(#names, 24) do listed[k] = names[k] end
  bad(string.format("this device has no preset %q (it has %s%s)", wanted, table.concat(listed, ", "), #names > 24 and ", ..." or ""))
end

local function preset_name(device, n)
  if type(n) ~= "number" or n < 1 or n > #device.presets then return nil end
  return device:preset(n)
end

local function preset_preview(song, env, input)
  local t, d, track, device = device_by_ref(song, env, input.ref, "device-preset")
  local n, name = preset_number(device, input.preset)
  local xml = device.active_preset_data
  local label = label_of(t, track, device)
  return {
    target = target_of(t, d), prior = { preset = device.active_preset, print = M.print_of(xml) },
    fence = { id = identity(track, device), state = material_.device_state(device).state, preset = device.active_preset },
    display = {
      title = string.format("%s: preset %s", label, q(name)),
      before = { preset = preset_name(device, device.active_preset) }, after = { preset = name },
      note = "Kumi keeps the settings it replaces to put back.",
    },
  }
end

local function preset_apply(song, env, input, fence, desc)
  local ok, t, d, track, device = pcall(device_by_ref, song, env, input.ref, "device-preset")
  if not ok then base.gone_is_changed(t) end
  local label = label_of(t, track, device)
  if fence.id ~= identity(track, device) then fail("changed", base.moved_text(label)) end
  if material_.device_state(device).state ~= fence.state or device.active_preset ~= fence.preset then
    fail("changed", label .. "'s settings changed since the preview")
  end
  local n, name = preset_number(device, input.preset)
  local xml = device.active_preset_data
  if #xml > M.MAX_PRESET_BYTES then
    fail("failed", string.format("%s holds %.1f MB of settings, more than Kumi keeps to put back, so it won't replace them", label, #xml / 1048576))
  end
  song:describe_undo(desc)
  watch_all(env, t, d, device)
  device.active_preset = n
  if device.active_preset ~= n then
    pcall(function() device.active_preset_data = xml end)
    fail("failed", "Renoise didn't take the preset " .. q(name))
  end
  local back = device.active_preset_data
  return {
    applied = { preset = n, state = material_.device_state(device).state, print = M.print_of(back) },
    material = (function()
      local kept = keep(env, t, d, track, device)
      kept.presetData = xml
      return kept
    end)(),
    target = target_of(t, d),
    display = { title = string.format("%s: preset %s", label, q(name)), before = { preset = preset_name(device, fence.preset) }, after = { preset = name } },
    undo = { desc = desc, entry = true },
    prior = { print = M.print_of(xml) }, journal_applied = { print = M.print_of(back) }, place = { track = t, device = d },
  }
end

-- Puts the kept settings back, if the device still has what the change left (`applied.state`).
local function settings_restore(kind, song, env, restore, desc, what)
  local material, applied, prior = restore.material, restore.applied, restore.prior
  if type(material) ~= "table" or type(material.presetData) ~= "string" then bad("material: the settings the change replaced") end
  if type(applied) ~= "table" or type(prior) ~= "table" then bad("prior and applied: the change's settings") end
  local t, d, track, device = device_by_place(song, env, material, "Kumi changed it")
  local label = label_of(t, track, device)
  local now = device.active_preset_data
  if M.print_of(now) == prior.print then
    return { restored = { preset = device.active_preset }, display = { title = label .. ": its settings are back already" }, undo = { desc = desc, entry = false }, unwritten = true }
  end
  if not restore.discard and material_.device_state(device).state ~= applied.state then
    fail("changed", string.format("%s's settings changed in Renoise since Kumi %s (automation and meta devices move them too)", label, what))
  end
  song:describe_undo(desc)
  watch_all(env, t, d, device)
  local ok, err = pcall(function() device.active_preset_data = material.presetData end)
  if not ok then fail("failed", "Renoise wouldn't take the settings back: " .. tostring(err)) end
  base.note_restored(env.journal, kind, { track = t, device = d })
  return {
    restored = { preset = device.active_preset }, display = { title = label .. ": settings put back" }, undo = { desc = desc, entry = true },
  }
end

local function settings_read(device) return { print = M.print_of(device.active_preset_data) } end
local function same_print(a, b) return a.print == b.print end

base.register({
  kind = "device-preset", custom = true, places = true,
  preview = preset_preview, apply = preset_apply,
  restore = function(song, env, restore, desc) return settings_restore("device-preset", song, env, restore, desc, "chose the preset") end,
  reread = reread, read = settings_read, same = same_print,
})

-- device-preset-data: the device's preset data written whole (XML, as Renoise gives it and as a read shows it).

local function data_preview(song, env, input)
  local t, d, track, device = device_by_ref(song, env, input.ref, "device-preset-data")
  local label = label_of(t, track, device)
  check_preset_data(device, input.presetData, label)
  local xml = device.active_preset_data
  return {
    target = target_of(t, d), prior = { preset = device.active_preset, print = M.print_of(xml) },
    fence = { id = identity(track, device), state = material_.device_state(device).state },
    display = {
      title = "Wrote the settings of " .. q(label), unchanged = xml == input.presetData or nil,
      note = "Kumi keeps the settings it replaces to put back.",
    },
  }
end

local function data_apply(song, env, input, fence, desc)
  local ok, t, d, track, device = pcall(device_by_ref, song, env, input.ref, "device-preset-data")
  if not ok then base.gone_is_changed(t) end
  local label = label_of(t, track, device)
  if fence.id ~= identity(track, device) then fail("changed", base.moved_text(label)) end
  if material_.device_state(device).state ~= fence.state then fail("changed", label .. "'s settings changed since the preview") end
  check_preset_data(device, input.presetData, label)
  local xml = device.active_preset_data
  if #xml > M.MAX_PRESET_BYTES then
    fail("failed", string.format("%s holds %.1f MB of settings, more than Kumi keeps to put back, so it won't replace them", label, #xml / 1048576))
  end
  song:describe_undo(desc)
  watch_all(env, t, d, device)
  local written, err = pcall(function() device.active_preset_data = input.presetData end)
  -- (Renoise raises before it writes anything: text that isn't XML, an empty one, a newer version)
  if not written then fail("failed", "Renoise refused the preset data: " .. tostring(err):gsub("^[^%s:]*%.lua:%d+: ", "")) end
  local back = device.active_preset_data
  local kept = keep(env, t, d, track, device)
  kept.presetData = xml
  return {
    applied = { state = material_.device_state(device).state, print = M.print_of(back) }, material = kept, target = target_of(t, d),
    display = { title = "Wrote the settings of " .. q(label) }, undo = { desc = desc, entry = true },
    prior = { print = M.print_of(xml) }, journal_applied = { print = M.print_of(back) }, place = { track = t, device = d },
  }
end

base.register({
  kind = "device-preset-data", custom = true, places = true,
  preview = data_preview, apply = data_apply,
  restore = function(song, env, restore, desc) return settings_restore("device-preset-data", song, env, restore, desc, "wrote them") end,
  reread = reread, read = settings_read, same = same_print,
})

-- device-set: the device's name in the chain (its display name; "" gives it back its own) and whether it shows maximized.

local function set_read(device, source)
  local out = {}
  if source == nil or source.name ~= nil then out.name = device.display_name end
  if source == nil or source.maximized ~= nil then out.maximized = device.is_maximized end
  return out
end

local function set_wanted(device, input)
  local out = {}
  if input.name ~= nil then
    if type(input.name) ~= "string" or #input.name > 256 or input.name:find("[%c]") then bad("name must be one line of text") end
    out.name = input.name
  end
  if input.maximized ~= nil then
    if type(input.maximized) ~= "boolean" then bad("maximized must be true or false") end
    out.maximized = input.maximized
  end
  if next(out) == nil then bad("give name or maximized") end
  return out
end

-- (a name given back as "" reads as the device's own name: either is what was asked)
local function set_same(device_name)
  return function(a, b)
    if (a.maximized == nil) ~= (b.maximized == nil) or a.maximized ~= b.maximized then return false end
    if (a.name == nil) ~= (b.name == nil) then return false end
    if a.name == b.name then return true end
    local function plain(n) return (n == "" or n == device_name) and "" or n end
    return device_name ~= nil and plain(a.name) == plain(b.name)
  end
end

local function set_title(label, track_name, from, to)
  local parts = {}
  if to.name ~= nil and to.name ~= from.name then
    local new = to.name == "" and "its own name" or q(to.name)
    parts[#parts + 1] = string.format("Renamed %s on %s → %s", q(label), q(track_name), new)
  end
  if to.maximized ~= nil and to.maximized ~= from.maximized then
    local word = to.maximized and "expanded" or "collapsed"
    if #parts == 0 then return string.format("%s on %s %s", label, track_name, word) end
    parts[#parts + 1] = word
  end
  if #parts == 0 then return string.format("%s on %s as it was", label, track_name) end
  return table.concat(parts, ", ")
end

local function set_write(device, state)
  if state.name ~= nil then device.display_name = state.name end
  if state.maximized ~= nil then device.is_maximized = state.maximized end
end

local function set_preview(song, env, input)
  local t, d, track, device = device_by_ref(song, env, input.ref, "device-set")
  local wanted = set_wanted(device, input)
  local now = set_read(device, wanted)
  local display = { title = set_title(devices_.label(device), base.named(t, track), now, wanted), before = now, after = wanted }
  if set_same(device.name)(now, wanted) then display.unchanged = true end
  local fence = set_read(device, wanted)
  fence.id = identity(track, device)
  return { target = target_of(t, d), prior = now, fence = fence, display = display }
end

local function set_apply(song, env, input, fence, desc)
  local ok, t, d, track, device = pcall(device_by_ref, song, env, input.ref, "device-set")
  if not ok then base.gone_is_changed(t) end
  local label, track_name = devices_.label(device), base.named(t, track)
  if fence.id ~= identity(track, device) then fail("changed", base.moved_text(label .. " on " .. track_name)) end
  local wanted = set_wanted(device, input)
  local now = set_read(device, wanted)
  local fenced = {}
  for k, v in pairs(fence) do if k ~= "id" then fenced[k] = v end end
  if not set_same(device.name)(now, fenced) then fail("changed", string.format("%s on %s changed since the preview", label, track_name)) end
  local same = set_same(device.name)
  if not same(now, wanted) then
    song:describe_undo(desc)
    set_write(device, wanted)
  end
  local after = set_read(device, wanted)
  if not same(after, wanted) then
    pcall(set_write, device, now)
    fail("failed", string.format("Renoise kept %s on %s as it was", label, track_name))
  end
  return {
    applied = after, material = keep(env, t, d, track, device), target = target_of(t, d),
    display = { title = set_title(label, track_name, now, after), before = now, after = after },
    undo = { desc = desc, entry = not same(now, wanted) }, prior = now, place = { track = t, device = d },
  }
end

local function set_restore(song, env, restore, desc)
  local prior, applied = restore.prior, restore.applied
  if type(prior) ~= "table" or type(applied) ~= "table" then bad("prior and applied: the name and look the change set") end
  local t, d, track, device = device_by_place(song, env, restore.material, "Kumi changed it")
  local label, track_name = devices_.label(device), base.named(t, track)
  local same = set_same(device.name)
  local now = set_read(device, prior)
  if same(now, prior) then
    return { restored = now, display = { title = set_title(label, track_name, prior, prior) .. " (back already)" }, undo = { desc = desc, entry = false }, unwritten = true }
  end
  if not restore.discard and not same(now, applied) then
    fail("changed", string.format("%s on %s was renamed or changed in Renoise since Kumi set it", label, track_name))
  end
  song:describe_undo(desc)
  set_write(device, prior)
  base.note_restored(env.journal, "device-set", { track = t, device = d })
  return {
    restored = set_read(device, prior), display = { title = set_title(label, track_name, now, prior) .. " (put back)" },
    undo = { desc = desc, entry = true },
  }
end

base.register({
  kind = "device-set", custom = true, places = true,
  preview = set_preview, apply = set_apply, restore = set_restore,
  reread = reread, read = function(device, source) return set_read(device, source) end,
  same = function(a, b) return set_same(nil)(a, b) end,
})

-- The Doofer, Splitter and Notepad edits Kumi's host composes (presets.rs): the host reads the device's preset data
-- (`preset_read`), edits it, and sends it whole with the shape it edited from; this writes it in one step if the device's
-- shape is still that (nothing structural changed in between), and puts the host's restore back the same way. The words
-- are the host's. Only containers (and the Notepad) take them, and only preset data of their own type.

local XML_KINDS = {
  { kind = "doofer-macro", devices = "container" },
  { kind = "doofer-device-add", devices = "container" },
  { kind = "doofer-device-delete", devices = "container" },
  { kind = "doofer-device-move", devices = "container" },
  { kind = "doofer-device-set", devices = "container" },
  { kind = "splitter-set", devices = "splitter" },
  { kind = "notepad-write", devices = "notepad" },
}

local CONTAINERS = { ["Audio/Effects/Native/Doofer"] = true, ["Audio/Effects/Native/Splitter"] = true }
local function takes(spec, device)
  local path = device.device_path
  if spec.devices == "container" then return CONTAINERS[path] == true end
  if spec.devices == "splitter" then return path == "Audio/Effects/Native/Splitter" end
  if spec.devices == "notepad" then return path == "Audio/Effects/Native/Notepad" or CONTAINERS[path] == true end
  return false
end

local function display_from(given, fallback)
  if type(given) == "table" and type(given.title) == "string" and given.title ~= "" then return given end
  return { title = fallback }
end

for _, spec in ipairs(XML_KINDS) do
  local kind = spec.kind
  local function write_checked(song, env, t, d, track, device, xml, shape, desc)
    local label = label_of(t, track, device)
    if not takes(spec, device) then bad(string.format("%s isn't a device %s changes", q(label), kind)) end
    check_preset_data(device, xml, label)
    if type(shape) ~= "string" or M.shape_of(device.active_preset_data) ~= shape then
      fail("changed", string.format("%s changed in Renoise since Kumi read it: read it again", q(label)))
    end
    song:describe_undo(desc)
    watch_all(env, t, d, device)
    local ok, err = pcall(function() device.active_preset_data = xml end)
    if not ok then fail("failed", "Renoise refused the preset data: " .. tostring(err):gsub("^[^%s:]*%.lua:%d+: ", "")) end
    return device.active_preset_data
  end
  base.register({
    kind = kind, custom = true, places = true, host_made = true,
    preview = function()
      bad(kind .. " is made by Kumi's host from the device's preset data: ask it through the host's change_preview")
    end,
    apply = function(song, env, input, fence, desc)
      local ok, t, d, track, device = pcall(device_by_ref, song, env, input.ref, kind)
      if not ok then base.gone_is_changed(t) end
      local label = label_of(t, track, device)
      if fence.id ~= nil and fence.id ~= identity(track, device) then fail("changed", base.moved_text(label)) end
      local before = M.shape_of(device.active_preset_data)
      local back = write_checked(song, env, t, d, track, device, input.presetData, input.shape, desc)
      return {
        applied = { shape = M.shape_of(back), edit = input.edit }, material = keep(env, t, d, track, device), target = target_of(t, d),
        display = display_from(input.display, "Changed " .. q(label)), undo = { desc = desc, entry = true },
        prior = { shape = before }, journal_applied = { shape = M.shape_of(back) }, place = { track = t, device = d },
      }
    end,
    restore = function(song, env, restore, desc)
      local material = restore.material
      if type(material) ~= "table" or type(material.write) ~= "table" then bad("material.write: the preset data Kumi's host put back") end
      local t, d, track, device = device_by_place(song, env, material, "Kumi changed it")
      local back = write_checked(song, env, t, d, track, device, material.write.presetData, material.write.shape, desc)
      base.note_restored(env.journal, kind, { track = t, device = d })
      return {
        restored = { shape = M.shape_of(back) }, display = display_from(material.write.display, q(label_of(t, track, device)) .. " put back"),
        undo = { desc = desc, entry = true },
      }
    end,
    reread = reread,
    read = function(device) return { shape = M.shape_of(device.active_preset_data) } end,
    same = function(a, b) return a.shape == b.shape end,
  })
end

-- preset_read {ref | at, epoch, layout}: a device's preset data and what Kumi's host needs to edit it (its shape, its
-- parameters, where a record finds it again). `at` is a record's place (`material`), for an undo.
function M.read(params, env)
  local song = base.context(params, env)
  local t, d, track, device
  if params.ref ~= nil then
    t, d, track, device = device_by_ref(song, env, params.ref, "preset_read")
  elseif type(params.at) == "table" then
    t, d, track, device = device_by_place(song, env, params.at, "Kumi changed it")
  else
    bad("ref (a device) or at (where a change left it)")
  end
  local xml = device.active_preset_data
  if #xml > M.MAX_PRESET_BYTES then
    fail("failed", string.format("%s holds %.1f MB of settings, more than Kumi edits", q(label_of(t, track, device)), #xml / 1048576))
  end
  local parameters = {}
  for k = 1, math.min(#device.parameters, M.MOST_WATCHED) do
    local parameter = device:parameter(k)
    parameters[k] = { name = parameter.name, value = parameter.value, valueString = parameter.value_string, min = parameter.value_min, max = parameter.value_max }
  end
  -- (the device paths asked about that the device's track can take: Audio Units the host would put inside a Doofer)
  local available
  if type(params.paths) == "table" then
    available = {}
    local wanted = {}
    for i = 1, math.min(#params.paths, 16) do
      if type(params.paths[i]) == "string" then wanted[params.paths[i]] = true; available[params.paths[i]] = false end
    end
    for _, info in ipairs(track.available_device_infos) do
      if wanted[info.path] then available[info.path] = true end
    end
  end
  return {
    available = available,
    epoch = env.epoch(), layout = env.layout(), ref = refs.make(env.epoch(), "device", t, d), target = target_of(t, d),
    at = keep(env, t, d, track, device), id = identity(track, device), path = device.device_path, name = device.name,
    label = devices_.label(device), trackName = base.named(t, track), active = device.is_active, presetData = xml,
    shape = M.shape_of(xml), print = M.print_of(xml), parameters = parameters,
  }
end

return M
