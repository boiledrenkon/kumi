-- Kumi's changes to the devices on a track: bypass and parameter values (and, below, putting a device in, taking it out and
-- moving it).

local import = ...
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")
local material_ = import("changes_material")
local json = import("json")

local bad, fail = base.bad, base.fail
local q = base.q
local M = {}

-- A device is found by its ref (`E:device:track.device`), a parameter by `E:param:track.device.parameter`; both are looked at
-- again by their place. The track's first device is its mixer device, which is the track's fader and pan: not changed here.
-- A device's name as the producer sees it: the name they gave it, or the device's own.
function M.label(device)
  return device.display_name ~= "" and device.display_name or device.name
end

local function device_of(song, env, reference, what, count)
  if type(reference) ~= "string" then bad(what .. ": ref must be a " .. (count == 3 and "parameter" or "device") .. " ref") end
  local numbers = refs.expect(reference, count == 3 and "param" or "device", count, env.epoch())
  local index, at = numbers[1], numbers[2]
  local track = base.track_at(song, index)
  if at < 1 or at > #track.devices then fail("gone", "that device isn't on the track any more") end
  if at == 1 then
    if what == "device-delete" or what == "device-move" then
      bad("a track's first device is its mixer device (its fader and pan): it stays where it is")
    end
    bad("a track's first device is its mixer device: change its volume and pan with track-volume and track-pan")
  end
  local device = track:device(at)
  return index, at, track, device, M.label(device) .. " on " .. base.named(index, track), numbers[3]
end

local function device_target(env, index, at)
  return { ref = refs.make(env.epoch(), "device", index, at), kind = "device" }
end

local function device_prop(spec)
  spec.locate = function(song, env, input)
    local index, at, track, device, name = device_of(song, env, input.ref, spec.kind, 2)
    return {
      object = device, name = name, place = { track = index, device = at }, target = device_target(env, index, at),
      id = base.track_id(track) .. "|" .. device.device_path,
    }
  end
  spec.reread = function(song, place)
    if place.track > rows.track_count(song) then return nil end
    local track = song:track(place.track)
    if place.device > #track.devices then return nil end
    return track:device(place.device)
  end
  base.register(spec)
end

device_prop {
  kind = "device-bypass", noun = "bypass", undoable = true,
  read = function(device) return { bypass = not device.is_active } end,
  wanted = function(_, input)
    if type(input.bypass) ~= "boolean" then bad("bypass must be true or false") end
    return { bypass = input.bypass }
  end,
  write = function(device, state) device.is_active = not state.bypass end,
  same = function(a, b) return a.bypass == b.bypass end,
  title = function(name, _, to) return name .. (to.bypass and " bypassed" or " switched on") end,
}

-- A parameter's value is in its own range; text is parsed by Renoise. A Receiver of the #Send device is a send track's number.
local function param_state(parameter)
  return { value = parameter.value, valueString = parameter.value_string }
end

local function receiver_check(device, parameter, value, song)
  if device.device_path:find("#Send", 1, true) and parameter.name == "Receiver" then
    local sends = song.send_track_count
    if value ~= math.floor(value) or value < 0 or value >= sends then
      bad(string.format("Receiver is a send track's number from 0 to %d (there are %d send tracks)", sends - 1, sends))
    end
  end
end

-- A parameter whose value is a place: a #Send's Receiver (a send track, counted among the sends from 0) and a meta device's
-- track (Dest. Track and the like: a track counted from 0). Renoise moves these values with the tracks made and deleted, so
-- a record keeps the track the value names as a track ref, which the layout carries; the value is worked out from it again.
local function points_at(device, parameter)
  local path, name = device.device_path, parameter.name
  if (path:find("#Send", 1, true) or path:find("#Multiband Send", 1, true)) and name:match("^Receiver") then return "send" end
  if path:find("/Native/*", 1, true) and name:match("Track$") then return "track" end
  return nil
end

local function master_of(song) return song.sequencer_track_count + 1 end

