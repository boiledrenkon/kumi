-- A song for the mock Renoise: tracks (sequencer, group, master, send), patterns with lines, the sequence, instruments with
-- samples, phrases and macros, devices with parameters, the selection and the transport. Everything is plain data behind
-- strict objects (member names are checked against the API like the rest of the mock), and `M.ops` changes the data the
-- way the producer does in Renoise's GUI: each fires the observables Renoise would.
--
-- Values follow Renoise: notes 0..119 (48 = C-4, 60 = C-5), 120 = OFF, 121 = empty; instrument, volume and panning 255 =
-- empty; text as the tracker shows it ("C-4", "01", "..").

return function(M, object)

local NOTE_NAMES = { "C-", "C#", "D-", "D#", "E-", "F-", "F#", "G-", "G#", "A-", "A#", "B-" }

local function note_text(value)
  if value == 121 then return "---" end
  if value == 120 then return "OFF" end
  return NOTE_NAMES[value % 12 + 1] .. math.floor(value / 12)
end

local function hex2(value)
  return string.format("%02X", value)
end

local function out_of_range(what, index)
  error("'" .. what .. "' index out of range: " .. tostring(index), 3)
end

local function list(items)
  local copy = {}
  for i, item in ipairs(items) do copy[i] = item end
  return copy
end

-- Columns

local function note_column(data)
  return object("renoise.NoteColumn", { is_selected = false }, {
    note_value = function() return data.note end,
    note_string = function() return note_text(data.note) end,
    instrument_value = function() return data.instrument end,
    instrument_string = function() return data.instrument == 255 and ".." or hex2(data.instrument) end,
    volume_value = function() return data.volume end,
    volume_string = function() return data.volume == 255 and ".." or hex2(data.volume) end,
    panning_value = function() return data.panning end,
    panning_string = function() return data.panning == 255 and ".." or hex2(data.panning) end,
    delay_value = function() return data.delay end,
    delay_string = function() return data.delay == 0 and ".." or hex2(data.delay) end,
    effect_number_value = function() return data.fxn end,
    effect_number_string = function() return hex2(data.fxn) end,
    effect_amount_value = function() return data.fxa end,
    effect_amount_string = function() return hex2(data.fxa) end,
    is_empty = function()
      return data.note == 121 and data.instrument == 255 and data.volume == 255 and data.panning == 255
        and data.delay == 0 and data.fxn == 0 and data.fxa == 0
    end,
  })
end

local function effect_column(data)
  return object("renoise.EffectColumn", { is_selected = false }, {
    number_value = function() return data.number end,
    number_string = function() return hex2(data.number) end,
    amount_value = function() return data.amount end,
    amount_string = function() return hex2(data.amount) end,
    is_empty = function() return data.number == 0 and data.amount == 0 end,
  })
end

local function empty_note() return { note = 121, instrument = 255, volume = 255, panning = 255, delay = 0, fxn = 0, fxa = 0 } end

-- Notes are given as {note, instrument, volume, panning, delay, fxn, fxa} (any of them).
local function note_data(given)
  local data = empty_note()
  for k, v in pairs(given or {}) do data[k] = v end
  return data
end

-- A line of sparse data: { notes = {[column] = data}, effects = {[column] = data} }.
local function line(data)
  data = data or {}
  local function empty()
    for _, n in pairs(data.notes or {}) do
      if not note_column(n).is_empty then return false end
    end
    for _, e in pairs(data.effects or {}) do
      if e.number ~= 0 or e.amount ~= 0 then return false end
    end
    return true
  end
  return object("renoise.PatternLine", {
    note_column = function(_, c)
      if type(c) ~= "number" or c < 1 or c > 12 then out_of_range("note column", c) end
      return note_column((data.notes or {})[c] or empty_note())
    end,
    effect_column = function(_, c)
      if type(c) ~= "number" or c < 1 or c > 8 then out_of_range("effect column", c) end
      return effect_column((data.effects or {})[c] or { number = 0, amount = 0 })
    end,
  }, { is_empty = empty })
end

-- Devices

local function parameter(data)
  return object("renoise.DeviceParameter", {
    name = data.name, name_observable = data.name_observable or M.observable(),
    value_min = data.min or 0, value_max = data.max or 1, value_default = data.default or 0,
    value_observable = data.value_observable or M.observable(),
    value_string_observable = M.observable(),
  }, {
    value = function() return data.value end,
    value_string = function() return data.string and data.string(data.value) or string.format("%.2f", data.value) end,
  })
