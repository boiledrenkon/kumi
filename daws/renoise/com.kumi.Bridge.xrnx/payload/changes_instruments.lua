-- Kumi's changes to an instrument (M4): its name and macros, a sample's settings and its keyzone. Each is a property of an object
-- found by its ref (see changes_base.lua for how a change goes): an instrument is `E:instrument:i`, a sample `E:sample:i.s`. The
-- refs go stale when the instruments, or the instrument's samples, are made, deleted or moved: nothing here is a number kept
-- across that.

local import = ...
local refs = import("refs")
local base = import("changes_base")

local bad, fail = base.bad, base.fail
local q, close = base.q, base.close
local M = {}

local function instrument_label(i, instrument)
  local name = instrument.name
  if name == nil or name == "" then return string.format("Instrument %02X", i - 1) end
  return name
end

local function instrument_of(song, env, reference, what)
  if type(reference) ~= "string" then bad(what .. ": ref must be an instrument ref") end
  local i = refs.expect(reference, "instrument", 1, env.epoch())[1]
  if i < 1 or i > #song.instruments then fail("gone", "that instrument isn't in the song any more") end
  return i, song:instrument(i)
end

local function sample_of(song, env, reference, what)
  if type(reference) ~= "string" then bad(what .. ": ref must be a sample ref") end
  local numbers = refs.expect(reference, "sample", 2, env.epoch())
  local i, s = numbers[1], numbers[2]
  if i < 1 or i > #song.instruments then fail("gone", "that instrument isn't in the song any more") end
  local instrument = song:instrument(i)
  if s < 1 or s > #instrument.samples then fail("gone", "that sample isn't in the instrument any more") end
  local sample = instrument:sample(s)
  local name = sample.name ~= "" and sample.name or string.format("Sample %02X", s - 1)
  return i, s, sample, string.format("%s in %s", q(name), q(instrument_label(i, instrument)))
end

local function instrument_target(env, i) return { ref = refs.make(env.epoch(), "instrument", i), kind = "instrument" } end
local function sample_target(env, i, s) return { ref = refs.make(env.epoch(), "sample", i, s), kind = "sample" } end

-- instrument-rename

base.register({
  kind = "instrument-rename", noun = "name", undoable = true,
  locate = function(song, env, input)
    local i, instrument = instrument_of(song, env, input.ref, "instrument-rename")
    return { object = instrument, name = instrument_label(i, instrument), place = { instrument = i }, target = instrument_target(env, i) }
  end,
  reread = function(song, place)
    if place.instrument > #song.instruments then return nil end
    return song:instrument(place.instrument)
  end,
  read = function(instrument) return { name = instrument.name } end,
  wanted = function(_, input)
    if type(input.name) ~= "string" or #input.name > 256 or input.name:find("[%c]") then bad("name must be one line of text") end
    return { name = input.name }
  end,
  write = function(instrument, state) instrument.name = state.name end,
  same = function(a, b) return a.name == b.name end,
  title = function(_, from, to)
    if from.name == "" then return "Named instrument " .. q(to.name) end
    return "Renamed instrument " .. q(from.name) .. " \226\134\146 " .. q(to.name)
  end,
})

-- instrument-macro: one of an instrument's eight macros, its value (0 to 1) and its name

base.register({
  kind = "instrument-macro", noun = "macro", undoable = true,
  locate = function(song, env, input)
    local i, instrument = instrument_of(song, env, input.ref, "instrument-macro")
    local k = input.macro
    if type(k) ~= "number" or k ~= math.floor(k) or k < 1 or k > 8 then bad("macro is a number from 1 to 8") end
    return {
      object = instrument:macro(k), name = string.format("%s macro %d", instrument_label(i, instrument), k),
      place = { instrument = i, macro = k }, target = instrument_target(env, i),
    }
  end,
  reread = function(song, place)
    if place.instrument > #song.instruments then return nil end
    return song:instrument(place.instrument):macro(place.macro)
  end,
  read = function(macro) return { value = macro.value, name = macro.name } end,
  wanted = function(macro, input)
    local value, name = macro.value, macro.name
    if input.value ~= nil then
      if type(input.value) ~= "number" or input.value ~= input.value or input.value < 0 or input.value > 1 then bad("value must be from 0 to 1") end
      value = input.value
    end
    if input.name ~= nil then
      if type(input.name) ~= "string" or #input.name > 64 or input.name:find("[%c]") then bad("name must be one line of text") end
      name = input.name
    end
    if input.value == nil and input.name == nil then bad("give value (0 to 1) or name") end
    return { value = value, name = name }
  end,
  write = function(macro, state)
    if macro.value ~= state.value then macro.value = state.value end
    if macro.name ~= state.name then macro.name = state.name end
  end,
  same = function(a, b) return close(a.value, b.value) and a.name == b.name end,
  title = function(name, from, to)
    if from.name ~= to.name and close(from.value, to.value) then return string.format("%s named %s", name, q(to.name)) end
    return string.format("%s %d%%", name, math.floor(to.value * 100 + 0.5))
  end,
})

