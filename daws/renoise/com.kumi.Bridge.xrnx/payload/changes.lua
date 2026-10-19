-- Kumi's changes to a Renoise song (M3): a preview that writes nothing, an apply fenced by what the preview saw, and a restore
-- fenced by what the apply wrote. Every change is written in ONE synchronous callback named by `describe_undo(desc)` (`desc`
-- carries the host's nonce), so the producer's own undo takes it back whole. Kumi's own undo never calls Renoise's: it writes
-- the prior values back as a new step. A restore too big for one callback (about 1 s of writes) runs as numbered steps
-- ("... (2/3)"), each its own callback, and takes back what it made if a later one cannot be made.
--
-- Each kind knows what it would do (`preview`), how to do it (`apply`, which reads what the preview fenced again and refuses
-- `changed` before it writes), and how to put it back (`restore`, which refuses `changed` if the fields it touched no longer
-- hold what the apply wrote). Mute, solo, loop and selection are not Renoise undo steps: they answer `entry: false`.

local import = ...
local json = import("json")
local refs = import("refs")
local rows = import("rows")

local M = {}

local fail, the_song = rows.fail, rows.the_song

local function bad(message) fail("bad-params", message) end

local MINUS = "\226\136\146" -- the true minus sign, U+2212
local INFINITY = "\226\136\158" -- U+221E

-- Writes that make one slice of a restore (about a second of Renoise's time), and the most a journalled change may hold.
M.SLICE_WRITES = 15000
-- The wait between the numbered steps of a restore. Renoise made one undo step of two callbacks 10 ms apart (measured), and
-- kept callbacks 300 ms apart apart.
M.STEP_GAP_MS = 400
M.MAX_MATERIAL_BYTES = 8 * 1024 * 1024
M.JOURNAL_MAX = 64

-- Text

local function trimmed(value)
  local text = string.format("%.1f", value)
  return (text:gsub("%.0$", ""))
end

-- dB with the true minus sign, as the titles show it: `−6`, `+3`, `0`, `−6.5`; `−∞` at silence.
function M.db_text(db)
  if db == nil then return MINUS .. INFINITY end
  local rounded = math.floor(db * 10 + 0.5) / 10
  if rounded == 0 then return "0" end
  if rounded < 0 then return MINUS .. trimmed(-rounded) end
  return "+" .. trimmed(rounded)
end

function M.pan_text(pan)
  local percent = math.floor(math.abs(pan) * 100 + 0.5)
  if percent == 0 then return "centre" end
  if pan < 0 then return percent .. "% left" end
  return percent .. "% right"
end

local function close(a, b)
  if type(a) ~= "number" or type(b) ~= "number" then return a == b end
  return math.abs(a - b) <= 1e-6 * math.max(1, math.abs(a), math.abs(b))
end

local function db_of(linear)
  if type(linear) == "number" and linear > 0 then return 20 * math.log10(linear) end
  return nil
end

local function hex_of(color)
  if type(color) ~= "table" or not color[1] then return nil end
  return string.format("#%02x%02x%02x", color[1], color[2], color[3])
end

local function rgb_of(text)
  local r, g, b = tostring(text):match("^#(%x%x)(%x%x)(%x%x)$")
  if not r then return nil end
  return { tonumber(r, 16), tonumber(g, 16), tonumber(b, 16) }
end

-- The song and the track a request names

local function need_text(params, name)
  local value = params[name]
  if type(value) ~= "string" or value == "" then bad(name .. " must be a string") end
  return value
end

local function need_object(params, name)
  local value = params[name]
  if type(value) ~= "table" then bad(name .. " must be an object") end
  return value
end

-- The song, if the request's epoch is the song's and Renoise isn't busy.
local function context(params, env)
  local epoch = params.epoch
  if type(epoch) ~= "string" or epoch == "" then bad("epoch: the epoch the refs were read in") end
  if epoch ~= env.epoch() then
    fail("stale", "those refs are from an earlier epoch (" .. epoch .. "; now " .. env.epoch() .. "): the song changed, read again")
  end
  local song = the_song()
  if song.rendering then fail("busy", "Renoise is rendering the song; nothing changed") end
  return song
end

local function track_at(song, index)
  if index < 1 or index > rows.track_count(song) then fail("gone", "that track isn't in the song any more") end
  return song:track(index)
end

local function find(song, env, reference, what)
  if type(reference) ~= "string" then bad(what .. ": ref must be a track ref") end
  local index = refs.expect(reference, "track", 1, env.epoch())[1]
  local track = track_at(song, index)
  return index, track
end

local function is_master(track) return track.type == renoise.Track.TRACK_TYPE_MASTER end

local function named(index, track)
  local name = track.name
  if name == nil or name == "" then return "Track " .. string.format("%02d", index) end
  return name
end

local function target(env, index)
  return { ref = refs.make(env.epoch(), "track", index), kind = "track" }
end

local function stage_of(source)
  local stage = type(source) == "table" and source.stage or nil
  if stage == nil then return "pre" end
  if stage ~= "pre" and stage ~= "post" then bad("stage must be \"pre\" or \"post\"") end
  return stage
end

-- The track properties

local PROPS = {}
local PROP_LIST = {}

local function prop(spec)
  PROPS[spec.kind] = spec
  PROP_LIST[#PROP_LIST + 1] = spec
end

prop {
  kind = "track-volume", noun = "volume", master = true, undoable = true, watch = true,
  read = function(track, source)
    local stage = stage_of(source)
    local value = (stage == "post" and track.postfx_volume or track.prefx_volume).value
    return { stage = stage, volume = value, volumeDb = db_of(value) }
  end,
  wanted = function(track, input)
    local stage = stage_of(input)
    local db = input.volumeDb
    if type(db) ~= "number" or db ~= db then bad("volumeDb must be a number") end
    local parameter = stage == "post" and track.postfx_volume or track.prefx_volume
    local top = db_of(parameter.value_max) or 3
    if db < -150 or db > top + 1e-6 then bad(string.format("volumeDb must be from -150 (silent) to %.1f", top)) end
    local value = db <= -150 and 0 or math.min(10 ^ (db / 20), parameter.value_max)
    return { stage = stage, volume = value, volumeDb = db_of(value) }
  end,
  write = function(track, state)
    (state.stage == "post" and track.postfx_volume or track.prefx_volume).value = state.volume
  end,
  same = function(a, b) return a.stage == b.stage and close(a.volume, b.volume) end,
  title = function(name, _, to) return name .. " volume " .. M.db_text(to.volumeDb) .. " dB" end,
}

prop {
  kind = "track-pan", noun = "pan", master = true, undoable = true, watch = true,
  read = function(track, source)
    local stage = stage_of(source)
    local value = (stage == "post" and track.postfx_panning or track.prefx_panning).value
    return { stage = stage, pan = value * 2 - 1 }
  end,
  wanted = function(_, input)
    local stage = stage_of(input)
    local pan = input.pan
    if type(pan) ~= "number" or pan ~= pan or pan < -1 or pan > 1 then bad("pan must be a number from -1 (left) to 1 (right)") end
    return { stage = stage, pan = pan }
  end,
  write = function(track, state)
    (state.stage == "post" and track.postfx_panning or track.prefx_panning).value = (state.pan + 1) / 2
  end,
  same = function(a, b) return a.stage == b.stage and close(a.pan, b.pan) end,
  title = function(name, _, to) return name .. " pan " .. M.pan_text(to.pan) end,
}

prop {
  kind = "track-mute", noun = "mute", master = false, undoable = false,
  read = function(track)
    local state = track.mute_state
    return { mute = state ~= renoise.Track.MUTE_STATE_ACTIVE, muteState = state }
  end,
  wanted = function(_, input)
    if type(input.mute) ~= "boolean" then bad("mute must be true or false") end
    local T = renoise.Track
    local state = input.mute and T.MUTE_STATE_MUTED or T.MUTE_STATE_ACTIVE
    return { mute = input.mute, muteState = state }
  end,
  write = function(track, state) track.mute_state = state.muteState end,
  same = function(a, b) return a.muteState == b.muteState end,
  title = function(name, _, to) return name .. (to.mute and " muted" or " unmuted") end,
}

prop {
  kind = "track-solo", noun = "solo", master = false, undoable = false,
  read = function(track) return { solo = track.solo_state == true } end,
  wanted = function(_, input)
    if type(input.solo) ~= "boolean" then bad("solo must be true or false") end
    return { solo = input.solo }
  end,
  write = function(track, state) track.solo_state = state.solo end,
  same = function(a, b) return a.solo == b.solo end,
  title = function(name, _, to) return name .. (to.solo and " soloed" or " unsoloed") end,
}

prop {
  kind = "track-rename", noun = "name", master = false, undoable = true,
  read = function(track) return { name = track.name } end,
  wanted = function(_, input)
    local name = input.name
    if type(name) ~= "string" then bad("name must be text") end
    if #name > 1024 or name:find("[%c]") then bad("name must be one line of text") end
    return { name = name }
  end,
  write = function(track, state) track.name = state.name end,
  same = function(a, b) return a.name == b.name end,
  title = function(_, from, to)
    if from.name == "" then return "Named track " .. to.name end
    return "Renamed track " .. from.name .. " to " .. to.name
  end,
}

prop {
  kind = "track-color", noun = "colour", master = true, undoable = true,
  read = function(track) return { color = hex_of(track.color), colorBlend = track.color_blend } end,
  wanted = function(track, input)
    local color = rgb_of(input.color)
    if not color then bad("color must be #rrggbb") end
    local blend = input.colorBlend
    if blend == nil then
      blend = track.color_blend
    elseif type(blend) ~= "number" or blend ~= math.floor(blend) or blend < 0 or blend > 100 then
      bad("colorBlend must be a whole number from 0 to 100")
    end
    return { color = hex_of(color), colorBlend = blend }
  end,
  write = function(track, state)
    track.color = rgb_of(state.color)
    track.color_blend = state.colorBlend
  end,
  same = function(a, b) return a.color == b.color and a.colorBlend == b.colorBlend end,
  title = function(name) return name .. " colour changed" end,
}

local NOT_UNDOABLE = "Renoise's own undo won't take this back; Kumi can."

local function prop_preview(song, env, spec, input)
  local index, track = find(song, env, input.ref, spec.kind)
  if is_master(track) and not spec.master then bad("the master track has no " .. spec.noun) end
  local now = spec.read(track, input)
  local wanted = spec.wanted(track, input)
  local display = { title = spec.title(named(index, track), now, wanted), before = now, after = wanted }
  if spec.same(now, wanted) then display.unchanged = true end
  if not spec.undoable then display.note = NOT_UNDOABLE end
  return { target = target(env, index), prior = now, fence = now, display = display }
end

local function gone_is_changed(err)
  if type(err) == "table" and err.code == "gone" then
    error({ code = "changed", message = err.message .. ": it went since the preview" }, 0)
  end
  error(err, 0)
end

local function prop_apply(song, env, spec, input, fence, desc)
  local ok, index, track = pcall(find, song, env, input.ref, spec.kind)
  if not ok then gone_is_changed(index) end
  local name = named(index, track)
  local now = spec.read(track, fence)
  if not spec.same(now, fence) then
    fail("changed", name .. "'s " .. spec.noun .. " changed since the preview")
  end
  local wanted = spec.wanted(track, input)
  if spec.undoable then song:describe_undo(desc) end
  if spec.watch then env.watch_params(index) end
  if not spec.same(now, wanted) then spec.write(track, wanted) end
  local after = spec.read(track, wanted)
  if not spec.same(after, wanted) then fail("failed", "Renoise kept " .. name .. "'s " .. spec.noun .. " as it was") end
  local display = { title = spec.title(name, now, after), before = now, after = after }
  if not spec.undoable then display.note = NOT_UNDOABLE end
  return {
    applied = after,
    display = display,
    undo = { desc = desc, entry = spec.undoable },
    spec = spec, index = index, prior = now,
  }
end

local function prop_restore(song, env, spec, restore, desc)
  local reference = restore.target and restore.target.ref
  local ok, index, track = pcall(find, song, env, reference, spec.kind)
  if not ok then
    if type(index) == "table" and index.code == "failed" then index.code = "gone" end
    error(index, 0)
  end
  local name = named(index, track)
  local now = spec.read(track, restore.prior)
  if not spec.same(now, restore.prior) then
    if not restore.discard and not spec.same(now, restore.applied) then
      fail("changed", name .. "'s " .. spec.noun .. " changed in Renoise since Kumi set it")
    end
    if spec.undoable then song:describe_undo(desc) end
    if spec.watch then env.watch_params(index) end
    spec.write(track, restore.prior)
    local after = spec.read(track, restore.prior)
    if not spec.same(after, restore.prior) then fail("failed", "Renoise kept " .. name .. "'s " .. spec.noun .. " as it was") end
  end
  local after = spec.read(track, restore.prior)
  local display = { title = spec.title(name, now, after) .. " (put back)", before = now, after = after }
  return { restored = after, display = display, undo = { desc = desc, entry = spec.undoable }, index = index }
end

-- Deleting a track, and making it again

local function lines_text(count) return count == 1 and "1 line" or count .. " lines" end

-- What tells a track apart from what the preview saw: its place, name and what it holds.
local function signature(song, index, track)
  local notes = 0
  for p = 1, #song.patterns do
    local pattern_track = song:pattern(p):track(index)
    if not pattern_track.is_empty then notes = notes + 1 end
  end
  return {
    name = track.name,
    index = index,
    kind = rows.track_type(track),
    devices = math.max(#track.devices - 1, 0),
    patternsWithLines = notes,
    noteColumns = track.visible_note_columns,
    effectColumns = track.visible_effect_columns,
  }
end

local function same_table(a, b)
  if type(a) ~= "table" or type(b) ~= "table" then return a == b end
  for k, v in pairs(a) do
    if not same_table(v, b[k]) then return false end
  end
  for k in pairs(b) do
    if a[k] == nil then return false end
  end
  return true
end

local function find_to_delete(song, env, input)
  local index, track = find(song, env, input.ref, "track-delete")
  local kind = rows.track_type(track)
  if kind == "master" then bad("the master track can't be deleted") end
  if kind == "group" then bad("a group track is deleted with its members; delete the group in Renoise") end
  return index, track, kind
end

local function delete_preview(song, env, input)
  local index, track = find_to_delete(song, env, input)
  local now = signature(song, index, track)
  local display = { title = "Deleted track " .. named(index, track) }
  if now.devices > 0 or now.patternsWithLines > 0 then
    display.note = string.format("It holds %d devices and lines in %d patterns; Kumi keeps them to put back.", now.devices, now.patternsWithLines)
  end
  return { target = target(env, index), prior = now, fence = now, display = display }
end

-- One note column's content, as exact values.
local function note_cell(column, c)
  return {
    col = c, note = column.note_value, instrument = column.instrument_value, volume = column.volume_value,
    panning = column.panning_value, delay = column.delay_value, fxn = column.effect_number_value, fxa = column.effect_amount_value,
  }
end

local function effect_cell(column, c)
  return { col = c, number = column.number_value, amount = column.amount_value }
end

-- The lines of a pattern track that hold anything, and how many writes it takes to put them back.
local function read_lines(pattern_track, total)
  local out, writes = {}, 0
  for l = 1, total do
    local line = pattern_track:line(l)
    if not line.is_empty then
      local row = { line = l, notes = {}, effects = {} }
      for c = 1, 12 do
        local column = line:note_column(c)
        if not column.is_empty then
          row.notes[#row.notes + 1] = note_cell(column, c)
          writes = writes + 7
        end
      end
      for c = 1, 8 do
        local column = line:effect_column(c)
        if not column.is_empty then
          row.effects[#row.effects + 1] = effect_cell(column, c)
          writes = writes + 2
        end
      end
      out[#out + 1] = row
    end
  end
  return out, writes
end

local function read_device(device)
  return {
    path = device.device_path, name = device.name, display = device.display_name, active = device.is_active,
    maximized = device.is_maximized, preset = device.active_preset, presetData = device.active_preset_data,
  }
end

-- The automation of the track's parameters that have any, in every pattern: by device and parameter place.
local function read_automation(song, index, track)
  local out = {}
  for d = 2, #track.devices do
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

local function gather(song, index, track, kind)
  local sequencer = song.sequencer
  local material = {
    index = index, kind = kind, name = track.name, color = hex_of(track.color), colorBlend = track.color_blend,
    muteState = track.mute_state, solo = track.solo_state == true, collapsed = track.collapsed,
    volume = { pre = track.prefx_volume.value, post = track.postfx_volume.value },
    panning = { pre = track.prefx_panning.value, post = track.postfx_panning.value },
    width = track.prefx_width.value,
    outputRouting = track.output_routing, outputDelay = track.output_delay,
    columns = {
      note = track.visible_note_columns, effect = track.visible_effect_columns,
      volume = track.volume_column_visible, panning = track.panning_column_visible, delay = track.delay_column_visible,
      sampleEffects = track.sample_effects_column_visible, names = {}, muted = {},
    },
    devices = {}, automation = {}, patterns = {}, slotMutes = {},
  }
  for c = 1, track.max_note_columns do
    local name = track:column_name(c)
    if name ~= "" then material.columns.names[tostring(c)] = name end
    if track:column_is_muted(c) then material.columns.muted[#material.columns.muted + 1] = c end
  end
  local parents = rows.group_parents(song)
  if parents[index] then
    material.group = { index = parents[index], name = song:track(parents[index]).name }
  end
  for d = 2, #track.devices do material.devices[#material.devices + 1] = read_device(track:device(d)) end
  material.automation = read_automation(song, index, track)
  local writes = 0
  for p = 1, #song.patterns do
    local pattern_track = song:pattern(p):track(index)
    if pattern_track.is_alias then
      material.patterns[#material.patterns + 1] = { pattern = p, alias = pattern_track.alias_pattern_index }
    elseif not pattern_track.is_empty then
      local lines, cost = read_lines(pattern_track, song:pattern(p).number_of_lines)
      if #lines > 0 then material.patterns[#material.patterns + 1] = { pattern = p, lines = lines, writes = cost } end
      writes = writes + cost
    end
  end
  for s = 1, #sequencer.pattern_sequence do
    if sequencer:track_sequence_slot_is_muted(index, s) then material.slotMutes[#material.slotMutes + 1] = s end
  end
  material.writes = writes
  return material
end

local function delete_apply(song, env, input, fence, desc)
  local ok, index, track, kind = pcall(find_to_delete, song, env, input)
  if not ok then gone_is_changed(index) end
  local now = signature(song, index, track)
  if not same_table(now, fence) then fail("changed", named(index, track) .. " changed since the preview") end
  local name = named(index, track)
  local material = gather(song, index, track, kind)
  local size = #json.encode(material)
  if size > M.MAX_MATERIAL_BYTES then
    fail("failed", string.format("%s holds %.1f MB, more than Kumi can keep to put it back, so it won't delete it", name, size / 1048576))
  end
  song:describe_undo(desc)
  song:delete_track_at(index)
  return {
    applied = { deleted = true },
    material = material,
    display = { title = "Deleted track " .. name },
    undo = { desc = desc, entry = true },
    kind = "track-delete", index = index, prior = { deleted = false }, deleted = { name = material.name, kind = kind },
  }
end

-- The writes that make a track again, in phases. A phase is one callback; the first makes the track, its properties, devices,
-- columns and slot mutes; the rest write lines (and the automation of those patterns), up to SLICE_WRITES each.
local function plan_phases(material)
  local phases, current, writes = {}, {}, 0
  for _, entry in ipairs(material.patterns or {}) do
    local cost = entry.writes or 1
    if writes > 0 and writes + cost > M.SLICE_WRITES then
      phases[#phases + 1] = current
      current, writes = {}, 0
    end
    current[#current + 1] = entry
    writes = writes + cost
  end
  phases[#phases + 1] = current
  return phases
end

-- What Renoise lets a track of this kind have: a send track has no output delay (it raises), no note columns, and the other
-- settings are tried and left as they are if Renoise refuses (they are looks, not content).
local function restore_properties(song, track, index, material)
  track.name = material.name
  local color = rgb_of(material.color)
  if color then track.color = color end
  track.color_blend = material.colorBlend
  track.prefx_volume.value = material.volume.pre
  track.postfx_volume.value = material.volume.post
  track.prefx_panning.value = material.panning.pre
  track.postfx_panning.value = material.panning.post
  local function try(write) pcall(write) end
  try(function() track.prefx_width.value = material.width end)
  if material.outputRouting then try(function() track.output_routing = material.outputRouting end) end
  if material.kind == "sequencer" and type(material.outputDelay) == "number" then
    try(function() track.output_delay = material.outputDelay end)
  end
  try(function() track.collapsed = material.collapsed == true end)
  local columns = material.columns or {}
  if track.max_note_columns > 0 then
    track.visible_note_columns = columns.note
    try(function() track.volume_column_visible = columns.volume == true end)
    try(function() track.panning_column_visible = columns.panning == true end)
    try(function() track.delay_column_visible = columns.delay == true end)
    try(function() track.sample_effects_column_visible = columns.sampleEffects == true end)
  end
  try(function() track.visible_effect_columns = columns.effect end)
  for c, name in pairs(columns.names or {}) do try(function() track:set_column_name(tonumber(c), name) end) end
  for _, c in ipairs(columns.muted or {}) do try(function() track:set_column_is_muted(c, true) end) end
  if material.solo and rows.track_type(track) ~= "master" then try(function() track.solo_state = true end) end
  if material.muteState and rows.track_type(track) ~= "master" then try(function() track.mute_state = material.muteState end) end
end

local function restore_devices(track, material)
  for i, saved in ipairs(material.devices or {}) do
    local device = track:insert_device_at(saved.path, i + 1)
    if not device then fail("failed", "Renoise wouldn't make the device " .. tostring(saved.path)) end
    if saved.presetData and saved.presetData ~= "" then pcall(function() device.active_preset_data = saved.presetData end) end
    if saved.display and saved.display ~= "" then device.display_name = saved.display end
    device.is_active = saved.active ~= false
    if saved.maximized ~= nil then pcall(function() device.is_maximized = saved.maximized end) end
  end
end

local function write_lines(song, index, entry)
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

local function write_automation(song, track, index, material, patterns)
  for _, saved in ipairs(material.automation or {}) do
    if patterns[saved.pattern] then
      local parameter = track:device(saved.device):parameter(saved.parameter)
      local pattern_track = song:pattern(saved.pattern):track(index)
      local automation = pattern_track:find_automation(parameter) or pattern_track:create_automation(parameter)
      automation.playmode = saved.playmode
      automation.points = saved.points
    end
  end
end

-- Where the track goes back: its old place, moved by the tracks made and deleted since. A track made at or before the master's
-- place is a sequencer track (the master moves up); one made after it is a send track (measured in Renoise 3.5.4: inserting AT
-- the master's place makes a sequencer track, not a send).
local function restore_index(song, material)
  local master = song.sequencer_track_count + 1
  if material.kind == "send" then
    return math.max(master + 1, math.min(material.index, rows.track_count(song) + 1))
  end
  return math.max(1, math.min(material.index, master))
end

local function make_track(song, material)
  local index = restore_index(song, material)
  song:insert_track_at(index)
  return index
end

-- A member of a group goes back into it: Renoise makes a track inserted just before a group (but not before its first member)
-- a member by itself; otherwise it is added, which puts it last among the members, and moved to its own place by swaps.
local function put_in_group(song, index, material)
  if not material.group then return index end
  if song:track(index).group_parent then return index end
  local group = math.min(material.group.index, rows.track_count(song))
  local candidate = song:track(group)
  if candidate.type ~= renoise.Track.TRACK_TYPE_GROUP or candidate.name ~= material.group.name then return index end
  song:add_track_to_group(index, group)
  -- it is the last member now, just before the group, which has moved up one
  local at = group - 1
  while at > index do
    song:swap_tracks_at(at, at - 1)
    at = at - 1
  end
  return index
end

-- Phase one, in the callback that made the track. Nothing half-done stays: a track that couldn't be finished is deleted again.
local function restore_first_phase(song, material, phases)
  local index = make_track(song, material)
  local ok, err = pcall(function()
    local track = song:track(index)
    restore_properties(song, track, index, material)
    restore_devices(track, material)
    index = put_in_group(song, index, material)
    local sequencer = song.sequencer
    for _, s in ipairs(material.slotMutes or {}) do
      if s <= #sequencer.pattern_sequence then sequencer:set_track_sequence_slot_is_muted(index, s, true) end
    end
    local done = {}
    for _, entry in ipairs(phases[1]) do
      write_lines(song, index, entry)
      done[entry.pattern] = true
    end
    write_automation(song, song:track(index), index, material, done)
  end)
  if not ok then
    pcall(function() song:delete_track_at(index) end)
    if type(err) ~= "table" then err = { code = "failed", message = "Renoise wouldn't put the track back: " .. tostring(err):gsub("^[^%s:]*%.lua:%d+: ", "") } end
    error(err, 0)
  end
  return index
end

local function restored_answer(env, song, index, material)
  local track = song:track(index)
  local display = { title = "Track " .. named(index, track) .. " put back" }
  return {
    restored = { ref = refs.make(env.epoch(), "track", index), name = track.name },
    display = display,
    index = index,
  }
end

-- A track with this name and kind is at the place already: it came back some other way, or was never gone.
local function already_there(song, material)
  local top = rows.track_count(song)
  for _, at in ipairs({ material.index, material.indexThen or material.index }) do
    local track = at >= 1 and at <= top and song:track(at) or nil
    if track ~= nil and track.name == material.name and rows.track_type(track) == material.kind then return true end
  end
  return false
end

local function delete_restore(song, env, restore, desc, journal)
  local material = restore.material
  if type(material) ~= "table" or type(material.name) ~= "string" then bad("material: what the delete removed") end
  local phases = plan_phases(material)
  if already_there(song, material) and #phases >= 1 and not restore.discard then
    fail("changed", "a track with that name is in the song again")
  end
  if #phases == 1 then
    song:describe_undo(desc)
    local index = restore_first_phase(song, material, phases)
    local answer = restored_answer(env, song, index, material)
    answer.undo = { desc = desc, entry = true }
    return answer
  end
  -- Too much for one callback: the track and its parts in the first, then lines, each its own numbered step.
  return {
    kumi_job = function(job)
      local total = #phases
      local function step(k) return string.format("%s (%d/%d)", desc, k, total) end
      song = the_song()
      song:describe_undo(step(1))
      local index = restore_first_phase(song, material, phases)
      job.rebase()
      local ok, err = pcall(function()
        for k = 2, total do
          job.step(M.STEP_GAP_MS)
          local live = the_song()
          if live.rendering then fail("busy", "Renoise is rendering the song") end
          live:describe_undo(step(k))
          local done = {}
          for _, entry in ipairs(phases[k]) do
            write_lines(live, index, entry)
            done[entry.pattern] = true
          end
          write_automation(live, live:track(index), index, material, done)
        end
      end)
      if not ok then
        -- Nothing half-done stays: the track this restore made goes again.
        pcall(function() the_song():delete_track_at(index) end)
        job.rebase()
        error(err, 0)
      end
      local live = the_song()
      local answer = restored_answer(env, live, index, material)
      answer.undo = { desc = desc, entry = true, steps = total }
      answer.epoch, answer.layout = env.epoch(), env.layout()
      return answer
    end,
  }
end

-- The journal: the changes this tool made that a producer's undo could take back

function M.journal_new()
  return { entries = {}, events = {} }
end

function M.journal_reset(journal)
  journal.entries, journal.events = {}, {}
end

local function remember(journal, entry)
  local entries = journal.entries
  entries[#entries + 1] = entry
  while #entries > M.JOURNAL_MAX do table.remove(entries, 1) end
end

local function queue(journal, desc, how)
  local events = journal.events
  local last = events[#events]
  if last and last.how == how then
    last.descs[#last.descs + 1] = desc
  else
    events[#events + 1] = { how = how, descs = { desc } }
  end
end

-- The events to tell Kumi of, once.
function M.journal_take(journal)
  local events = journal.events
  journal.events = {}
  return events
end

function M.journal_active(journal)
  return #journal.entries > 0
end

-- A track made or deleted at `index`: the places the journal knows move with them; when the producer's undo or redo does it,
-- it may be Kumi's delete taken back (or done again).
function M.journal_track_event(journal, type_, index, undoing)
  for i = #journal.entries, 1, -1 do
    local entry = journal.entries[i]
    if entry.kind == "track-delete" then
      if type_ == "insert" then
        if undoing and entry.state == "applied" and entry.index == index then
          entry.state = "undone"
          queue(journal, entry.desc, "undo")
        elseif entry.index >= index then
          entry.index = entry.index + 1
        end
      else
        if undoing and entry.state == "undone" and entry.index == index then
          entry.state = "applied"
          queue(journal, entry.desc, "redo")
        elseif entry.index > index then
          entry.index = entry.index - 1
        end
      end
    else
      if type_ == "insert" then
        if entry.index >= index then entry.index = entry.index + 1 end
      elseif entry.index == index then
        table.remove(journal.entries, i) -- (the track is gone: nothing left to compare)
      elseif entry.index > index then
        entry.index = entry.index - 1
      end
    end
  end
end

-- After a producer's undo or redo: the journalled properties are read again and compared with what Kumi wrote and what it
-- replaced.
function M.journal_check(journal)
  local song = renoise.song()
  if not song then return end
  for _, entry in ipairs(journal.entries) do
    local spec = entry.spec
    if spec and entry.index <= rows.track_count(song) then
      local ok, now = pcall(function() return spec.read(song:track(entry.index), entry.applied) end)
      if ok then
        if entry.state == "applied" and spec.same(now, entry.prior) and not spec.same(now, entry.applied) then
          entry.state = "undone"
          queue(journal, entry.desc, "undo")
        elseif entry.state == "undone" and spec.same(now, entry.applied) then
          entry.state = "applied"
          queue(journal, entry.desc, "redo")
        end
      end
    end
  end
end

-- A restore of a change: the change is not the producer's to undo any more.
local function note_restored(journal, kind, index, applied, spec)
  for _, entry in ipairs(journal.entries) do
    if entry.kind == kind and entry.state == "applied" and (kind == "track-delete" or (entry.index == index and spec.same(entry.applied, applied))) then
      entry.state = "undone"
      if kind == "track-delete" then entry.index = index end
      return
    end
  end
end

-- The three methods

local function kind_of(params)
  local name = need_text(params, "kind")
  if PROPS[name] or name == "track-delete" then return name end
  bad("Renoise has no change kind " .. string.format("%q", name))
end

function M.preview(params, env)
  local song = context(params, env)
  local kind = kind_of(params)
  local input = need_object(params, "input")
  local preview
  if kind == "track-delete" then
    preview = delete_preview(song, env, input)
  else
    preview = prop_preview(song, env, PROPS[kind], input)
  end
  return {
    epoch = env.epoch(), layout = env.layout(), target = preview.target, prior = preview.prior, fence = preview.fence,
    display = preview.display,
  }
end

function M.apply(params, env)
  local song = context(params, env)
  local kind = kind_of(params)
  local input, fence = need_object(params, "input"), need_object(params, "fence")
  local desc = need_text(params, "desc")
  local done
  if kind == "track-delete" then
    done = delete_apply(song, env, input, fence, desc)
  else
    done = prop_apply(song, env, PROPS[kind], input, fence, desc)
  end
  local answer = {
    epoch = env.epoch(), layout = env.layout(), applied = done.applied, display = done.display, undo = done.undo,
  }
  if done.material then answer.material = done.material end
  if done.undo.entry then
    remember(env.journal, {
      desc = desc, kind = kind, index = done.index, spec = done.spec, prior = done.prior, applied = done.applied,
      state = "applied", name = done.deleted and done.deleted.name or nil,
    })
  end
  return answer
end

function M.restore(params, env)
  local song = context(params, env)
  local kind = kind_of(params)
  local desc = need_text(params, "desc")
  local restore = {
    target = need_object(params, "target"), prior = params.prior, applied = params.applied, material = params.material,
    discard = params.discard == true,
  }
  local done
  if kind == "track-delete" then
    done = delete_restore(song, env, restore, desc, env.journal)
    if done.kumi_job then
      local work = done.kumi_job
      done.kumi_job = function(job)
        local answer = work(job)
        note_restored(env.journal, "track-delete", answer.index, nil, nil)
        answer.index = nil
        return answer
      end
      return done
    end
    note_restored(env.journal, kind, done.index, nil, nil)
  else
    local spec = PROPS[kind]
    if type(restore.prior) ~= "table" or type(restore.applied) ~= "table" then bad("prior and applied: the change's values") end
    done = prop_restore(song, env, spec, restore, desc)
    note_restored(env.journal, kind, done.index, restore.applied, spec)
  end
  done.epoch, done.layout = env.epoch(), env.layout()
  done.index = nil
  return done
end

-- The track delete's restore needs its place carried by hand: the deleted track's own ref is gone by design.
function M.carry_restore(params, shifts, since)
  local material = params.material
  if type(material) == "table" and type(material.index) == "number" then
    local moved = refs.shift(material.index, shifts, since)
    local copy = {}
    for k, v in pairs(params) do copy[k] = v end
    copy.material = {}
    for k, v in pairs(material) do copy.material[k] = v end
    -- a place that was the deleted track's own is still its place (the track after it moved up into it); one before it moved
    copy.material.index = moved or material.index
    copy.material.indexThen = material.index
    return copy
  end
  return params
end

M.PROPS = PROPS
return M