end

local function device(data)
  local params = {}
  for i, p in ipairs(data.params or {}) do params[i] = parameter(p) end
  -- (the proxy is made anew on every read: what is watched lives in the data)
  data.is_active_observable = data.is_active_observable or M.observable()
  data.display_name_observable = data.display_name_observable or M.observable()
  return object("renoise.AudioDevice", {
    name = data.name, display_name = data.display_name or data.name, short_name = data.name,
    device_path = data.path or ("Audio/Effects/Native/" .. data.name),
    is_active = data.active ~= false, is_active_observable = data.is_active_observable,
    display_name_observable = data.display_name_observable,
    active_preset = data.preset or 0, active_preset_observable = M.observable(),
    presets = data.presets or {},
    parameters = params,
    preset = function(_, i)
      local name = (data.presets or {})[i]
      if not name then out_of_range("preset", i) end
      return name
    end,
    parameter = function(_, i)
      if not params[i] then out_of_range("parameter", i) end
      return params[i]
    end,
  })
end

-- The observable of a note column's mute or name: one per column, kept in the track's data.
local function column_observable(t, kind, c)
  if type(c) ~= "number" or c < 1 or c > 12 or t.kind == "master" or t.kind == "send" then out_of_range("column", c) end
  t.column_observables = t.column_observables or {}
  local key = kind .. c
  t.column_observables[key] = t.column_observables[key] or M.observable()
  return t.column_observables[key]
end

-- A song

