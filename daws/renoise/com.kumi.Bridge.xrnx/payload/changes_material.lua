-- What a change reads from the song to put it back, and writes to put it back: pattern lines, devices and their automation.
-- Values are Renoise's exact numbers.

local import = ...
local base = import("changes_base")
local rows = import("rows")

local fail = base.fail
local M = {}

-- One note column's content, as exact values.
function M.note_cell(column, c)
  return {
    col = c, note = column.note_value, instrument = column.instrument_value, volume = column.volume_value,
    panning = column.panning_value, delay = column.delay_value, fxn = column.effect_number_value, fxa = column.effect_amount_value,
  }
end

function M.effect_cell(column, c)
  return { col = c, number = column.number_value, amount = column.amount_value }
end

-- The lines of a pattern track that hold anything, and how many writes it takes to put them back.
function M.read_lines(pattern_track, total)
  local out, writes = {}, 0
  for l = 1, total do
    local line = pattern_track:line(l)
    if not line.is_empty then
      local row = { line = l, notes = {}, effects = {} }
      for c = 1, 12 do
        local column = line:note_column(c)
        if not column.is_empty then
          row.notes[#row.notes + 1] = M.note_cell(column, c)
          writes = writes + 7
        end
      end
      for c = 1, 8 do
        local column = line:effect_column(c)
        if not column.is_empty then
          row.effects[#row.effects + 1] = M.effect_cell(column, c)
          writes = writes + 2
        end
      end
      out[#out + 1] = row
    end
  end
  return out, writes
end

function M.write_lines(song, index, entry)
  local pattern_track = song:pattern(entry.pattern):track(index)
  if entry.alias then
    pattern_track.alias_pattern_index = entry.alias
    return
  end
  for _, row in ipairs(entry.lines) do
    local line = pattern_track:line(row.line)
    for _, cell in ipairs(row.notes) do
      local column = line:note_column(cell.col)
      column.note_value, column.instrument_value, column.volume_value = cell.note, cell.instrument, cell.volume
      column.panning_value, column.delay_value = cell.panning, cell.delay
      column.effect_number_value, column.effect_amount_value = cell.fxn, cell.fxa
    end
    for _, cell in ipairs(row.effects) do
      local column = line:effect_column(cell.col)
      column.number_value, column.amount_value = cell.number, cell.amount
    end
  end
end

local function parameter_named(device, name)
  for k = 1, #device.parameters do
    local parameter = device:parameter(k)
    if parameter.name == name then return parameter end
  end
  return nil
end

-- The paths of a track's devices after its mixer device. A record that names a device by its number on a track is good only while
-- the track still has the devices it had (their numbers move with every device made, deleted or moved, and nothing says which).
local function device_paths(track)
  local out = {}
  for d = 2, #track.devices do out[#out + 1] = track:device(d).device_path end
  return out
end

local function same_paths(track, list)
  if type(list) ~= "table" or #track.devices - 1 ~= #list then return false end
  for d = 2, #track.devices do
    if track:device(d).device_path ~= list[d - 1] then return false end
  end
  return true
end

-- What was put back, and what could not be (a track or a devices list that is not the one the record was made for is left alone).
function M.new_tally() return { set = 0, failed = 0, skipped = 0 } end

function M.count(tally, status)
  if status == "set" then tally.set = tally.set + 1
  elseif status == "failed" then tally.failed = tally.failed + 1
  elseif status then tally.skipped = tally.skipped + 1 end
end

local function routings(count) return count == 1 and "1 routing" or (count .. " routings") end

function M.tally_note(tally)
  local parts = {}
  if tally.set > 0 then parts[#parts + 1] = string.format("%s aimed at it set again.", routings(tally.set)) end
  if tally.skipped > 0 then
    parts[#parts + 1] = string.format("%s could not be set again, as the tracks or devices %s name changed in Renoise since: look at %s.",
      routings(tally.skipped), tally.skipped == 1 and "it" or "they", tally.skipped == 1 and "it" or "them")
  end
  if tally.failed > 0 then parts[#parts + 1] = string.format("%s could not be set again: look at %s.", routings(tally.failed), tally.failed == 1 and "it" or "them") end
  return #parts > 0 and table.concat(parts, " ") or nil
end

-- Where a meta device (*LFO and the like) sends its values: Dest. Track (a track's number less one), Dest. Effect (a device's) and
-- Dest. Parameter, -1 for none. Its preset data doesn't hold them (measured in Renoise 3.5.4): a device put back has none.
-- `own` is the place of the track it is on; with `removing` that track goes, and the tracks after it move up. A destination is kept
-- as the track's place (`to`, carried through the shifts since) and the devices that track had (`list`), never as Renoise's number.
local function read_destination(device, song, own, removing)
  if not device.device_path:find("/Native/*", 1, true) then return nil end
  local track_p = parameter_named(device, "Dest. Track")
  if not track_p or track_p.value < 0 then return nil end
  local effect_p, parameter_p = parameter_named(device, "Dest. Effect"), parameter_named(device, "Dest. Parameter")
  local target = track_p.value + 1
  local saved = { effect = effect_p and effect_p.value, parameter = parameter_p and parameter_p.value }
  if target == own then
    saved.self = true
  elseif target >= 1 and target <= rows.track_count(song) then
    saved.to = (removing and target > own) and target - 1 or target
    saved.list = device_paths(song:track(target))
  else
    return nil
  end
  return saved
end

-- A device as it is: enough to make it again. (`song` and `own`, the track's place, add where a meta device sends its values.)
function M.read_device(device, song, own, removing)
  return {
    path = device.device_path, name = device.name, display = device.display_name, active = device.is_active,
    maximized = device.is_maximized, preset = device.active_preset, presetData = device.active_preset_data,
    destination = song and read_destination(device, song, own, removing) or nil,
  }
end

-- Sets a device's destination again, once the devices around it are there: `own` is the place of the track it is on, now, and
-- `moved(at, own)` where a track numbered as at the delete is now (nil when it is gone). Answers "set", or why it was left alone:
-- "gone" (the track is gone), "changed" (its devices are not those the effect number was for) or "failed".
function M.restore_destination(song, device, saved, own, moved)
  local d = saved.destination
  if type(d) ~= "table" then return nil end
  local track_p = parameter_named(device, "Dest. Track")
  if not track_p or track_p.value ~= -1 then return nil end
  local target = d.self and own or (type(d.to) == "number" and moved(d.to, own) or nil)
  if not target or target < 1 or target > rows.track_count(song) then return "gone" end
  if not d.self and not same_paths(song:track(target), d.list) then return "changed" end
  local ok = pcall(function()
    track_p.value = target - 1
    for name, value in pairs({ ["Dest. Effect"] = d.effect, ["Dest. Parameter"] = d.parameter }) do
      local parameter = parameter_named(device, name)
      if parameter and type(value) == "number" and value >= 0 and parameter.value == -1 then parameter.value = value end
    end
  end)
  return ok and "set" or "failed"
end

-- A device put in at `at` as it was read (the state is the preset data; a plug-in that refuses it keeps its defaults).
function M.restore_device(track, at, saved)
  local device = track:insert_device_at(saved.path, at)
  if not device then fail("failed", "Renoise wouldn't make the device " .. tostring(saved.path)) end
  if saved.presetData and saved.presetData ~= "" then pcall(function() device.active_preset_data = saved.presetData end) end
  if saved.display and saved.display ~= "" then device.display_name = saved.display end
  device.is_active = saved.active ~= false
  if saved.maximized ~= nil then pcall(function() device.is_maximized = saved.maximized end) end
  return device
end

-- The automation of the track's parameters that have any, in every pattern: by device and parameter place, the mixer device's
-- (device 1: the track's volume, pan and width) too. `only` limits it to one device.
function M.read_automation(song, index, track, only)
  local out = {}
  for d = only or 1, only or #track.devices do
    local device = track:device(d)
    for k = 1, #device.parameters do
      local parameter = device:parameter(k)
      if parameter.is_automated then
        for p = 1, #song.patterns do
          local automation = song:pattern(p):track(index):find_automation(parameter)
          if automation then
            local points = {}
            for i, point in ipairs(automation.points) do
              points[i] = { time = point.time, value = point.value, scaling = point.scaling }
            end
            out[#out + 1] = { pattern = p, device = d, parameter = k, playmode = automation.playmode, points = points }
          end
        end
      end
    end
  end
  return out
end

-- The automation read above, for the patterns in `patterns` (a set), written to the devices now at the places it names.
function M.write_automation(song, track, index, list, patterns)
  for _, saved in ipairs(list or {}) do
    if patterns == nil or patterns[saved.pattern] then
      local parameter = track:device(saved.device):parameter(saved.parameter)
      local pattern_track = song:pattern(saved.pattern):track(index)
      local automation = pattern_track:find_automation(parameter) or pattern_track:create_automation(parameter)
      automation.playmode = saved.playmode
      automation.points = saved.points
    end
  end
end

-- A device told apart from another by what it is and how it is set: a state that two reads of a device that nobody touched
-- agree on (its parameters' values, not the preset text, which can be megabytes).
function M.device_state(device)
  local a, b = 5381, 0
  local count = #device.parameters
  for k = 1, count do
    local text = string.format("%.9g|", device:parameter(k).value)
    for i = 1, #text do
      local byte = text:byte(i)
      a = (a * 33 + byte) % 4294967296
      b = (b * 65599 + byte) % 4294967296
    end
  end
  return {
    path = device.device_path, display = device.display_name, active = device.is_active, parameters = count,
    state = string.format("%08x%08x", a, b),
  }
end

-- Routing aimed at a track or a device: a #Send's Receiver (the number of a send track among the sends) and a meta device's
-- destination (Dest. Track, Dest. Effect, Dest. Parameter: 0-based, -1 is none). Renoise follows tracks and devices being made, moved
-- and swapped, but clears these when their target is deleted and doesn't set them again when it comes back (measured in 3.5.4).
-- Kumi keeps what was aimed at what it deletes, and sets it again where nothing has touched it since: the device is the one at that
-- number of a track that still has the devices it had (`list`), on the track found through the shifts since (`at`).

-- The number of the send track at `index` among the sends.
local function send_number(song, index)
  local n = 0
  for i = 1, index - 1 do
    if song:track(i).type == renoise.Track.TRACK_TYPE_SEND then n = n + 1 end
  end
  return n
end

-- The devices (on other tracks, or on the track itself for a device) aimed at track `victim` (a send, for receivers; a sequencer
-- track for destinations) or, with `device_at`, at that device of the track. Their tracks are numbered as they are when the target is
-- gone: tracks after a deleted track are one less.
function M.read_routes(song, victim, device_at)
  local routes = {}
  local victim_track = song:track(victim)
  local is_send = victim_track.type == renoise.Track.TRACK_TYPE_SEND
  local is_sequencer = victim_track.type == renoise.Track.TRACK_TYPE_SEQUENCER
  local number = (is_send and not device_at) and send_number(song, victim) or nil
  for t = 1, rows.track_count(song) do
    if device_at or t ~= victim then
      local track = song:track(t)
      for d = 2, #track.devices do
        if not (t == victim and d == device_at) then
          local device = track:device(d)
          local path = device.device_path
          local route
          if number and path:find("#", 1, true) then
            for k = 1, #device.parameters do
              local parameter = device:parameter(k)
              if parameter.name:match("^Receiver") and parameter.value == number then
                route = route or {}
                route.receivers = route.receivers or {}
                route.receivers[#route.receivers + 1] = parameter.name
              end
            end
          end
          local destination = (is_sequencer or device_at) and path:find("/Native/*", 1, true) and parameter_named(device, "Dest. Track") or nil
          if destination and destination.value == victim - 1 then
            local effect, parameter = parameter_named(device, "Dest. Effect"), parameter_named(device, "Dest. Parameter")
            if not device_at or (effect and effect.value == device_at - 1) then
              route = route or {}
              route.destination = { effect = effect and effect.value, parameter = parameter and parameter.value }
            end
          end
          if route then
            route.at = (not device_at and t > victim) and t - 1 or t
            route.device, route.path, route.list = d, path, device_paths(track)
            routes[#routes + 1] = route
          end
        end
      end
    end
  end
  return routes
end

-- Sets the routes again after the target came back: the track at `now.track`, or the device at `now.device` of the track
-- `now.on`. `moved(at, inserted)` is where a route's track is now (with the track put back in), or nil when it is gone. Counts in
-- `tally` how many were set and how many were left alone or refused by Renoise.
function M.write_routes(song, routes, now, moved, tally)
  for _, route in ipairs(routes or {}) do
    local t = type(route.at) == "number" and moved(route.at, now.track) or nil
    local track = t and t >= 1 and t <= rows.track_count(song) and song:track(t) or nil
    if not track or not same_paths(track, route.list) or type(route.device) ~= "number" or route.device < 2 or route.device > #track.devices then
      M.count(tally, "gone")
    else
      local device = track:device(route.device)
      if device.device_path ~= route.path then
        M.count(tally, "changed")
      else
        local ok, set = pcall(function()
          local set = false
          if route.receivers and now.track then
            local number = send_number(song, now.track)
            for _, name in ipairs(route.receivers) do
              local parameter = parameter_named(device, name)
              if parameter and parameter.value == -1 then
                parameter.value = number
                set = true
              end
            end
          end
          if route.destination then
            local track_p = parameter_named(device, "Dest. Track")
            local effect_p, parameter_p = parameter_named(device, "Dest. Effect"), parameter_named(device, "Dest. Parameter")
            local saved = route.destination
            local function put(p, value)
              if p and type(value) == "number" and value >= 0 and p.value == -1 then p.value = value end
            end
            if now.track then
              if track_p and track_p.value == -1 then
                track_p.value = now.track - 1
                put(effect_p, saved.effect)
                put(parameter_p, saved.parameter)
                set = true
              end
            elseif track_p and track_p.value == now.on - 1 and effect_p and effect_p.value == -1 then
              put(effect_p, now.device - 1)
              put(parameter_p, saved.parameter)
              set = true
            end
          end
          return set
        end)
        M.count(tally, ok and (set and "set" or nil) or "failed")
      end
    end
  end
end

-- What to say about the routes when something is deleted: how many are aimed at it.
function M.routes_note(count)
  if count == 0 then return nil end
  return string.format("%s aimed at it (sends, modulation targets): Renoise lets go of them when it goes, and Kumi sets them again if it is put back.", routings(count))
end

return M
