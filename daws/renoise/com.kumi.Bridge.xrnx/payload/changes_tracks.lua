-- Kumi's changes to a track: its volume, pan, mute, solo, name and colour; and deleting it, with what undo needs to make it
-- again (see changes_base.lua for how a change goes).

local import = ...
local json = import("json")
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")

local fail, bad, the_song = base.fail, base.bad, base.the_song
local close, db_of, hex_of, rgb_of = base.close, base.db_of, base.hex_of, base.rgb_of
local find, is_master, named, target, stage_of = base.find, base.is_master, base.named, base.target, base.stage_of
local same_table = base.same_table
local NOT_UNDOABLE = base.NOT_UNDOABLE

local M = {}

-- The track properties

-- A track property is found by the track's ref, and found again by its place.
local function track_reread(song, place)
  if place.track >= 1 and place.track <= rows.track_count(song) then return song:track(place.track) end
  return nil
end

local function prop(spec)
  spec.locate = function(song, env, input)
    local index, track = find(song, env, input.ref, spec.kind)
    if is_master(track) and not spec.master then bad("the master track has no " .. spec.noun) end
    return { object = track, name = named(index, track), place = { track = index }, target = target(env, index) }
  end
  spec.reread = track_reread
  if spec.watch then
    spec.watch = function(env, loc) env.watch_params(loc.place.track) end
  end
  base.register(spec)
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
  title = function(name, _, to) return name .. " volume " .. base.db_text(to.volumeDb) .. " dB" end,
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
  title = function(name, _, to) return name .. " pan " .. base.pan_text(to.pan) end,
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
  if not ok then base.gone_is_changed(index) end
  local now = signature(song, index, track)
  if not same_table(now, fence) then fail("changed", named(index, track) .. " changed since the preview") end
  local name = named(index, track)
  local material = gather(song, index, track, kind)
  local size = #json.encode(material)
  if size > base.MAX_MATERIAL_BYTES then
    fail("failed", string.format("%s holds %.1f MB, more than Kumi can keep to put it back, so it won't delete it", name, size / 1048576))
  end
  song:describe_undo(desc)
  song:delete_track_at(index)
  return {
    applied = { deleted = true },
    material = material,
    display = { title = "Deleted track " .. name },
    undo = { desc = desc, entry = true },
    prior = { deleted = false }, place = { track = index }, shape = "deleted",
  }
end

-- The writes that make a track again, in phases. A phase is one callback; the first makes the track, its properties, devices,
-- columns and slot mutes; the rest write lines (and the automation of those patterns), up to SLICE_WRITES each.
local function plan_phases(material)
  local phases, current, writes = {}, {}, 0
  for _, entry in ipairs(material.patterns or {}) do
    local cost = entry.writes or 1
    if writes > 0 and writes + cost > base.SLICE_WRITES then
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
    base.note_restored(env.journal, "track-delete", { track = index })
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
          job.step(base.STEP_GAP_MS)
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
      base.note_restored(env.journal, "track-delete", { track = index })
      return answer
    end,
  }
end

base.register({
  kind = "track-delete", custom = true, shape = "deleted",
  preview = delete_preview, apply = delete_apply, restore = delete_restore,
})

return M
