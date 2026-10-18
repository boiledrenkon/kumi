-- The song as rows (contract §3). Everything here is addressed by index and looked up again where it is used: nothing
-- keeps a Renoise object from one slice of a job to the next (the song can change between slices).

local import = ...
local json = import("json")
local refs = import("refs")
local status = import("status")

local M = {}

local function fail(code, message)
  error({ code = code, message = message }, 0)
end

local function the_song()
  local song = renoise.song()
  if not song then fail("no-song", "Renoise has no song open") end
  return song
end

local function track_count(song)
  return song.sequencer_track_count + 1 + song.send_track_count
end

local function hex(color)
  if type(color) ~= "table" or not color[1] then return json.null end
  return string.format("#%02x%02x%02x", color[1], color[2], color[3])
end

local function db(linear)
  if type(linear) == "number" and linear > 0 then return 20 * math.log10(linear) end
  return json.null
end

---@return integer?
local function integer(value, name, low, high)
  if value == nil then return nil end
  if type(value) ~= "number" or value ~= math.floor(value) or value < low or value > high then
    fail("bad-params", name .. " must be a whole number from " .. low .. " to " .. high)
  end
  return value
end

local function array(items)
  return json.array(items)
end

-- Names for Renoise's enums, read from the API's own constants.
local function track_type(song_track)
  local T = renoise.Track
  local t = song_track.type
  if t == T.TRACK_TYPE_SEQUENCER then return "sequencer" end
  if t == T.TRACK_TYPE_GROUP then return "group" end
  if t == T.TRACK_TYPE_SEND then return "send" end
  return "master"
end

local function mute_name(state)
  local T = renoise.Track
  if state == T.MUTE_STATE_OFF then return "off" end
  if state == T.MUTE_STATE_MUTED then return "muted" end
  return "active"
end

local function loop_name(mode)
  local S = renoise.Sample
  if mode == S.LOOP_MODE_FORWARD then return "forward" end
  if mode == S.LOOP_MODE_REVERSE then return "reverse" end
  if mode == S.LOOP_MODE_PING_PONG then return "pingpong" end
  return "off"
end

local function key_tracking_name(mode)
  local P = renoise.InstrumentPhrase
  if mode == P.KEY_TRACKING_TRANSPOSE then return "transpose" end
  if mode == P.KEY_TRACKING_OFFSET then return "offset" end
  return "none"
end

-- Group tracks hold the tracks just before them in the song's order, and Renoise can't compare two track objects, so
-- who is in which group is worked out from the order and each group's member count: parent[i] = the group's index.
local function group_parents(song)
  local parents = {}
  local T = renoise.Track
  -- Marks the group's direct children (its `members` are all of its descendants, just before it) and returns the
  -- position before its whole subtree.
  local function place(group)
    local first = group - #song:track(group).members
    local at = group - 1
    while at >= first and at >= 1 do
      parents[at] = group
      if song:track(at).type == T.TRACK_TYPE_GROUP then at = place(at) else at = at - 1 end
    end
    return first - 1
  end
  local at = song.sequencer_track_count
  while at >= 1 do
    if song:track(at).type == T.TRACK_TYPE_GROUP then at = place(at) else at = at - 1 end
  end
  return parents
end

-- Rows

-- Set by the bridge: the song's `id` and `identity` (it knows the pid and which document this is).
---@type fun(song: renoise.Song): table
M.song_extra = function(_) return {} end

local function song_row(song)
  local transport = song.transport
  local path = song.file_name
  local row = {
    name = status.cap(song.name or ""),
    path = (path and path ~= "") and path or json.null,
    bpm = transport.bpm,
    lpb = transport.lpb,
    tpl = transport.tpl,
    patternCount = #song.patterns,
    sequenceLength = #song.sequencer.pattern_sequence,
    playing = transport.playing,
    editMode = transport.edit_mode,
    followPlayer = transport.follow_player,
  }
  for k, v in pairs(M.song_extra(song)) do row[k] = v end
  return row
end

local function track_row(song, epoch, i, parents)
  local track = song:track(i)
  local is_master = track.type == renoise.Track.TRACK_TYPE_MASTER
  local row = {
    ref = refs.make(epoch, "track", i),
    index = i,
    name = track.name,
    type = track_type(track),
    color = hex(track.color),
    colorBlend = track.color_blend,
    volumeDb = db(track.prefx_volume.value),
    pan = (track.prefx_panning.value - 0.5) * 2,
    deviceCount = #track.devices,
    noteColumns = track.visible_note_columns,
    effectColumns = track.visible_effect_columns,
    groupRef = parents and parents[i] and refs.make(epoch, "track", parents[i]) or json.null,
  }
  if not is_master then
    row.mute = mute_name(track.mute_state)
    row.solo = track.solo_state
  end
  return row
end