-- sample-set: a sample's settings, the ones given

local LOOPS = { off = 1, forward = 2, reverse = 3, pingpong = 4 }
local NNAS = { cut = 1, off = 2, sustain = 3 }
local function name_of(table_, value)
  for name, v in pairs(table_) do
    if v == value then return name end
  end
  return value
end

-- Each setting: how it is read and written (Renoise's field, and to and from what Kumi says), and what it may be.
local SAMPLE = {
  volumeDb = {
    field = "volume", from = function(v) return base.db_of(v) or -200 end, to = function(db) return db <= -200 and 0 or 10 ^ (db / 20) end,
    check = function(v) return type(v) == "number" and v <= 12.04 end, says = "volumeDb must be a number up to 12 (-200 or less is silence)",
    text = function(v) return "volume " .. base.db_text(v > -200 and v or nil) .. " dB" end,
  },
  panning = {
    field = "panning", from = function(v) return v * 2 - 1 end, to = function(p) return (p + 1) / 2 end,
    check = function(v) return type(v) == "number" and v >= -1 and v <= 1 end, says = "panning is from -1 (left) to 1 (right)",
    text = function(v) return "pan " .. base.pan_text(v) end,
  },
  transpose = {
    field = "transpose", check = function(v) return type(v) == "number" and v == math.floor(v) and v >= -120 and v <= 120 end,
    says = "transpose is whole semitones from -120 to 120", text = function(v) return "transpose " .. v end,
  },
  fineTune = {
    field = "fine_tune", check = function(v) return type(v) == "number" and v == math.floor(v) and v >= -127 and v <= 127 end,
    says = "fineTune is from -127 to 127", text = function(v) return "fine tune " .. v end,
  },
  loop = {
    field = "loop_mode", from = function(v) return name_of(LOOPS, v) end, to = function(name) return LOOPS[name] end,
    check = function(v) return LOOPS[v] ~= nil end, says = "loop is off, forward, reverse or pingpong", text = function(v) return "loop " .. v end,
  },
  beatSync = {
    field = "beat_sync_enabled", check = function(v) return type(v) == "boolean" end, says = "beatSync is true or false",
    text = function(v) return v and "beat sync on" or "beat sync off" end,
  },
  beatSyncLines = {
    field = "beat_sync_lines", check = function(v) return type(v) == "number" and v == math.floor(v) and v >= 1 and v <= 512 end,
    says = "beatSyncLines is from 1 to 512", text = function(v) return "beat sync " .. v .. " lines" end,
  },
  nna = {
    field = "new_note_action", from = function(v) return name_of(NNAS, v) end, to = function(name) return NNAS[name] end,
    check = function(v) return NNAS[v] ~= nil end, says = "nna is cut, off or sustain", text = function(v) return "new note " .. v end,
  },
  muteGroup = {
    field = "mute_group", check = function(v) return type(v) == "number" and v == math.floor(v) and v >= 0 and v <= 15 end,
    says = "muteGroup is from 0 (none) to 15", text = function(v) return v == 0 and "no mute group" or ("mute group " .. v) end,
  },
  oneshot = { field = "oneshot", check = function(v) return type(v) == "boolean" end, says = "oneshot is true or false", text = function(v) return v and "one-shot" or "not one-shot" end },
  autofade = { field = "autofade", check = function(v) return type(v) == "boolean" end, says = "autofade is true or false", text = function(v) return v and "autofade on" or "autofade off" end },
}

-- (a setting's state names only the settings asked, in their order, for the title)
local ORDER = { "volumeDb", "panning", "transpose", "fineTune", "loop", "beatSync", "beatSyncLines", "nna", "muteGroup", "oneshot", "autofade" }

local function settings(table_, object, source)
  local out = {}
  for _, key in ipairs(ORDER) do
    local spec = table_[key]
    if spec and type(source) == "table" and source[key] ~= nil then
      local value = object[spec.field]
      out[key] = spec.from and spec.from(value) or value
    end
  end
  return out
end

local function settings_wanted(table_, input, what)
  local out, any = {}, false
  for _, key in ipairs(ORDER) do
    local spec = table_[key]
    if spec and input[key] ~= nil then
      if not spec.check(input[key]) then bad(spec.says) end
      out[key], any = input[key], true
    end
  end
  if not any then bad("give at least one of " .. what) end
  return out
end

local function settings_write(table_, object, state)
  for key, value in pairs(state) do
    local spec = table_[key]
    if spec then object[spec.field] = spec.to and spec.to(value) or value end
  end
end

local function settings_same(a, b)
  for key, value in pairs(a) do
    if type(value) == "number" then
      if not close(value, b[key]) then return false end
    elseif value ~= b[key] then
      return false
    end
  end
  for key in pairs(b) do
    if a[key] == nil then return false end
  end
  return true
end

local function settings_title(table_, name, to)
  local parts = {}
  for _, key in ipairs(ORDER) do
    if table_[key] and to[key] ~= nil then parts[#parts + 1] = table_[key].text(to[key]) end
  end
  return name .. ": " .. table.concat(parts, ", ")
end

local SAMPLE_KEYS = "volumeDb, panning, transpose, fineTune, loop, beatSync, beatSyncLines, nna, muteGroup, oneshot, autofade"

base.register({
  kind = "sample-set", noun = "settings", undoable = true,
  locate = function(song, env, input)
    local i, s, sample, name = sample_of(song, env, input.ref, "sample-set")
    return { object = sample, name = name, place = { instrument = i, sample = s }, target = sample_target(env, i, s) }
  end,
  reread = function(song, place)
    if place.instrument > #song.instruments then return nil end
    local instrument = song:instrument(place.instrument)
    if place.sample > #instrument.samples then return nil end
    return instrument:sample(place.sample)
  end,
  read = function(sample, source) return settings(SAMPLE, sample, source) end,
  wanted = function(_, input) return settings_wanted(SAMPLE, input, SAMPLE_KEYS) end,
  write = function(sample, state) settings_write(SAMPLE, sample, state) end,
  same = settings_same,
  title = function(name, _, to) return settings_title(SAMPLE, name, to) end,
})

-- keyzone-set: where a sample plays in its instrument's keyzones

local function range_check(low, high)
  return function(v)
    return type(v) == "table" and type(v[1]) == "number" and type(v[2]) == "number" and v[1] == math.floor(v[1])
      and v[2] == math.floor(v[2]) and v[1] >= low and v[2] <= high and v[1] <= v[2]
  end
end
local LAYERS = { ["note-on"] = 1, ["note-off"] = 2 }
local function range_text(v) return v[1] .. "-" .. v[2] end

local ZONE = {
  noteRange = { field = "note_range", check = range_check(0, 119), says = "noteRange is [low, high] notes from 0 to 119", text = function(v) return "notes " .. range_text(v) end },
  velocityRange = { field = "velocity_range", check = range_check(0, 127), says = "velocityRange is [low, high] from 0 to 127", text = function(v) return "velocity " .. range_text(v) end },
  baseNote = {
    field = "base_note", check = function(v) return type(v) == "number" and v == math.floor(v) and v >= 0 and v <= 119 end,
    says = "baseNote is a note from 0 to 119", text = function(v) return "base note " .. v end,
  },
  layer = {
    field = "layer", from = function(v) return name_of(LAYERS, v) end, to = function(name) return LAYERS[name] end,
    check = function(v) return LAYERS[v] ~= nil end, says = "layer is note-on or note-off", text = function(v) return "plays on " .. v end,
  },
  keyTracking = { field = "map_key_to_pitch", check = function(v) return type(v) == "boolean" end, says = "keyTracking is true or false", text = function(v) return v and "pitch follows the key" or "fixed pitch" end },
  velocityToVolume = { field = "map_velocity_to_volume", check = function(v) return type(v) == "boolean" end, says = "velocityToVolume is true or false", text = function(v) return v and "velocity to volume" or "fixed volume" end },
}
for _, key in ipairs({ "noteRange", "velocityRange", "baseNote", "layer", "keyTracking", "velocityToVolume" }) do ORDER[#ORDER + 1] = key end

-- (ranges are compared and kept as copies: Renoise answers its own tables)
local function zone_read(mapping, source)
  local out = settings(ZONE, mapping, source)
  for _, key in ipairs({ "noteRange", "velocityRange" }) do
    if out[key] then out[key] = { out[key][1], out[key][2] } end
  end
  return out
end

local function zone_same(a, b)
  local plain_a, plain_b = {}, {}
  for key, value in pairs(a) do plain_a[key] = type(value) == "table" and range_text(value) or value end
  for key, value in pairs(b) do plain_b[key] = type(value) == "table" and range_text(value) or value end
  return settings_same(plain_a, plain_b)
end

base.register({
  kind = "keyzone-set", noun = "keyzone", undoable = true,
  locate = function(song, env, input)
    local i, s, sample, name = sample_of(song, env, input.ref, "keyzone-set")
    local mapping = sample.sample_mapping
    if mapping.read_only then bad("that sample is a slice: its keyzone follows the sample it is sliced from") end
    return { object = mapping, name = name, place = { instrument = i, sample = s }, target = sample_target(env, i, s) }
  end,
  reread = function(song, place)
    if place.instrument > #song.instruments then return nil end
    local instrument = song:instrument(place.instrument)
    if place.sample > #instrument.samples then return nil end
    return instrument:sample(place.sample).sample_mapping
  end,
  read = zone_read,
  wanted = function(_, input) return settings_wanted(ZONE, input, "noteRange, velocityRange, baseNote, layer, keyTracking, velocityToVolume") end,
  write = function(mapping, state) settings_write(ZONE, mapping, state) end,
  same = zone_same,
  title = function(name, _, to) return settings_title(ZONE, name .. " keyzone", to) end,
})

return M
