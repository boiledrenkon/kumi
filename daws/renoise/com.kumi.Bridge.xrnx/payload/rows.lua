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

local function layer_name(layer)
  local I = renoise.Instrument
  if layer == I.LAYER_NOTE_OFF then return "note-off" end
  if layer == I.LAYER_NOTE_DISABLED then return "disabled" end
  return "note-on"
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
  M.song_settings(song, row)
  return row
end

-- M4 (n-song): the settings Kumi's song kinds change, as they take them.
local MOST_COMMENTS_SHOWN = 1000
function M.song_settings(song, row)
  local transport = song.transport
  local amounts = {}
  for i, v in ipairs(transport.groove_amounts) do amounts[i] = v end
  row.groove = { on = transport.groove_enabled, amounts = array(amounts) }
  row.metronome = {
    on = transport.metronome_enabled, beatsPerBar = transport.metronome_beats_per_bar, linesPerBeat = transport.metronome_lines_per_beat,
    precount = transport.metronome_precount_enabled and transport.metronome_precount_bars or false, volumeDb = db(transport.metronome_volume),
  }
  row.editing = {
    editStep = transport.edit_step, octave = transport.octave,
    quantize = transport.record_quantize_enabled and transport.record_quantize_lines or false,
    wrap = transport.wrapped_pattern_edit, singleTrack = transport.single_track_edit_mode,
    keyboardVelocity = transport.keyboard_velocity_enabled and transport.keyboard_velocity or false,
    recordTo = transport.record_parameter_mode == renoise.Transport.RECORD_PARAMETER_MODE_AUTOMATION and "automation" or "pattern",
  }
  row.headroomDb = db(transport.track_headroom)
  row.artist = status.cap(song.artist or "")
  local comments = table.concat(song.comments, "\n")
  if #comments > MOST_COMMENTS_SHOWN then
    -- (cut on a character's start: the text stays UTF-8)
    local cut = MOST_COMMENTS_SHOWN
    while cut > 0 and comments:byte(cut + 1) and comments:byte(cut + 1) >= 0x80 and comments:byte(cut + 1) < 0xC0 do cut = cut - 1 end
    row.comments, row.commentsCut = comments:sub(1, cut), true
  else
    row.comments = comments
  end
end

-- The note columns of a track that are muted (their numbers) and the ones that are named ({"2": "Pad"}); nil for none.
local function column_state(track)
  local muted, names
  for c = 1, track.max_note_columns do
    if track:column_is_muted(c) then
      muted = muted or {}
      muted[#muted + 1] = c
    end
    local name = track:column_name(c)
    if name ~= "" then
      names = names or {}
      names[tostring(c)] = name
    end
  end
  return muted and array(muted) or nil, names
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
    -- (volumeDb and pan are the pre-FX fader and pan, as they were; the post-FX ones are the fader a producer rides)
    volumeDb = db(track.prefx_volume.value),
    pan = (track.prefx_panning.value - 0.5) * 2,
    preVolumeDb = db(track.prefx_volume.value),
    prePan = (track.prefx_panning.value - 0.5) * 2,
    preWidth = track.prefx_width.value,
    postVolumeDb = db(track.postfx_volume.value),
    postPan = (track.postfx_panning.value - 0.5) * 2,
    deviceCount = #track.devices,
    noteColumns = track.visible_note_columns,
    effectColumns = track.visible_effect_columns,
    groupRef = parents and parents[i] and refs.make(epoch, "track", parents[i]) or json.null,
  }
  if not is_master then
    row.mute = mute_name(track.mute_state)
    row.solo = track.solo_state
  end
  row.mutedColumns, row.columnNames = column_state(track)
  M.track_settings(track, row)
  return row
end

-- M4 (n-song): what Kumi's track kinds change, as they take it: the collapse, the width in percent, the output routing and
-- delay, and which sub-columns show.
function M.track_settings(track, row)
  local T = renoise.Track
  row.collapsed = track.collapsed
  if track.type == T.TRACK_TYPE_GROUP then row.groupCollapsed = track.group_collapsed end
  row.width = track.prefx_width.value / track.prefx_width.value_max * 100
  row.outputRouting = track.output_routing
  if track.type == T.TRACK_TYPE_SEQUENCER then row.outputDelay = track.output_delay end
  if track.type == T.TRACK_TYPE_SEQUENCER and track.max_note_columns > 0 then
    row.subColumns = {
      volume = track.volume_column_visible, panning = track.panning_column_visible, delay = track.delay_column_visible,
      sampleEffects = track.sample_effects_column_visible,
    }
  end
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
  local aliases, colors = {}, {}
  for t = 1, tracks do
    local pattern_track = pattern:track(t)
    local alias = pattern_track.alias_pattern_index
    if alias > 0 then aliases[tostring(t)] = alias end
    -- M4 (n-song): the slot colours of the pattern's tracks in the matrix
    local color = pattern_track.color
    if color then colors[tostring(t)] = hex(color) end
  end
  local row = {
    ref = refs.make(epoch, "pattern", p),
    name = pattern.name,
    lines = pattern.number_of_lines,
    aliases = next(aliases) and aliases or json.object(),
  }
  if next(colors) then row.colors = colors end
  return row
end

local function macros_of(instrument)
  local macros = {}
  for k = 1, #instrument.macros do
    local macro = instrument:macro(k)
    macros[k] = { name = macro.name, value = macro.value }
  end
  return array(macros)
end

local function midi_type_name(kind)
  local P = renoise.InstrumentMidiOutputProperties
  if kind == P.TYPE_LINE_IN_RET then return "line-in" end
  if kind == P.TYPE_INTERNAL then return "internal" end
  return "external"
end

-- What the instrument plays through: its samples, a plug-in, or a MIDI port (its settings under `midi`).
local function instrument_row(song, epoch, i)
  local instrument = song:instrument(i)
  local plugin = instrument.plugin_properties
  local out = instrument.midi_output_properties
  local kind = "sample"
  if plugin.plugin_loaded then
    kind = "plugin"
  elseif instrument.active_tab == renoise.Instrument.TAB_EXT_MIDI then
    kind = "midi"
  end
  local row = {
    ref = refs.make(epoch, "instrument", i),
    index = i,
    name = instrument.name,
    kind = kind,
    sampleCount = #instrument.samples,
    phraseCount = #instrument.phrases,
    plugin = plugin.plugin_loaded and plugin.plugin_device.name or json.null,
    macros = macros_of(instrument),
  }
  if kind == "midi" then
    row.midi = {
      device = out.device_name, type = midi_type_name(out.instrument_type), channel = out.channel, program = out.program,
      bank = out.bank, transpose = out.transpose, delay = out.delay, duration = out.duration,
    }
  end
  return row
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
    layer = layer_name(mapping.layer),
    mapKeyToPitch = mapping.map_key_to_pitch,
    mapVelocityToVolume = mapping.map_velocity_to_volume,
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
    -- (the native ones only: a plug-in of that name is not a container; the paths are Renoise's, measured in 3.5.4)
    isDoofer = path == "Audio/Effects/Native/Doofer",
    isSplitter = path == "Audio/Effects/Native/Splitter",
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

local MAX_NOTE_COLUMNS, MAX_EFFECT_COLUMNS = 12, 8

-- The columns with something in them, in Renoise's own text: note columns first..last, then effect columns.
local function filled_columns(line, first_note, last_note, first_effect, last_effect)
  local notes, effects = {}, {}
  for c = first_note, last_note do
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
  for c = first_effect, last_effect do
    local column = line:effect_column(c)
    if not column.is_empty then effects[#effects + 1] = { col = c, number = column.number_string, amount = column.amount_string } end
  end
  return notes, effects
end

-- A pattern line (or a phrase's) as the tracker shows it, in Renoise's own text; nil when it is empty. Only columns
-- with something in them are listed, each with its number, and a field the tracker shows as ".." is left out.
-- Data in a column the track doesn't show is under `hidden` ({notes?, effects?}). A line with nothing in the shown columns
-- is looked through for it, since the line isn't empty; one that has something is only when `scan_hidden` (reading every
-- column of every line costs about six times as much).
local function line_row(line, number, note_columns, effect_columns, scan_hidden)
  if line.is_empty then return nil end
  local notes, effects = filled_columns(line, 1, note_columns, 1, effect_columns)
  local row = { line = number }
  if #notes > 0 then row.notes = array(notes) end
  if #effects > 0 then row.effects = array(effects) end
  if scan_hidden or (#notes == 0 and #effects == 0) then
    local hidden_notes, hidden_effects =
      filled_columns(line, note_columns + 1, MAX_NOTE_COLUMNS, effect_columns + 1, MAX_EFFECT_COLUMNS)
    if #hidden_notes > 0 or #hidden_effects > 0 then
      row.hidden = {}
      if #hidden_notes > 0 then row.hidden.notes = array(hidden_notes) end
      if #hidden_effects > 0 then row.hidden.effects = array(hidden_effects) end
    end
  end
  if row.notes == nil and row.effects == nil and row.hidden == nil then return nil end
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
M.column_state = column_state

return M