function M.make_song(options)
  options = options or {}
  local d = { options = options }
  M.data = d

  local function simple_observable() return M.observable() end

  -- Tracks: sequencer tracks (a group is one, after its members), then the master, then the sends.
  d.tracks = {}
  local sequencer_count = options.sequencer_tracks or 1
  local specs = options.tracks
  if specs then sequencer_count = #specs end
  for i = 1, sequencer_count do
    local spec = specs and specs[i] or {}
    d.tracks[i] = {
      kind = spec.kind or "sequencer", name = spec.name or string.format("Track %02d", i), color = spec.color,
      members = spec.members or 0, mute_state = spec.mute_state or 1, solo = spec.solo or false,
      volume = spec.volume or 1, panning = spec.panning or 0.5,
      note_columns = spec.note_columns or 1, effect_columns = spec.effect_columns or 1, devices = spec.devices or {},
    }
  end
  d.tracks[#d.tracks + 1] = { kind = "master", name = "Master", members = 0, mute_state = 1, volume = 1, panning = 0.5,
    note_columns = 1, effect_columns = 1, devices = {} }
  for i = 1, options.send_tracks or 0 do
    d.tracks[#d.tracks + 1] = { kind = "send", name = "Send " .. i, members = 0, mute_state = 1, volume = 1, panning = 0.5,
      note_columns = 1, effect_columns = 1, devices = {} }
  end
  for _, t in ipairs(d.tracks) do
    t.name_observable, t.color_observable, t.mute_state_observable = simple_observable(), simple_observable(), simple_observable()
    t.solo_state_observable, t.devices_observable = simple_observable(), simple_observable()
    t.visible_note_columns_observable, t.visible_effect_columns_observable = simple_observable(), simple_observable()
    t.volume_observable, t.panning_observable = simple_observable(), simple_observable()
  end

  -- Patterns
  d.patterns = {}
  local function new_pattern(lines, name)
    local pattern = {
      name = name or "", lines = lines or options.pattern_lines or 64, tracks = {},
      name_observable = simple_observable(), number_of_lines_observable = simple_observable(), notifiers = {},
    }
    d.patterns[#d.patterns + 1] = pattern
    return pattern
  end
  for _ = 1, options.patterns or 1 do new_pattern() end

  -- The sequence: pattern indices; sections and muted slots hang off the slot
  d.sequence = {}
  for s = 1, options.sequence_length or #d.patterns do d.sequence[s] = options.sequence and options.sequence[s] or ((s - 1) % #d.patterns + 1) end
  d.sections, d.muted = {}, {}

  -- Instruments
  d.instruments = {}
  local function new_instrument(spec)
    spec = spec or {}
    local instrument = {
      name = spec.name or "", samples = {}, phrases = {}, macros = {}, plugin = spec.plugin,
      name_observable = simple_observable(), samples_observable = simple_observable(), phrases_observable = simple_observable(),
    }
    for k = 1, 8 do instrument.macros[k] = { name = "Macro " .. k, value = 0, name_observable = simple_observable(), value_observable = simple_observable() } end
    for k = 1, spec.samples or 0 do
      instrument.samples[k] = { name = "Sample " .. k, base_note = 48, note_range = { 0, 119 }, velocity_range = { 0, 127 },
        loop_mode = 1, mute_group = 0 }
    end
    for k = 1, spec.phrases or 0 do
      instrument.phrases[k] = { name = "Phrase " .. k, lines = 16, lpb = 4, key_tracking = 1, looping = true,
        note_columns = 1, effect_columns = 1, data = {} }
    end
    d.instruments[#d.instruments + 1] = instrument
    return instrument
  end
  d.new_instrument = new_instrument
  for i = 1, options.instruments or 1 do new_instrument(options.instrument_specs and options.instrument_specs[i]) end

  -- Proxies. The song's own, built on every call from the data, so nothing is cached across a restructure.
  local song
  local track, pattern_object, instrument_object

  local function track_count() return #d.tracks end

  -- The pre-FX volume and panning are plain parameters: their value reads the data.
  local function prefx(t, field)
    local observable = field == "volume" and "volume_observable" or "panning_observable"
    return object("renoise.DeviceParameter", {
      name = field == "volume" and "Volume" or "Panning", name_observable = M.observable(),
      value_min = 0, value_max = field == "volume" and 1.4125375747681 or 1, value_default = field == "volume" and 1 or 0.5,
      value_observable = t[observable], value_string_observable = M.observable(),
    }, {
      value = function() return t[field == "volume" and "volume" or "panning"] end,
      value_string = function() return string.format("%.2f", t[field == "volume" and "volume" or "panning"]) end,
    })
  end

  -- The post-FX fader and pan and the pre-FX width: plain parameters too.
  local function level(t, key, default, max)
    return object("renoise.DeviceParameter", {
      name = key, name_observable = M.observable(), value_min = 0, value_max = max, value_default = default,
      value_observable = M.observable(), value_string_observable = M.observable(),
    }, {
      value = function() return t[key] or default end,
      value_string = function() return string.format("%.2f", t[key] or default) end,
    })
  end

  local KIND = { sequencer = 1, master = 2, send = 3, group = 4 }

  track = function(i)
    local t = type(i) == "number" and d.tracks[i]
    if not t then out_of_range("track", i) end
    local fields = {
      name_observable = t.name_observable, color_observable = t.color_observable,
      mute_state_observable = t.mute_state_observable, solo_state_observable = t.solo_state_observable,
      devices_observable = t.devices_observable,
      visible_note_columns_observable = t.visible_note_columns_observable,
      visible_effect_columns_observable = t.visible_effect_columns_observable,
      prefx_volume = prefx(t, "volume"), prefx_panning = prefx(t, "panning"),
      prefx_width = level(t, "width", 1, 1), postfx_volume = level(t, "post_volume", 1, 1.4125375747681),
      postfx_panning = level(t, "post_panning", 0.5, 1),
      column_is_muted_observable = function(_, c) return column_observable(t, "muted", c) end,
      column_name_observable = function(_, c) return column_observable(t, "name", c) end,
      device = function(_, k)
        if not t.devices[k] then out_of_range("device", k) end
        return device(t.devices[k])
      end,
    }
    local getters = {
      name = function() return t.name end,
      color = function() return t.color or M.NIL end,
      type = function() return KIND[t.kind] end,
      mute_state = function() return t.mute_state end,
      solo_state = function() return t.solo or false end,
      visible_note_columns = function() return t.note_columns end,
      visible_effect_columns = function() return t.effect_columns end,
      max_note_columns = function() return (t.kind == "master" or t.kind == "send") and 0 or 12 end,
      devices = function()
        local devices = {}
        for k, spec in ipairs(t.devices) do devices[k] = device(spec) end
        return devices
      end,
    }
    local class = "renoise.Track"
    if t.kind == "group" then
      class = "renoise.GroupTrack"
      getters.members = function()
        local members = {}
        for k = 1, t.members do members[k] = track(i - k) end
        return members
      end
    end
    -- Every track has a colour in Renoise (it assigns one), and a blend of how much of it shows.
    getters.color = function() return t.color or (t.kind == "master" and { 220, 220, 220 } or { 41, 166, 153 }) end
    getters.color_blend = function() return t.color_blend or 0 end
    return object(class, fields, getters)
  end

  local function pattern_track(p, t)
    local pattern = d.patterns[p]
    pattern.tracks[t] = pattern.tracks[t] or { alias = 0, lines = {} }
    local data = pattern.tracks[t]
    data.alias_observable = data.alias_observable or M.observable()
    return object("renoise.PatternTrack", {
      alias_pattern_index_observable = data.alias_observable,
      -- an alias shows (and writes to) the lines of the pattern it is an alias of
      line = function(_, k)
        if type(k) ~= "number" or k < 1 or k > pattern.lines then out_of_range("line", k) end
        local source = data.alias > 0 and d.patterns[data.alias] and d.patterns[data.alias].tracks[t] or data
        return line(source.lines[k])
      end,
    }, {
      alias_pattern_index = function() return data.alias end,
      is_alias = function() return data.alias > 0 end,
      is_empty = function()
        for _, l in pairs(data.lines) do
          if not line(l).is_empty then return false end
        end
        return true
      end,
    })
  end

  pattern_object = function(p)
    local pattern = type(p) == "number" and d.patterns[p]
    if not pattern then out_of_range("pattern", p) end
    return object("renoise.Pattern", {
      name_observable = pattern.name_observable, number_of_lines_observable = pattern.number_of_lines_observable,
      track = function(_, t)
        if type(t) ~= "number" or t < 1 or t > track_count() then out_of_range("pattern track", t) end
        return pattern_track(p, t)
      end,
      add_line_notifier = function(_, fn)
        for _, existing in ipairs(pattern.notifiers) do
          if existing == fn then error("the notifier was already added", 2) end
        end
        pattern.notifiers[#pattern.notifiers + 1] = fn
      end,
      remove_line_notifier = function(_, fn)
        for i, existing in ipairs(pattern.notifiers) do
          if existing == fn then table.remove(pattern.notifiers, i) return end
        end
        error("the notifier was not added", 2)
      end,
      has_line_notifier = function(_, fn)
        for _, existing in ipairs(pattern.notifiers) do
          if existing == fn then return true end
        end
        return false
      end,
    }, {
      name = function() return pattern.name end,
      number_of_lines = function() return pattern.lines end,
    })
  end

  -- (what is watched lives in the data: the proxies are made anew on every read)
  local function sample_object(i, s)
    local sample = d.instruments[i].samples[s]
    sample.observables = sample.observables or {}
    local watched = sample.observables
    for _, key in ipairs({ "name", "note_range", "velocity_range", "base_note", "layer", "map_key_to_pitch", "map_velocity_to_volume" }) do
      watched[key] = watched[key] or M.observable()
    end
    local mapping = object("renoise.SampleMapping", {
      base_note = sample.base_note, note_range = sample.note_range, velocity_range = sample.velocity_range,
      layer = sample.layer or 1, map_key_to_pitch = sample.map_key_to_pitch ~= false,
      map_velocity_to_volume = sample.map_velocity_to_volume ~= false,
      base_note_observable = watched.base_note, note_range_observable = watched.note_range,
      velocity_range_observable = watched.velocity_range, layer_observable = watched.layer,
      map_key_to_pitch_observable = watched.map_key_to_pitch, map_velocity_to_volume_observable = watched.map_velocity_to_volume,
    })
    return object("renoise.Sample", { name = sample.name, loop_mode = sample.loop_mode, mute_group = sample.mute_group,
      name_observable = watched.name, sample_mapping = mapping })
  end

  local function phrase_object(i, ph)
    local phrase = d.instruments[i].phrases[ph]
    phrase.notifiers = phrase.notifiers or {}
    phrase.observables = phrase.observables or {}
    local watched = phrase.observables
    for _, key in ipairs({ "name", "number_of_lines", "lpb", "visible_note_columns", "visible_effect_columns" }) do
      watched[key] = watched[key] or M.observable()
    end
    return object("renoise.InstrumentPhrase", {
      name = phrase.name, number_of_lines = phrase.lines, lpb = phrase.lpb, key_tracking = phrase.key_tracking,
      looping = phrase.looping, visible_note_columns = phrase.note_columns, visible_effect_columns = phrase.effect_columns,
      name_observable = watched.name, number_of_lines_observable = watched.number_of_lines, lpb_observable = watched.lpb,
      visible_note_columns_observable = watched.visible_note_columns,
      visible_effect_columns_observable = watched.visible_effect_columns,
      line = function(_, k)
        if type(k) ~= "number" or k < 1 or k > phrase.lines then out_of_range("phrase line", k) end
        return line(phrase.data[k])
      end,
      add_line_notifier = function(_, fn)
        for _, existing in ipairs(phrase.notifiers) do
          if existing == fn then error("the notifier was already added", 2) end
        end
        phrase.notifiers[#phrase.notifiers + 1] = fn
      end,
      remove_line_notifier = function(_, fn)
        for n, existing in ipairs(phrase.notifiers) do
          if existing == fn then table.remove(phrase.notifiers, n) return end
        end
        error("the notifier was not added", 2)
      end,
      has_line_notifier = function(_, fn)
        for _, existing in ipairs(phrase.notifiers) do
          if existing == fn then return true end
        end
        return false
      end,
    })
  end

  instrument_object = function(i)
    local instrument = type(i) == "number" and d.instruments[i]
    if not instrument then out_of_range("instrument", i) end
    return object("renoise.Instrument", {
      name_observable = instrument.name_observable, samples_observable = instrument.samples_observable,
      phrases_observable = instrument.phrases_observable,
      plugin_properties = object("renoise.InstrumentPluginProperties", {
        plugin_loaded = instrument.plugin ~= nil,
        plugin_device = instrument.plugin and device(instrument.plugin) or M.NIL,
      }),
      sample = function(_, s)
        if not instrument.samples[s] then out_of_range("sample", s) end
        return sample_object(i, s)
      end,
      phrase = function(_, ph)
        if not instrument.phrases[ph] then out_of_range("phrase", ph) end
        return phrase_object(i, ph)
      end,
      macro = function(_, k)
        if not instrument.macros[k] then out_of_range("macro", k) end
        local macro = instrument.macros[k]
        return object("renoise.InstrumentMacro", { name = macro.name, value = macro.value, name_observable = macro.name_observable,
          value_observable = macro.value_observable })
      end,
    }, {
      name = function() return instrument.name end,
      samples = function()
        local samples = {}
        for s in ipairs(instrument.samples) do samples[s] = sample_object(i, s) end
        return samples
      end,
      phrases = function()
        local phrases = {}
        for ph in ipairs(instrument.phrases) do phrases[ph] = phrase_object(i, ph) end
        return phrases
      end,
      macros = function()
        local macros = {}
        for k, macro in ipairs(instrument.macros) do
          macros[k] = object("renoise.InstrumentMacro", { name = macro.name, value = macro.value, name_observable = macro.name_observable,
            value_observable = macro.value_observable })
        end
        return macros
      end,
    })
  end

  d.sequence_observable, d.assignments_observable, d.mutes_observable =
    simple_observable(), simple_observable(), simple_observable()
  d.sections_observable = simple_observable()
  local sequencer = object("renoise.PatternSequencer", {
    pattern_sequence_observable = d.sequence_observable, pattern_assignments_observable = d.assignments_observable,
    pattern_slot_mutes_observable = d.mutes_observable,
    sequence_sections_changed_observable = function() return d.sections_observable end,
    pattern = function(_, s)
      if not d.sequence[s] then out_of_range("sequence", s) end
      return d.sequence[s]
    end,
    sequence_is_start_of_section = function(_, s)
      if not d.sequence[s] then out_of_range("sequence", s) end
      return d.sections[s] ~= nil
    end,
    sequence_section_name = function(_, s)
      if not d.sequence[s] then out_of_range("sequence", s) end
      return d.sections[s] or ""
    end,
    track_sequence_slot_is_muted = function(_, t, s)
      if not d.sequence[s] or not d.tracks[t] then out_of_range("slot", t .. "," .. s) end
      return d.muted[t .. "," .. s] == true
    end,
  }, {
    pattern_sequence = function() return list(d.sequence) end,
  })

  local transport_fields = {
    playing = options.playing or false, bpm = options.bpm or 120, lpb = options.lpb or 4, tpl = options.tpl or 6,
    edit_mode = false, follow_player = false, loop_pattern = false, loop_block_enabled = false,
    playback_pos_beats = 0, metronome_beats_per_bar = 0, loop_start_beats = 0, loop_end_beats = 16,
    loop_sequence_start = 0, loop_sequence_end = 0,
    playing_observable = simple_observable(), bpm_observable = simple_observable(), lpb_observable = simple_observable(),
    tpl_observable = simple_observable(), edit_mode_observable = simple_observable(),
    follow_player_observable = simple_observable(), loop_pattern_observable = simple_observable(),
    loop_block_enabled_observable = simple_observable(),
    playback_pos = object("renoise.SongPos", { sequence = 1, line = 1 }),
  }
  local transport = object("renoise.Transport", transport_fields)

  local song_fields = {
    name = options.name or "", name_observable = simple_observable(), file_name = options.file_name or "",
    tool_data = options.tool_data or M.NIL, rendering = false,
    transport = transport, sequencer = sequencer,
    sequencer_track_count = 0, send_track_count = 0,
    tracks_observable = simple_observable(), instruments_observable = simple_observable(),
    patterns_observable = simple_observable(),
    selected_track_index = 1, selected_instrument_index = 1, selected_pattern_index = 1, selected_sequence_index = 1,
    selected_line_index = 1, selected_note_column_index = 1, selected_effect_column_index = 0,
    selected_sample_index = 0, selected_phrase_index = 0, selected_track_device_index = 0,
    selected_phrase_line_index = 0, selection_in_pattern = M.NIL,
    selected_track_index_observable = simple_observable(), selected_instrument_index_observable = simple_observable(),
    selected_pattern_index_observable = simple_observable(), selected_sequence_index_observable = simple_observable(),
    selected_sample_observable = simple_observable(), selected_phrase_observable = simple_observable(),
    selected_track_device_observable = simple_observable(),
    track = function(_, i) return track(i) end,
    pattern = function(_, p) return pattern_object(p) end,
    instrument = function(_, i) return instrument_object(i) end,
  }
  local song_getters = {
    patterns = function()
      local patterns = {}
      for p in ipairs(d.patterns) do patterns[p] = pattern_object(p) end
      return patterns
    end,
    instruments = function()
      local instruments = {}
      for i in ipairs(d.instruments) do instruments[i] = instrument_object(i) end
      return instruments
    end,
  }
  d.song_fields = song_fields
  d.transport_fields = transport_fields
  function d.recount()
    local sequencers, sends = 0, 0
    for _, t in ipairs(d.tracks) do
      if t.kind == "sequencer" or t.kind == "group" then sequencers = sequencers + 1 end
      if t.kind == "send" then sends = sends + 1 end
    end
    song_fields.sequencer_track_count, song_fields.send_track_count = sequencers, sends
  end
  d.recount()
  song = object("renoise.Song", song_fields, song_getters)
  d.song = song
  return song
end

-- What the producer does. Each changes the data and fires what Renoise would.

local ops = {}
M.ops = ops

local function fire(observable, ...) M.fire(observable, ...) end

local function song_fields() return M.data.song_fields end

function ops.set_note(p, t, k, column, given)
  local pattern = M.data.patterns[p]
  pattern.tracks[t] = pattern.tracks[t] or { alias = 0, lines = {} }
  -- (written through an alias, it lands in the pattern the alias shows)
  local alias = pattern.tracks[t].alias
  if alias > 0 then
    p = alias
    pattern = M.data.patterns[p]
    pattern.tracks[t] = pattern.tracks[t] or { alias = 0, lines = {} }
  end
  local data = pattern.tracks[t]
  data.lines[k] = data.lines[k] or { notes = {}, effects = {} }
  data.lines[k].notes = data.lines[k].notes or {}
  data.lines[k].notes[column] = note_data(given)
  for _, fn in ipairs(list(pattern.notifiers)) do
    local ok, err = pcall(fn, { pattern = p, track = t, line = k })
    if not ok then M.errors[#M.errors + 1] = "notifier: " .. tostring(err) end
  end
end

function ops.set_effect(p, t, k, column, number, amount)
  local pattern = M.data.patterns[p]
  pattern.tracks[t] = pattern.tracks[t] or { alias = 0, lines = {} }
  local data = pattern.tracks[t]
  data.lines[k] = data.lines[k] or { notes = {}, effects = {} }
  data.lines[k].effects = data.lines[k].effects or {}
  data.lines[k].effects[column] = { number = number, amount = amount }
  for _, fn in ipairs(list(pattern.notifiers)) do pcall(fn, { pattern = p, track = t, line = k }) end
end

function ops.alias(p, t, of)
  local pattern = M.data.patterns[p]
  pattern.tracks[t] = pattern.tracks[t] or { alias = 0, lines = {} }
  pattern.tracks[t].alias = of
  pattern.tracks[t].alias_observable = pattern.tracks[t].alias_observable or M.observable()
  fire(pattern.tracks[t].alias_observable)
end

function ops.mute_slot(t, s, muted)
  M.data.muted[t .. "," .. s] = muted or nil
  fire(M.data.mutes_observable)
end

function ops.section(s, name)
  M.data.sections[s] = name
  fire(M.data.sections_observable)
end

function ops.mute_column(t, c, muted)
  local track = M.data.tracks[t]
  track.muted_columns = track.muted_columns or {}
  track.muted_columns[c] = muted or nil
  fire(column_observable(track, "muted", c))
end

function ops.name_column(t, c, name)
  local track = M.data.tracks[t]
  track.column_names = track.column_names or {}
  track.column_names[c] = name
  fire(column_observable(track, "name", c))
end

function ops.set_device(t, d, field, value)
  local data = M.data.tracks[t].devices[d]
  data[field] = value
  local key = (field == "active" and "is_active" or field) .. "_observable"
  data[key] = data[key] or M.observable()
  fire(data[key])
end

function ops.set_macro(i, k, value)
  local macro = M.data.instruments[i].macros[k]
  macro.value = value
  fire(macro.value_observable)
end

-- A sample's mapping: field is note_range, velocity_range, base_note, layer, map_key_to_pitch or map_velocity_to_volume.
function ops.set_mapping(i, s, field, value)
  local sample = M.data.instruments[i].samples[s]
  sample.observables = sample.observables or {}
  sample[field] = value
  sample.observables[field] = sample.observables[field] or M.observable()
  fire(sample.observables[field])
end

function ops.set_phrase_note(i, ph, k, column, given)
  local phrase = M.data.instruments[i].phrases[ph]
  phrase.data[k] = phrase.data[k] or { notes = {}, effects = {} }
  phrase.data[k].notes = phrase.data[k].notes or {}
  phrase.data[k].notes[column] = note_data(given)
  for _, fn in ipairs(list(phrase.notifiers or {})) do pcall(fn, { line = k }) end
end

-- A phrase's lines or lpb.
function ops.set_phrase(i, ph, field, value)
  local phrase = M.data.instruments[i].phrases[ph]
  phrase[field == "number_of_lines" and "lines" or field] = value
  phrase.observables = phrase.observables or {}
  phrase.observables[field] = phrase.observables[field] or M.observable()
  fire(phrase.observables[field])
end

local function track_spec(spec)
  spec = spec or {}
  local t = {
    kind = spec.kind or "sequencer", name = spec.name or "Track", color = spec.color, members = spec.members or 0,
    mute_state = 1, solo = false, volume = 1, panning = 0.5, note_columns = 1, effect_columns = 1, devices = spec.devices or {},
    name_observable = M.observable(), color_observable = M.observable(), mute_state_observable = M.observable(),
    solo_state_observable = M.observable(), devices_observable = M.observable(),
    visible_note_columns_observable = M.observable(), visible_effect_columns_observable = M.observable(),
    volume_observable = M.observable(), panning_observable = M.observable(),
  }
  return t
end

-- The data of every pattern follows its track: a track made at `index` has none, those after it move up by one.
local function move_pattern_tracks(shift)
  for _, pattern in ipairs(M.data.patterns) do
    local moved = {}
    for t, data in pairs(pattern.tracks) do
      local to = shift(t)
      if to then moved[to] = data end
    end
    pattern.tracks = moved
  end
end

function ops.insert_track(index, spec)
  table.insert(M.data.tracks, index, track_spec(spec))
  move_pattern_tracks(function(t) return t >= index and t + 1 or t end)
  M.data.recount()
  fire(song_fields().tracks_observable, { type = "insert", index = index })
end

function ops.remove_track(index)
  -- a group that held the track has one member fewer
  for g, t in ipairs(M.data.tracks) do
    if t.kind == "group" and g > index and index >= g - t.members then t.members = t.members - 1 end
  end
  table.remove(M.data.tracks, index)
  move_pattern_tracks(function(t) if t == index then return nil end return t > index and t - 1 or t end)
  M.data.recount()
  fire(song_fields().tracks_observable, { type = "remove", index = index })
end

function ops.swap_tracks(a, b)
  M.data.tracks[a], M.data.tracks[b] = M.data.tracks[b], M.data.tracks[a]
  move_pattern_tracks(function(t) if t == a then return b elseif t == b then return a end return t end)
  fire(song_fields().tracks_observable, { type = "swap", index = a, index2 = b })
end

function ops.rename_track(index, name)
  local t = M.data.tracks[index]
  t.name = name
  fire(t.name_observable)
end

function ops.mute_track(index, state)
  local t = M.data.tracks[index]
  t.mute_state = state
  fire(t.mute_state_observable)
end

function ops.insert_device(index, spec, at)
  local t = M.data.tracks[index]
  table.insert(t.devices, at or (#t.devices + 1), spec)
  fire(t.devices_observable, { type = "insert", index = at or #t.devices })
end

function ops.insert_pattern(lines)
  local data = M.data
  data.patterns[#data.patterns + 1] = {
    name = "", lines = lines or 64, tracks = {}, name_observable = M.observable(),
    number_of_lines_observable = M.observable(), notifiers = {},
  }
  fire(song_fields().patterns_observable, { type = "insert", index = #data.patterns })
  return #data.patterns
end

function ops.insert_sequence_slot(pattern_index, at)
  local data = M.data
  table.insert(data.sequence, at or (#data.sequence + 1), pattern_index)
  fire(data.sequence_observable)
end

function ops.rename_pattern(p, name)
  local pattern = M.data.patterns[p]
  pattern.name = name
  fire(pattern.name_observable)
end

function ops.insert_sample(i, spec)
  local instrument = M.data.instruments[i]
  local sample = { name = "Sample " .. (#instrument.samples + 1), base_note = 48, note_range = { 0, 119 },
    velocity_range = { 0, 127 }, loop_mode = 1, mute_group = 0 }
  for k, v in pairs(spec or {}) do sample[k] = v end
  instrument.samples[#instrument.samples + 1] = sample
  fire(instrument.samples_observable, { type = "insert", index = #instrument.samples })
end

function ops.insert_phrase(i)
  local instrument = M.data.instruments[i]
  instrument.phrases[#instrument.phrases + 1] = { name = "Phrase " .. (#instrument.phrases + 1), lines = 16, lpb = 4,
    key_tracking = 1, looping = true, note_columns = 1, effect_columns = 1, data = {} }
  fire(instrument.phrases_observable, { type = "insert", index = #instrument.phrases })
end

function ops.insert_instrument(spec)
  M.data.new_instrument(spec)
  fire(song_fields().instruments_observable, { type = "insert", index = #M.data.instruments })
end

function ops.remove_instrument(i)
  local instruments = M.data.instruments
  table.remove(instruments, i)
  fire(song_fields().instruments_observable, { type = "remove", index = i })
end

function ops.remove_pattern(p)
  table.remove(M.data.patterns, p)
  fire(song_fields().patterns_observable, { type = "remove", index = p })
end

function ops.remove_sample(i, s)
  local instrument = M.data.instruments[i]
  table.remove(instrument.samples, s)
  fire(instrument.samples_observable, { type = "remove", index = s })
end

function ops.remove_device(t, d)
  local track = M.data.tracks[t]
  table.remove(track.devices, d)
  fire(track.devices_observable, { type = "remove", index = d })
end

function ops.rename_instrument(i, name)
  local instrument = M.data.instruments[i]
  instrument.name = name
  fire(instrument.name_observable)
end

function ops.select(key, value)
  local fields = song_fields()
  fields[key] = value
  if fields[key .. "_observable"] then fire(fields[key .. "_observable"]) end
end

-- Selected sample, phrase and device have no index observable of their own in the API.
function ops.select_sample(value)
  song_fields().selected_sample_index = value
  fire(song_fields().selected_sample_observable)
end

function ops.select_phrase(value)
  song_fields().selected_phrase_index = value
  fire(song_fields().selected_phrase_observable)
end

function ops.select_device(value)
  song_fields().selected_track_device_index = value
  fire(song_fields().selected_track_device_observable)
end

function ops.transport(key, value)
  local fields = M.data.transport_fields
  fields[key] = value
  if fields[key .. "_observable"] then fire(fields[key .. "_observable"]) end
end

function ops.play_at(sequence, k, beats)
  local pos = M.impl(M.data.transport_fields.playback_pos)
  pos.sequence, pos.line = sequence, k
  M.data.transport_fields.playback_pos_beats = beats or 0
end

end
