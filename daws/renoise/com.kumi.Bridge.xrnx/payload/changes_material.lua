-- What a change reads from the song to put it back, and writes to put it back: pattern lines, devices and their automation.
-- Values are Renoise's exact numbers.

local import = ...
local base = import("changes_base")

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

-- A device as it is: enough to make it again.
function M.read_device(device)
  return {
    path = device.device_path, name = device.name, display = device.display_name, active = device.is_active,
    maximized = device.is_maximized, preset = device.active_preset, presetData = device.active_preset_data,
  }
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

-- The automation of the track's parameters that have any, in every pattern: by device and parameter place. `only` limits it to
-- one device.
function M.read_automation(song, index, track, only)
  local out = {}
  for d = only or 2, only or #track.devices do
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

return M