local function sequence_row(song, epoch, s, tracks)
  local sequencer = song.sequencer
  local muted = {}
  for t = 1, tracks do
    if sequencer:track_sequence_slot_is_muted(t, s) then muted[#muted + 1] = t end
  end
  return {
    ref = refs.make(epoch, "seq", s),
    patternRef = refs.make(epoch, "pattern", sequencer:pattern(s)),
    section = sequencer:sequence_is_start_of_section(s) and sequencer:sequence_section_name(s) or json.null,
    mutedTracks = array(muted),
  }
end

local function pattern_row(song, epoch, p, tracks)
  local pattern = song:pattern(p)
  local aliases = {}
  for t = 1, tracks do
    local alias = pattern:track(t).alias_pattern_index
    if alias > 0 then aliases[tostring(t)] = alias end
  end
  return {
    ref = refs.make(epoch, "pattern", p),
    name = pattern.name,
    lines = pattern.number_of_lines,
    aliases = next(aliases) and aliases or json.object(),
  }
end

local function macros_of(instrument)
  local macros = {}
  for k = 1, #instrument.macros do
    local macro = instrument:macro(k)
    macros[k] = { name = macro.name, value = macro.value }
  end
  return array(macros)
end

local function instrument_row(song, epoch, i)
  local instrument = song:instrument(i)
  local plugin = instrument.plugin_properties
  return {
    ref = refs.make(epoch, "instrument", i),
    index = i,
    name = instrument.name,
    sampleCount = #instrument.samples,
    phraseCount = #instrument.phrases,
    plugin = plugin.plugin_loaded and plugin.plugin_device.name or json.null,
    macros = macros_of(instrument),
  }
end

local function range(pair)
  return array({ pair[1], pair[2] })
end

local function sample_row(song, epoch, i, s)
  local sample = song:instrument(i):sample(s)
  local mapping = sample.sample_mapping
  return {
    ref = refs.make(epoch, "sample", i, s),
    index = s,
    name = sample.name,
    baseNote = mapping.base_note,
    noteRange = range(mapping.note_range),
    velocityRange = range(mapping.velocity_range),
    loop = loop_name(sample.loop_mode),
    muteGroup = sample.mute_group,
  }
end

local function phrase_row(song, epoch, i, p)
  local phrase = song:instrument(i):phrase(p)
  return {
    ref = refs.make(epoch, "phrase", i, p),
    index = p,
    name = phrase.name,
    lines = phrase.number_of_lines,
    lpb = phrase.lpb,
    keyTracking = key_tracking_name(phrase.key_tracking),
    looping = phrase.looping,
  }
end

local function device_row(song, epoch, t, d)
  local device = song:track(t):device(d)
  local path = device.device_path or ""
  local preset = device.active_preset
  return {
    ref = refs.make(epoch, "device", t, d),
    index = d,
    name = device.name,
    displayName = device.display_name,
    path = path,
    active = device.is_active,
    preset = (preset and preset > 0) and device:preset(preset) or json.null,
    paramCount = #device.parameters,
    plugin = path:find("^Audio/Effects/Native/") == nil,
    isDoofer = path:find("/Doofer$") ~= nil,
    isSplitter = path:find("/Splitter$") ~= nil,
  }
end

local function param_row(song, epoch, t, d, k)
  local parameter = song:track(t):device(d):parameter(k)
  return {
    ref = refs.make(epoch, "param", t, d, k),
    index = k,
    name = parameter.name,
    value = parameter.value,
    valueString = parameter.value_string,
    min = parameter.value_min,
    max = parameter.value_max,
  }
end

-- A pattern line (or a phrase's) as the tracker shows it, in Renoise's own text; nil when it is empty. Only columns
-- with something in them are listed, each with its number, and a field the tracker shows as ".." is left out.
local function line_row(line, number, note_columns, effect_columns)
  if line.is_empty then return nil end
  local notes, effects = {}, {}
  for c = 1, note_columns do
    local column = line:note_column(c)
    if not column.is_empty then
      local note = { col = c, note = column.note_string }
      local text = column.instrument_string
      if text ~= ".." then note.instrument = text end
      text = column.volume_string
      if text ~= ".." then note.volume = text end
      text = column.panning_string
      if text ~= ".." then note.panning = text end
      text = column.delay_string
      if text ~= ".." then note.delay = text end
      if column.effect_number_value ~= 0 or column.effect_amount_value ~= 0 then
        note.fx = column.effect_number_string .. column.effect_amount_string
      end
      notes[#notes + 1] = note
    end
  end
  for c = 1, effect_columns do
    local column = line:effect_column(c)
    if not column.is_empty then effects[#effects + 1] = { col = c, number = column.number_string, amount = column.amount_string } end
  end
  if #notes == 0 and #effects == 0 then return nil end
  local row = { line = number }
  if #notes > 0 then row.notes = array(notes) end
  if #effects > 0 then row.effects = array(effects) end
  return row
end

M.fail = fail
M.the_song = the_song
M.track_count = track_count
M.hex = hex
M.integer = integer
M.array = array
M.track_type = track_type
M.key_tracking_name = key_tracking_name
M.group_parents = group_parents
M.song_row = song_row
M.track_row = track_row
M.sequence_row = sequence_row
M.pattern_row = pattern_row
M.instrument_row = instrument_row
M.sample_row = sample_row
M.phrase_row = phrase_row
M.device_row = device_row
M.param_row = param_row
M.line_row = line_row

return M