local function track_of_value(song, points, value)
  if type(value) ~= "number" or value < 0 or value ~= math.floor(value) then return nil end
  local n = points == "send" and master_of(song) + 1 + value or value + 1
  if n < 1 or n > rows.track_count(song) then return nil end
  return n
end

local function value_of_track(song, points, n)
  return points == "send" and n - master_of(song) - 1 or n - 1
end

local param_spec = {
  kind = "device-param", noun = "parameter", undoable = true,
  wire = function(state, env, loc)
    if not (loc and loc.points) or type(state) ~= "table" then return state end
    local n = track_of_value(renoise.song(), loc.points, state.value)
    if not n then return state end
    local out = {}
    for k, v in pairs(state) do out[k] = v end
    out.at = refs.make(env.epoch(), "track", n)
    return out
  end,
  unwire = function(state, env, loc)
    if type(state) ~= "table" or type(state.at) ~= "string" or not (loc and loc.points) then return state end
    -- (carried to where that track is now; a ref of another epoch is `stale`)
    local n = refs.expect(state.at, "track", 1, env.epoch())[1]
    local out = {}
    for k, v in pairs(state) do out[k] = v end
    out.at, out.value, out.valueString = nil, value_of_track(renoise.song(), loc.points, n), nil
    return out
  end,
  read = function(parameter) return param_state(parameter) end,
  wanted = function(parameter, input)
    if type(input.value) == "number" then
      if input.value ~= input.value or input.value < parameter.value_min or input.value > parameter.value_max then
        bad(string.format("value must be from %s to %s", tostring(parameter.value_min), tostring(parameter.value_max)))
      end
      return { value = input.value }
    elseif type(input.valueString) == "string" then
      return { valueString = input.valueString }
    end
    bad("value (a number in the parameter's own range) or valueString (text as Renoise shows it)")
  end,
  write = function(parameter, state)
    if state.value ~= nil then
      parameter.value = state.value
    else
      parameter.value_string = state.valueString
    end
  end,
  -- (text is read by Renoise, which answers the value in its own words: "0.5" is read back as "0.500 dB", "-3 dB" as "-3.00 dB".
  -- Text matches what it was read as when it begins that, or has the same number to the digits shown and the same rest)
  same = function(a, b)
    if a.value ~= nil and b.value ~= nil then return base.close(a.value, b.value) end
    local read, wanted = a, b
    if a.value == nil then read, wanted = b, a end
    if read.value == nil or wanted.valueString == nil then return a.valueString == b.valueString end
    local function plain(text) return (tostring(text):lower():gsub("\226\136\146", "-"):gsub("^%s+", ""):gsub("%s+$", ""):gsub("%s+", " ")) end
    local text, asked = plain(read.valueString), plain(wanted.valueString)
    if text == asked or text:sub(1, #asked) == asked then return true end
    local n1, rest1 = text:match("^([%-+]?%d*%.?%d+)%s*(.*)$")
    local n2, rest2 = asked:match("^([%-+]?%d*%.?%d+)%s*(.*)$")
    if not n1 or not n2 or rest1 ~= rest2 then return false end
    local digits = #(n1:match("%.(%d+)$") or "")
    return math.abs(tonumber(n1) - tonumber(n2)) <= 0.5 * 10 ^ -digits + 1e-9
  end,
  title = function(name, _, to) return name .. " " .. (to.valueString or tostring(to.value)) end,
}

-- Which parameter of a device a request means: its number from 1, or its name (any case).
local function parameter_number(device, wanted)
  local count = #device.parameters
  if type(wanted) == "number" then
    if wanted ~= math.floor(wanted) or wanted < 1 or wanted > count then bad(string.format("param is a number from 1 to %d", count)) end
    return wanted
  end
  if type(wanted) ~= "string" or wanted == "" then bad("param: a parameter's number (from 1) or its name, with a device ref") end
  -- (the name as written, or else the start of one name: EQ 5 names its gains "Gain 1 (100 Hz)", which follow the frequency)
  local lower = wanted:lower()
  for _, exact in ipairs({ true, false }) do
    local found
    for k = 1, count do
      local name = device:parameter(k).name:lower()
      if (exact and name == lower) or (not exact and name:sub(1, #lower) == lower) then
        if found then bad(string.format("%q names more than one parameter of this device: give its number", wanted)) end
        found = k
      end
    end
    if found then return found end
  end
  local names = {}
  for k = 1, math.min(count, 24) do names[k] = device:parameter(k).name end
  bad(string.format("this device has no parameter %q (it has %s%s)", wanted, table.concat(names, ", "), count > 24 and ", ..." or ""))
end

-- A parameter is named by its ref, or by its device's ref and `param`.
param_spec.locate = function(song, env, input)
  local parsed = type(input.ref) == "string" and refs.parse(input.ref) or nil
  local index, at, track, device, name, k
  if parsed and parsed.kind == "device" then
    index, at, track, device, name = device_of(song, env, input.ref, param_spec.kind, 2)
    k = parameter_number(device, input.param)
  else
    index, at, track, device, name, k = device_of(song, env, input.ref, param_spec.kind, 3)
  end
  if k < 1 or k > #device.parameters then fail("gone", "that parameter isn't on the device any more") end
  local parameter = device:parameter(k)
  if input.value ~= nil then receiver_check(device, parameter, input.value, song) end
  return {
    object = parameter, name = name .. " " .. parameter.name, place = { track = index, device = at, param = k },
    target = { ref = refs.make(env.epoch(), "param", index, at, k), kind = "param" },
    id = base.track_id(track) .. "|" .. device.device_path .. "|" .. k, points = points_at(device, parameter),
  }
end
param_spec.reread = function(song, place)
  if place.track > rows.track_count(song) then return nil end
  local track = song:track(place.track)
  if place.device > #track.devices then return nil end
  local device = track:device(place.device)
  if place.param > #device.parameters then return nil end
  return device:parameter(place.param)
end
-- The producer's undo moves a parameter: asked of its observable, once.
param_spec.watch = function(env, loc) env.watch_param(loc.place.track, loc.place.device, loc.place.param) end
base.register(param_spec)


-- Putting a device in, taking it out, moving it. These change the track's list of devices, so the epoch moves (the answer is in
-- the new one, and says where the device is: `created` for a device made, `material.track` for the track to find it on).

-- The device Renoise has for what was asked: its path, or its name when one device has it.
local function resolve_device(track, wanted)
  if type(wanted) ~= "string" or wanted == "" then bad("path: a device's path (as Renoise lists it) or its name") end
  local lower, named_, near = wanted:lower(), {}, {}
  for _, info in ipairs(track.available_device_infos) do
    if info.path == wanted then return info end
    local last = info.path:match("([^/]+)$") or info.path
    if info.path:lower() == lower or last:lower() == lower or (info.short_name or ""):lower() == lower then
      named_[#named_ + 1] = info
    elseif info.path:lower():find(lower, 1, true) and #near < 5 then
      near[#near + 1] = info.path
    end
  end
  if #named_ == 1 then return named_[1] end
  if #named_ > 1 then
    local paths = {}
    for i = 1, math.min(#named_, 5) do paths[i] = named_[i].path end
    bad(string.format("%q names %d devices: say which by its path (%s)", wanted, #named_, table.concat(paths, ", ")))
  end
  local hint = #near > 0 and (" Close: " .. table.concat(near, ", ") .. ".") or ""
  bad(string.format("Renoise has no device %q for this track.%s", wanted, hint))
end

local function short_name(info)
  local name = info.short_name
  if name == nil or name == "" then name = info.path:match("([^/]+)$") or info.path end
  return name
end

local function sends_check(song, receiver)
  local sends = song.send_track_count
  if type(receiver) ~= "number" or receiver ~= math.floor(receiver) or receiver < 0 or receiver >= sends then
    bad(string.format("receiver is a send track's number from 0 to %d (there are %d send tracks)", sends - 1, sends))
  end
end

local function add_input(song, track, input)
  local info = resolve_device(track, input.path)
  local is_send = info.path:find("#Send", 1, true) ~= nil
  if (input.receiver ~= nil or input.amount ~= nil) and not is_send then
    bad("receiver and amount are for the #Send device")
  end
  if input.receiver ~= nil then sends_check(song, input.receiver) end
  -- (the #Send's Amount is 0 to 1: measured, 3.5.4. Checked before anything is written; apply checks the parameter's own range too)
  if input.amount ~= nil and (type(input.amount) ~= "number" or input.amount ~= input.amount or input.amount < 0 or input.amount > 1) then
    bad("amount must be a number from 0 to 1")
  end
  local last = #track.devices + 1
  local at = input.index
  if at == nil then
    at = last
  elseif type(at) ~= "number" or at ~= math.floor(at) or at < 2 or at > last then
    bad(string.format("index must be a whole number from 2 (after the mixer device) to %d", last))
  end
  return info, at
end

local function add_signature(index, track)
  return { name = base.track_id(track), index = index, devices = #track.devices }
end

local function add_preview(song, env, input)
  local index, track = base.find(song, env, input.ref, "device-add")
  local info, at = add_input(song, track, input)
  local now = add_signature(index, track)
  -- (the send the receiver names, as a track the layout carries: one made before it by the apply refuses `changed`)
  if input.receiver ~= nil then now.receiver = refs.make(env.epoch(), "track", song.sequencer_track_count + 2 + input.receiver) end
  local display = {
    title = string.format("Added %s to %s", q(short_name(info)), q(base.named(index, track))),
    before = { devices = now.devices }, after = { devices = now.devices + 1, path = info.path, index = at },
  }
  return { target = base.target(env, index), prior = { devices = now.devices }, fence = now, display = display }
end

local function parameter_named(device, name)
  for k = 1, #device.parameters do
    if device:parameter(k).name == name then return device:parameter(k) end
  end
  return nil
end

local function add_apply(song, env, input, fence, desc)
  local ok, index, track = pcall(base.find, song, env, input.ref, "device-add")
  if not ok then base.gone_is_changed(index) end
  local now = add_signature(index, track)
  if type(fence.receiver) == "string" then
    local send = refs.expect(fence.receiver, "track", 1, env.epoch())[1]
    if input.receiver ~= send - song.sequencer_track_count - 2 then fail("changed", "the send the receiver names moved since the preview: read again") end
    now.receiver = fence.receiver
  end
  if not base.same_table(now, fence) then fail("changed", base.named(index, track) .. "'s devices changed since the preview") end
  local info, at = add_input(song, track, input)
  song:describe_undo(desc)
  local made, device = pcall(function() return track:insert_device_at(info.path, at) end)
  if not made or not device then
    local why = made and "" or ": " .. tostring(device):gsub("^[^%s:]*%.lua:%d+: ", "")
    fail("failed", "Renoise wouldn't make the device " .. info.path .. why)
  end
  -- The device is set up in this callback; if that can't be done it goes again, so a refused change leaves no device behind.
  local set, err = pcall(function()
    if input.receiver ~= nil then
      local parameter = parameter_named(device, "Receiver")
      if not parameter then error({ code = "failed", message = "the #Send device has no Receiver parameter in this Renoise" }, 0) end
      parameter.value = input.receiver
    end
    if input.amount ~= nil then
      local parameter = parameter_named(device, "Amount")
      if not parameter then error({ code = "failed", message = "the #Send device has no Amount parameter in this Renoise" }, 0) end
      if input.amount < parameter.value_min or input.amount > parameter.value_max then
        error({ code = "bad-params", message = string.format("amount must be from %s to %s", tostring(parameter.value_min), tostring(parameter.value_max)) }, 0)
      end
      parameter.value = input.amount
    end
  end)
  if not set then
    pcall(function() track:delete_device_at(at) end)
    if type(err) ~= "table" then err = { code = "failed", message = "Renoise wouldn't set up the device: " .. tostring(err) } end
    error(err, 0)
  end
  local state = material_.device_state(device)
  return {
    applied = { index = at, path = info.path, state = state },
    created = { ref = refs.make(env.epoch(), "device", index, at), kind = "device" },
    target = { ref = refs.make(env.epoch(), "device", index, at), kind = "device" },
    material = { track = index },
    display = {
      title = string.format("Added %s to %s", q(short_name(info)), q(base.named(index, track))),
      before = { devices = now.devices }, after = { devices = #track.devices, path = info.path, index = at },
    },
    undo = { desc = desc, entry = true },
    prior = { devices = now.devices }, place = { track = index, device = at }, shape = "made",
  }
end

local function add_restore(song, env, restore, desc)
  local created = restore.created
  if type(created) ~= "table" or type(created.ref) ~= "string" then bad("created: the device the change made") end
  local numbers = refs.expect(created.ref, "device", 2, env.epoch())
  local index, at = numbers[1], numbers[2]
  local track = base.track_at(song, index)
  if at < 2 or at > #track.devices then fail("gone", "the device isn't on the track any more") end
  local device = track:device(at)
  local applied = restore.applied
  if type(applied) ~= "table" then bad("applied: what the change wrote") end
  local now = material_.device_state(device)
  local title = (device.display_name ~= "" and device.display_name or device.name) .. " on " .. base.named(index, track)
  if not restore.discard and (now.path ~= applied.path or now.state ~= (applied.state or {}).state or now.active ~= (applied.state or {}).active) then
    fail("changed", title .. " changed in Renoise since Kumi made it")
  end
  local device_name, track_name = M.label(device), base.named(index, track)
  song:describe_undo(desc)
  track:delete_device_at(at)
  base.note_restored(env.journal, "device-add", { track = index, device = at })
  return {
    restored = { deleted = true },
    display = { title = string.format("Removed %s from %s (put back)", q(device_name), q(track_name)) },
    undo = { desc = desc, entry = true },
  }
end

base.register({
  kind = "device-add", custom = true, shape = "made",
  preview = add_preview, apply = add_apply, restore = add_restore,
})

-- Deleting a device keeps its path, name, bypass, preset data and automation to make it again.
local function delete_signature(track, at, device)
  local state = material_.device_state(device)
  state.index = at
  state.track = base.track_id(track) -- (the device on another track at that place is not this one)
  return state
end

local function delete_preview(song, env, input)
  local index, at, track, device, name = device_of(song, env, input.ref, "device-delete", 2)
  local state = delete_signature(track, at, device)
  local display = {
    title = string.format("Deleted %s from %s", q(M.label(device)), q(base.named(index, track))),
    before = { devices = #track.devices }, after = { devices = #track.devices - 1 },
  }
  display.note = "Kumi keeps its settings (and any automation) to put back."
  return { target = device_target(env, index, at), prior = { deleted = false }, fence = state, display = display }
end

local function delete_apply(song, env, input, fence, desc)
  local ok, index, at, track, device, name = pcall(device_of, song, env, input.ref, "device-delete", 2)
  if not ok then base.gone_is_changed(index) end
  local state = delete_signature(track, at, device)
  if not base.same_table(state, fence) then fail("changed", name .. " changed since the preview") end
  local automation = material_.read_automation(song, index, track, at)
  local patterns = {}
  for _, saved in ipairs(automation) do patterns[saved.pattern] = true end
  local material = {
    -- (its place: where it goes back is after the device before it, followed through the track's devices since; see places.lua)
    track = index, index = at, after = env.device_mark and env.device_mark(index) or nil,
    device = material_.read_device(device, song, index, false, env), state = state,
    automation = automation, pool = material_.keep_pool(song, env, patterns),
    routes = material_.read_routes(song, index, at, env),
  }
  local size = #json.encode(material)
  if size > base.MAX_MATERIAL_BYTES then
    fail("failed", string.format("%s holds %.1f MB, more than Kumi can keep to put it back, so it won't delete it", name, size / 1048576))
  end
  local title = string.format("Deleted %s from %s", q(M.label(device)), q(base.named(index, track)))
  song:describe_undo(desc)
  track:delete_device_at(at)
  return {
    applied = { deleted = true }, material = material,
    display = { title = title, note = material_.routes_note(#material.routes) }, target = device_target(env, index, at),
    undo = { desc = desc, entry = true },
    prior = { deleted = false }, place = { track = index, device = at }, shape = "deleted",
  }
end

local function delete_restore(song, env, restore, desc)
  local material = restore.material
  if type(material) ~= "table" or type(material.track) ~= "number" or type(material.device) ~= "table" then
    bad("material: what the delete removed")
  end
  local index = material.track
  local track = base.track_at(song, index)
  local saved = material.device
  local label = (saved.display ~= nil and saved.display ~= "" and saved.display or saved.name or saved.path)
  local track_name = base.named(index, track)
  -- Where it goes back: after the device that was before it, followed through the devices made, deleted and moved on the track
  -- since. When that can't be told, nothing is written.
  local at = type(material.index) == "number" and env.device_after and env.device_after(index, material.index, material.after) or nil
  if not at then
    fail("stale", string.format("the devices on %s changed in a way Kumi couldn't follow since it deleted %s, so it can't tell where it goes: nothing was put back", q(track_name), q(label)))
  end
  at = math.max(2, math.min(at or 2, #track.devices + 1))
  -- It is back already (the producer's undo, or an earlier restore): nothing to write.
  local present = at <= #track.devices and track:device(at) or nil
  if present and present.device_path == saved.path then
    local state = material_.device_state(present)
    if state.state == (material.state or {}).state and state.display == (material.state or {}).display then
      return { restored = { deleted = false }, display = { title = string.format("%s is back on %s already", q(label), q(track_name)) }, undo = { desc = desc, entry = false } }
    end
  end
  local pattern_now = material_.pool_now(song, env, material.pool)
  if not pattern_now then
    fail("stale", string.format("the song's patterns changed in a way Kumi couldn't follow since it deleted %s, so it can't tell where its automation goes: nothing was put back", q(label)))
  end
  song:describe_undo(desc)
  local device = material_.restore_device(track, at, saved)
  local tally = material_.new_tally()
  -- (where a track numbered as at the delete is now)
  local function moved(n)
    if type(restore.layout) ~= "number" then return n end
    return (env.shifted(n, restore.layout))
  end
  local lost = 0
  local ok, err = pcall(function()
    material_.count(tally, material_.restore_destination(song, device, saved, index, moved, env))
    local list = {}
    for _, entry in ipairs(material.automation or {}) do
      local p = pattern_now(entry.pattern)
      if p then
        list[#list + 1] = { pattern = p, device = at, parameter = entry.parameter, playmode = entry.playmode, points = entry.points }
      else
        lost = lost + 1
      end
    end
    material_.write_automation(song, track, index, list, nil)
  end)
  if ok then material_.write_routes(song, material.routes, { on = index, device = at }, moved, tally, env) end
  if not ok then
    pcall(function() track:delete_device_at(at) end)
    if type(err) ~= "table" then err = { code = "failed", message = "Renoise wouldn't put the device back: " .. tostring(err) } end
    error(err, 0)
  end
  base.note_restored(env.journal, "device-delete", { track = index, device = at })
  return {
    restored = { ref = refs.make(env.epoch(), "device", index, at), name = device.display_name },
    display = {
      title = string.format("%s put back on %s", q(label), q(track_name)),
      note = (function()
        local note = material_.tally_note(tally)
        if lost == 0 then return note end
        local more = string.format("Its automation in %s isn't put back: %s deleted or changed in Renoise since.",
          lost == 1 and "1 pattern" or (lost .. " patterns"), lost == 1 and "that pattern was" or "those patterns were")
        return note and (note .. " " .. more) or more
      end)(),
    },
    undo = { desc = desc, entry = true },
  }
end

base.register({
  kind = "device-delete", custom = true, shape = "deleted",
  preview = delete_preview, apply = delete_apply, restore = delete_restore,
})

-- Moving a device: swaps with its neighbours until it is where it was asked to go (`index` is the place it ends at).
local function move_wanted(track, input, at)
  local to = input.index
  if type(to) ~= "number" or to ~= math.floor(to) or to < 2 or to > #track.devices then
    bad(string.format("index must be a whole number from 2 (after the mixer device) to %d", #track.devices))
  end
  return to
end

local function move_preview(song, env, input)
  local index, at, track, device, name = device_of(song, env, input.ref, "device-move", 2)
  local to = move_wanted(track, input, at)
  local state = delete_signature(track, at, device)
  local display = {
    title = string.format("Moved %s on %s to place %d", q(M.label(device)), q(base.named(index, track)), to),
    before = { index = at }, after = { index = to },
  }
  if to == at then display.unchanged = true end
  return { target = device_target(env, index, at), prior = { index = at }, fence = state, display = display }
end

local function move_apply(song, env, input, fence, desc)
  local ok, index, at, track, device, name = pcall(device_of, song, env, input.ref, "device-move", 2)
  if not ok then base.gone_is_changed(index) end
  local state = delete_signature(track, at, device)
  if not base.same_table(state, fence) then fail("changed", name .. " changed since the preview") end
  local to = move_wanted(track, input, at)
  local display = {
    title = string.format("Moved %s on %s to place %d", q(M.label(device)), q(base.named(index, track)), to),
    before = { index = at }, after = { index = to },
  }
  if to == at then
    display.unchanged = true
    return {
      applied = { index = to, state = state }, material = { track = index }, display = display, undo = { desc = desc, entry = false },
      prior = { index = at },
    }
  end
  song:describe_undo(desc)
  local step = to > at and 1 or -1
  for i = at, to - step, step do track:swap_devices_at(i, i + step) end
  local after = material_.device_state(track:device(to))
  if after.state ~= state.state or after.path ~= state.path then fail("failed", "Renoise didn't move " .. name) end
  return {
    applied = { index = to, state = delete_signature(track, to, track:device(to)) }, material = { track = index }, display = display,
    target = device_target(env, index, to),
    undo = { desc = desc, entry = true },
    prior = { index = at }, place = { track = index, device = to }, follows = "device",
  }
end

local function move_restore(song, env, restore, desc)
  local material, applied, prior = restore.material, restore.applied, restore.prior
  if type(material) ~= "table" or type(material.track) ~= "number" then bad("material: the track the device is on") end
  if type(applied) ~= "table" or type(prior) ~= "table" then bad("prior and applied: the places the device moved between") end
  local index = material.track
  local track = base.track_at(song, index)
  local function is_it(at)
    if at < 2 or at > #track.devices then return false end
    local state = material_.device_state(track:device(at))
    local wanted = applied.state or {}
    return state.path == wanted.path and state.state == wanted.state and state.display == wanted.display
  end
  local to, from = applied.index, prior.index
  local found
  if is_it(to) then
    found = to
  elseif is_it(from) then
    return { restored = { index = from }, display = { title = "The device is back at place " .. from .. " already" }, undo = { desc = desc, entry = false } }
  elseif restore.discard then
    fail("gone", "the device isn't on the track any more")
  else
    fail("changed", "the device isn't at place " .. to .. " any more: it was moved or changed in Renoise since Kumi moved it")
  end
  if from < 2 or from > #track.devices then fail("changed", "place " .. from .. " isn't on the track any more") end
  song:describe_undo(desc)
  local step = from > found and 1 or -1
  for i = found, from - step, step do track:swap_devices_at(i, i + step) end
  base.note_restored(env.journal, "device-move", { track = index, device = from }, applied, base.KINDS["device-move"])
  return {
    restored = { index = from }, display = { title = string.format("Moved back to place %d", from) },
    undo = { desc = desc, entry = true },
  }
end

base.register({
  kind = "device-move", custom = true,
  preview = move_preview, apply = move_apply, restore = move_restore,
  same = function(a, b) return a.index == b.index end,
})

return M
