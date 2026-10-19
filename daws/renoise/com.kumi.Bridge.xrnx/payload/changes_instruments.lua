-- Kumi's changes to the song's instruments (M4): an instrument's name, settings and macros; making, deleting, loading,
-- duplicating and moving instruments; and saving one to a file (an action, with no undo). See changes_base.lua for how a
-- change goes and instruments.lua for how a record finds its instrument again: by its place, followed through the
-- instruments made, deleted and swapped since (Renoise renumbers the notes that play them as it does: lab 2026-10-19), never
-- by its number alone.

local import = ...
local refs = import("refs")
local json = import("json")
local rows = import("rows")
local base = import("changes_base")
local places_ = import("instruments")

local bad, fail = base.bad, base.fail
local q, close = base.q, base.close
local M = {}

-- How many reads a look through the song for the notes of an instrument takes in one callback (about half a second); a bigger
-- song is looked through in slices first, and the change is written in its own callback after them.
M.SCAN_BUDGET = 300000
-- The most an instrument Kumi deletes may hold (its samples' bytes, as Renoise keeps them in memory), so that saving it and
-- loading it back stay well inside one callback (measured: see the notes).
M.MAX_INSTRUMENT_BYTES = 96 * 1024 * 1024
-- The biggest file Kumi loads in one callback.
M.MAX_LOAD_BYTES = 96 * 1024 * 1024
-- The most swaps one move makes (each renumbers the notes of the whole song).
M.MAX_MOVE_SWAPS = 64

-- Property kinds of an instrument (and its macros): found by their place on a restore.

local function instrument_spec(spec)
  local of = spec.of or "instrument"
  spec.by_place = true
  spec.locate = function(song, env, input)
    local n, objects, label = places_.locate(song, env, of, input, spec.kind)
    local object = spec.object and spec.object(objects, input) or objects[of]
    local loc = {
      object = object, name = spec.label and spec.label(label, input) or label, place = places_.journal_place(of, n),
      target = places_.target_of(env, of, n), objects = objects, numbers = n,
    }
    -- (what in the object the change is about, kept with its place: a macro's number)
    if spec.decorate then spec.decorate(loc, input) end
    return loc
  end
  -- (what the change writes is watched for the producer's undo of it)
  spec.watch = spec.watch or function(env, loc)
    if not (spec.observables and env.watch_undo) then return end
    for _, observable in ipairs(spec.observables(loc.object)) do env.watch_undo(observable) end
  end
  spec.reread = spec.reread or function(song, place)
    local found = places_.reread(of)(song, place)
    if found and spec.object then
      local objects = { [of] = found }
      if of ~= "instrument" then objects.instrument = places_.reread("instrument")(song, place) end
      local ok, object = pcall(spec.object, objects, place)
      return ok and object or nil
    end
    return found
  end
  base.register(spec)
end
M.instrument_spec = instrument_spec

local function one_line(value, most, what)
  if type(value) ~= "string" or #value > most or value:find("[%c]") then bad(what .. " must be one line of text (at most " .. most .. " bytes)") end
  return value
end

-- instrument-rename

instrument_spec({
  kind = "instrument-rename", noun = "name", undoable = true,
  read = function(instrument) return { name = instrument.name } end,
  observables = function(instrument) return { instrument.name_observable } end,
  wanted = function(_, input) return { name = one_line(input.name, 256, "name") } end,
  write = function(instrument, state) instrument.name = state.name end,
  same = function(a, b) return a.name == b.name end,
  title = function(_, from, to)
    if from.name == "" then return "Named instrument " .. q(to.name) end
    return "Renamed instrument " .. q(from.name) .. " \226\134\146 " .. q(to.name)
  end,
})

-- Settings: a table of fields, each read and written through Renoise's (`get`, `set`), checked (`check`, said by `says`),
-- named in the title (`text`). A change names only the fields given, in this order.

local function settings_kind(spec)
  local order, fields = spec.order, spec.fields
  spec.read = function(object, source)
    local out = {}
    for _, key in ipairs(order) do
      if type(source) == "table" and source[key] ~= nil then out[key] = fields[key].get(object) end
    end
    return out
  end
  spec.wanted = spec.wanted or function(object, input)
    local out, any = {}, false
    for _, key in ipairs(order) do
      if input[key] ~= nil then
        local field = fields[key]
        local value = input[key]
        if field.parse then value = field.parse(value, object) end
        if value == nil or (field.check and not field.check(value, object)) then bad(field.says) end
        out[key], any = value, true
      end
    end
    if not any then bad("give at least one of " .. table.concat(order, ", ")) end
    return out
  end
  spec.write = function(object, state)
    for _, key in ipairs(order) do
      if state[key] ~= nil then fields[key].set(object, state[key]) end
    end
  end
  -- (only the settings count: a fence carries more, what it names and what it wants)
  spec.same = function(a, b)
    for _, key in ipairs(order) do
      local value, other = a[key], b[key]
      if type(value) == "number" and type(other) == "number" then
        if not close(value, other) then return false end
      elseif type(value) == "table" and type(other) == "table" then
        if #value ~= #other then return false end
        for k = 1, #value do if value[k] ~= other[k] then return false end end
      elseif value ~= other then
        return false
      end
    end
    return true
  end
  spec.title = spec.title or function(name, _, to)
    local parts = {}
    for _, key in ipairs(order) do
      if to[key] ~= nil then parts[#parts + 1] = fields[key].text(to[key]) end
    end
    return name .. ": " .. table.concat(parts, ", ")
  end
  -- (a field's observable is its Renoise field's, or `obs` says)
  spec.observables = spec.observables or function(object)
    local out = {}
    for _, key in ipairs(order) do
      local field = fields[key]
      local ok, observable = pcall(function()
        if field.obs then return field.obs(object) end
        return field.field and object[field.field .. "_observable"] or nil
      end)
      if ok and observable then out[#out + 1] = observable end
    end
    return out
  end
  return spec
end
M.settings_kind = settings_kind

local function whole(low, high)
  return function(v) return type(v) == "number" and v == math.floor(v) and v >= low and v <= high end
end
local function is_boolean(v) return type(v) == "boolean" end
M.whole, M.is_boolean = whole, is_boolean

-- A name for one of Renoise's numbered settings, both ways.
local function named(map)
  local back = {}
  for name, value in pairs(map) do back[value] = name end
  return {
    parse = function(v) return map[v] end,
    get = function(value) return back[value] or value end,
    check = function(v) return v ~= nil end,
  }
end
M.named = named

local QUANTIZE = named({ none = 1, line = 2, beat = 3, bar = 4 })
local KEYS = { "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B" }

local function db_to_linear(db) return db <= -200 and 0 or 10 ^ (db / 20) end
local function linear_to_db(v) return base.db_of(v) or -200 end
M.db_to_linear, M.linear_to_db = db_to_linear, linear_to_db

instrument_spec(settings_kind({
  kind = "instrument-set", noun = "settings", undoable = true,
  order = { "volumeDb", "transpose", "quantize", "monophonic", "glide", "scale", "scaleKey" },
  fields = {
    volumeDb = {
      get = function(i) return linear_to_db(i.volume) end, set = function(i, v) i.volume = db_to_linear(v) end, field = "volume",
      check = function(v) return type(v) == "number" and v == v and v <= 6.0206 end, says = "volumeDb is a number up to +6 (-200 or less is silence)",
      text = function(v) return "volume " .. base.db_text(v > -200 and v or nil) .. " dB" end,
    },
    transpose = {
      get = function(i) return i.transpose end, set = function(i, v) i.transpose = v end, check = whole(-120, 120), field = "transpose",
      says = "transpose is whole semitones from -120 to 120", text = function(v) return "transpose " .. v end,
    },
    quantize = {
      get = function(i) return QUANTIZE.get(i.trigger_options.quantize) end, set = function(i, v) i.trigger_options.quantize = QUANTIZE.parse(v) end,
      obs = function(i) return i.trigger_options.quantize_observable end,
      check = function(v) return QUANTIZE.parse(v) ~= nil end, says = "quantize is none, line, beat or bar",
      text = function(v) return "trigger quantize " .. v end,
    },
    monophonic = {
      get = function(i) return i.trigger_options.monophonic end, set = function(i, v) i.trigger_options.monophonic = v end,
      obs = function(i) return i.trigger_options.monophonic_observable end,
      check = is_boolean, says = "monophonic is true or false", text = function(v) return v and "mono" or "poly" end,
    },
    glide = {
      get = function(i) return i.trigger_options.monophonic_glide end, set = function(i, v) i.trigger_options.monophonic_glide = v end,
      obs = function(i) return i.trigger_options.monophonic_glide_observable end,
      check = whole(0, 255), says = "glide is from 0 (off) to 255 (instant)", text = function(v) return "glide " .. v end,
    },
    scale = {
      get = function(i) return i.trigger_options.scale_mode end, set = function(i, v) i.trigger_options.scale_mode = v end,
      obs = function(i) return i.trigger_options.scale_mode_observable end,
      check = function(v, i)
        if type(v) ~= "string" then return false end
        for _, name in ipairs(i.trigger_options.available_scale_modes) do if name == v then return true end end
        return false
      end,
      says = "scale is one of the instrument's scales (as Renoise names them: \"None\", \"Natural Minor\", ...)",
      text = function(v) return "scale " .. v end,
    },
    scaleKey = {
      get = function(i) return KEYS[i.trigger_options.scale_key] or i.trigger_options.scale_key end,
      obs = function(i) return i.trigger_options.scale_key_observable end,
      set = function(i, v)
        for k, name in ipairs(KEYS) do if name == v then i.trigger_options.scale_key = k end end
      end,
      parse = function(v)
        if type(v) == "number" then return KEYS[v] end
        if type(v) ~= "string" then return nil end
        local upper = v:upper():gsub("%-", "")
        for _, name in ipairs(KEYS) do if name == upper then return name end end
        return nil
      end,
      check = function(v) return v ~= nil end, says = "scaleKey is a key, C to B (C#, not Db), or 1 to 12", text = function(v) return "key " .. v end,
    },
  },
}))

-- instrument-macro: one of the eight macros, or the pitch-bend, mod-wheel or channel-pressure macro; its value (0 to 1) and name.

local MIDI_MACROS = { pitchbend = "pitchbend_macro", modwheel = "modulation_wheel_macro", pressure = "channel_pressure_macro" }

local function macro_of(instrument, k)
  if MIDI_MACROS[k] then return instrument[MIDI_MACROS[k]] end
  return instrument:macro(k)
end

-- (the macro a request or a kept place names)
local function macro_number(input)
  local k = input.macro
  if k == nil and type(input.place) == "table" then k = input.place.macro end
  if not (MIDI_MACROS[k] or (type(k) == "number" and k == math.floor(k) and k >= 1 and k <= 8)) then
    bad("macro is a number from 1 to 8, or pitchbend, modwheel or pressure")
  end
  return k
end

instrument_spec({
  kind = "instrument-macro", noun = "macro", undoable = true,
  object = function(objects, input) return macro_of(objects.instrument, macro_number(input)) end,
  label = function(label, input) return string.format("%s macro %s", label, tostring(macro_number(input))) end,
  decorate = function(loc, input)
    local k = macro_number(input)
    loc.place.macro, loc.target.place.macro = k, k
  end,
  read = function(macro) return { value = macro.value, name = macro.name } end,
  observables = function(macro) return { macro.value_observable, macro.name_observable } end,
  wanted = function(macro, input)
    local value, name = macro.value, macro.name
    if input.value ~= nil then
      if type(input.value) ~= "number" or input.value ~= input.value or input.value < 0 or input.value > 1 then bad("value must be from 0 to 1") end
      value = input.value
    end
    if input.name ~= nil then name = one_line(input.name, 64, "name") end
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

-- Instruments made, deleted, loaded, copied, moved

-- The selected instrument is what Renoise's file calls act on: chosen for the call, then put back on the instrument the
-- producer had (not an undo step).
local function with_selected(song, i, fn)
  local before = song.selected_instrument_index
  song.selected_instrument_index = i
  local ok, result = pcall(fn)
  pcall(function()
    if before >= 1 and before <= #song.instruments then song.selected_instrument_index = before end
  end)
  if not ok then error(result, 0) end
  return result
end
M.with_selected = with_selected

-- The selection after an instrument was made at `i`: the producer's moved up one with the others.
local function keep_selection(song, i, before)
  local wanted = before >= i and before + 1 or before
  pcall(function()
    if wanted >= 1 and wanted <= #song.instruments and song.selected_instrument_index ~= wanted then song.selected_instrument_index = wanted end
  end)
end

-- The devices on tracks aimed at an instrument (*Instr. Macros, *Instr. Automation, *Instr. MIDI Control): their "Instrument"
-- parameter counts the instruments from 0.
local function instrument_links(song, v)
  local found = {}
  for t = 1, rows.track_count(song) do
    local track = song:track(t)
    for d = 2, #track.devices do
      local device = track:device(d)
      if device.device_path:find("/Native/*Instr.", 1, true) then
        for k = 1, #device.parameters do
          local parameter = device:parameter(k)
          if parameter.name == "Instrument" and parameter.value == v then found[#found + 1] = { t = t, d = d, k = k, path = device.device_path } end
        end
      end
    end
  end
  return found
end
M.instrument_links = instrument_links

-- Whether Kumi may take away an instrument it made (an add's, a load's, a duplicate's undo): nothing plays it, nothing is
-- aimed at it, and it is as Kumi made it. `discard` takes it whatever is in it, as a track's add does.
local function check_removable(song, i, instrument, applied, discard, label)
  local print = places_.instrument_print(instrument)
  if not discard and type(applied) == "table" and type(applied.print) == "table" and not base.same_table(print, applied.print) then
    fail("changed", string.format("%s changed in Renoise since Kumi made it (samples, phrases, a plug-in or its name)", q(label)))
  end
  if discard then return end
  local notes = places_.notes_playing(song, i - 1, M.SCAN_BUDGET)
  if notes == nil then
    fail("changed", string.format("Kumi can't tell whether notes play %s now (the song is too big to look through in one go), so it left it: delete it in Renoise, or undo with discard", q(label)))
  end
  if #notes > 0 then
    fail("changed", string.format("%d %s in the song play %s since Kumi made it: Kumi left it (Renoise would clear them)", #notes, #notes == 1 and "note" or "notes", q(label)))
  end
  if #instrument_links(song, i - 1) > 0 then
    fail("changed", string.format("a device on a track is aimed at %s since Kumi made it: Kumi left it", q(label)))
  end
end

local function index_input(input, count, what)
  local index = input.index
  if index == nil then return count + 1 end
  if type(index) ~= "number" or index ~= math.floor(index) or index < 1 or index > count + 1 then
    bad(string.format("%s is a place from 1 to %d (the instruments are numbered from 1 here; Renoise shows them from 00)", what or "index", count + 1))
  end
  return index
end

local function song_target(env) return { ref = refs.make(env.epoch(), "song", 1), kind = "song" } end

-- Undoing something Kumi made: the instrument found by its place, taken away if nothing came of it since.
local function remove_made(song, env, restore, desc, kind, noun)
  local material = restore.material
  if type(material) ~= "table" or type(material.made) ~= "table" then bad("material: the instrument the change made") end
  local label = (restore.applied or {}).name or noun
  local n, why = places_.follow(env, material.made)
  if not n then
    if why == "gone" then fail("gone", string.format("the instrument Kumi made (%s) isn't in the song any more", q(label))) end
    places_.follow_failed(why, "instrument", label)
  end
  local i = n[1]
  local instrument = song:instrument(i)
  label = places_.instrument_label(i, instrument)
  if #song.instruments == 1 then fail("changed", "it is the song's only instrument now: Renoise needs one") end
  check_removable(song, i, instrument, restore.applied, restore.discard, label)
  song:describe_undo(desc)
  song:delete_instrument_at(i)
  base.note_restored_at(env.journal, kind, places_.journal_place("instrument", { i }))
  return {
    restored = { deleted = true }, display = { title = string.format("Removed instrument %s (put back)", q(label)) },
    undo = { desc = desc, entry = true },
  }
end

-- instrument-add {index?, name?}: an empty instrument.

local function add_preview(song, env, input)
  local count = #song.instruments
  if count >= 255 then bad("a song has at most 255 instruments") end
  local index = index_input(input, count)
  if input.name ~= nil then one_line(input.name, 256, "name") end
  local title = input.name and input.name ~= "" and ("Added instrument " .. q(input.name)) or string.format("Added an instrument at %02X", index - 1)
  local display = { title = title, before = { instruments = count }, after = { instruments = count + 1, index = index } }
  if index <= count then display.note = "The instruments after it move up one; Renoise renumbers the notes that play them." end
  return { target = song_target(env), prior = { instruments = count }, fence = { instruments = count, index = index }, display = display }
end

local function add_apply(song, env, input, fence, desc)
  local count = #song.instruments
  if count ~= fence.instruments then fail("changed", "the instruments changed since the preview") end
  local index = index_input(input, count)
  local before = song.selected_instrument_index
  song:describe_undo(desc)
  local instrument = song:insert_instrument_at(index)
  if input.name and input.name ~= "" then instrument.name = input.name end
  keep_selection(song, index, before)
  local label = places_.instrument_label(index, instrument)
  local n = { index }
  return {
    applied = { index = index, name = instrument.name, print = places_.instrument_print(instrument) },
    created = { ref = places_.ref_of(env, "instrument", n), kind = "instrument" }, target = places_.target_of(env, "instrument", n),
    material = { made = places_.place_of(env, "instrument", n) },
    display = { title = "Added instrument " .. q(label), before = { instruments = count }, after = { instruments = count + 1, index = index } },
    undo = { desc = desc, entry = true }, prior = { instruments = count }, place = places_.journal_place("instrument", n), shape = "made",
  }
end

base.register({
  kind = "instrument-add", custom = true, shape = "made", by_place = true,
  preview = add_preview, apply = add_apply,
  restore = function(song, env, restore, desc) return remove_made(song, env, restore, desc, "instrument-add", "the new instrument") end,
})

-- instrument-delete {ref}: Kumi keeps the instrument as an .xrni in Renoise's temporary folder (Renoise empties it as it quits;
-- Kumi's undo can't outlive the song anyway), with the notes that play it (Renoise clears them as it deletes it: lab), and the
-- devices aimed at it. The undo makes it again at its place, loads it back, and writes the notes back where nothing else came.

local function samples_bytes(instrument)
  local bytes = 0
  for s = 1, #instrument.samples do
    local buffer = instrument:sample(s).sample_buffer
    if buffer.has_sample_data and not buffer.read_only then
      bytes = bytes + buffer.number_of_frames * buffer.number_of_channels * math.max(1, math.floor(buffer.bit_depth / 8))
    end
  end
  local plugin = instrument.plugin_properties
  if plugin.plugin_loaded then bytes = bytes + #(plugin.plugin_device.active_preset_data or "") end
  return bytes
end
M.samples_bytes = samples_bytes

local function delete_fence(i, instrument)
  local fence = places_.instrument_print(instrument)
  fence.index = i
  return fence
end

-- What a delete's print was (its fence less the place).
local function print_of_fence(fence)
  local print = {}
  for k, value in pairs(fence) do if k ~= "index" then print[k] = value end end
  return print
end

local function delete_preview(song, env, input)
  local n, objects, label = places_.locate(song, env, "instrument", input, "instrument-delete")
  if #song.instruments == 1 then bad("it is the song's only instrument: Renoise needs one") end
  local bytes = samples_bytes(objects.instrument)
  local display = {
    title = "Deleted instrument " .. q(label), before = { instruments = #song.instruments }, after = { instruments = #song.instruments - 1 },
    note = "Kumi keeps it (saved as an instrument file) and the notes that play it, to put back. The instruments after it move down one; Renoise renumbers their notes.",
  }
  if bytes > M.MAX_INSTRUMENT_BYTES then
    display.note = string.format("It holds %.0f MB, more than Kumi can keep to put it back: the apply will refuse.", bytes / 1048576)
  end
  return { target = places_.target_of(env, "instrument", n), prior = { deleted = false }, fence = delete_fence(n[1], objects.instrument), display = display }
end

-- The notes playing instrument value `v` found by a look through the song: inline when it fits a callback, else in slices of a
-- job (each pattern's count of changed lines is kept, so the writing step looks again at the patterns changed since).
local function scan_cost(song)
  local cost = 0
  for p = 1, #song.patterns do cost = cost + song:pattern(p).number_of_lines end
  return cost * rows.track_count(song)
end

local function notes_in_pattern(song, p, v, found)
  local pattern = song:pattern(p)
  for t = 1, rows.track_count(song) do
    local pattern_track = pattern:track(t)
    if not pattern_track.is_alias and not pattern_track.is_empty and song:track(t).max_note_columns > 0 then
      for l = 1, pattern.number_of_lines do
        local line = pattern_track:line(l)
        if not line.is_empty then
          for c = 1, 12 do
            local column = line:note_column(c)
            if column.instrument_value == v then
              found[#found + 1] = {
                p = p, t = t, line = l, col = c, note = column.note_value, volume = column.volume_value, panning = column.panning_value,
                delay = column.delay_value, fxn = column.effect_number_value, fxa = column.effect_amount_value,
              }
            end
          end
        end
      end
    end
  end
end
M.notes_in_pattern = notes_in_pattern

local function scan(job, env, v)
  local song = rows.the_song()
  local found, marks = {}, {}
  for p = 1, #song.patterns do
    if job then job.slice() end
    local live = rows.the_song()
    if p > #live.patterns then break end
    marks[p] = env.pattern_touch_mark and env.pattern_touch_mark(p) or false
    notes_in_pattern(live, p, v, found)
  end
  return found, marks
end
M.scan = scan

-- The patterns changed since their look: looked at again (found is kept for the others).
local function rescan_changed(env, v, found, marks)
  local song = rows.the_song()
  local again = {}
  for p, mark in pairs(marks) do
    local changed = true
    if mark and env.pattern_touched_since then
      local since = env.pattern_touched_since(p, mark)
      changed = since ~= false
    end
    if changed then again[p] = true end
  end
  if not next(again) then return found end
  local kept = {}
  for _, note in ipairs(found) do
    if not again[note.p] then kept[#kept + 1] = note end
  end
  for p in pairs(again) do
    if p <= #song.patterns then notes_in_pattern(song, p, v, kept) end
  end
  return kept
end

local function delete_write(song, env, input, fence, desc, notes)
  local ok, n, objects, label = pcall(places_.locate, song, env, "instrument", input, "instrument-delete")
  if not ok then base.gone_is_changed(n) end
  local i, instrument = n[1], objects.instrument
  if not base.same_table(delete_fence(i, instrument), fence) then fail("changed", q(label) .. " changed since the preview") end
  if #song.instruments == 1 then bad("it is the song's only instrument: Renoise needs one") end
  local bytes = samples_bytes(instrument)
  if bytes > M.MAX_INSTRUMENT_BYTES then
    fail("failed", string.format("%s holds %.0f MB, more than Kumi can keep to put it back, so it won't delete it", q(label), bytes / 1048576))
  end
  -- (saved first: if Renoise can't save it, nothing is deleted)
  local file = os.tmpname(".xrni")
  local saved = with_selected(song, i, function() return renoise.app():save_instrument(file) end)
  if not saved or not io.exists(file) then
    fail("failed", "Renoise wouldn't save " .. q(label) .. " to keep it, so Kumi didn't delete it")
  end
  local stat = io.stat(file)
  local links = instrument_links(song, i - 1)
  local patterns = {}
  for _, note in ipairs(notes) do patterns[note.p] = true end
  local material_ = import("changes_material")
  local pool = material_.keep_pool(song, env, patterns)
  local name = instrument.name
  song:describe_undo(desc)
  song:delete_instrument_at(i)
  -- (the place it goes back to: after the instrument before it, followed from here)
  local material = {
    file = file, bytes = stat and stat.size or nil, print = print_of_fence(fence), name = name, index = i,
    after = env.instrument_mark and env.instrument_mark() or nil, notes = notes, pool = pool, links = links,
  }
  if #json.encode(material) > base.MAX_MATERIAL_BYTES then
    -- (too many notes to keep: the instrument goes back, and nothing is lost)
    fail("failed", string.format("%s plays too many notes for Kumi to keep them all: it won't delete it", q(label)))
  end
  local note = #notes > 0 and string.format("Renoise cleared the %d %s that played it; Kumi puts %s back with it.", #notes, #notes == 1 and "note" or "notes", #notes == 1 and "it" or "them") or nil
  return {
    applied = { deleted = true }, material = material, target = song_target(env),
    display = { title = "Deleted instrument " .. q(label), note = note }, undo = { desc = desc, entry = true },
    prior = { deleted = false }, place = places_.journal_place("instrument", { i }), shape = "deleted",
  }
end

local function delete_apply(song, env, input, fence, desc)
  local v = fence.index - 1
  if scan_cost(song) <= M.SCAN_BUDGET then
    local found = scan(nil, env, v)
    return delete_write(song, env, input, fence, desc, found)
  end
  -- A big song: looked through in slices (nothing written), then the delete in one step of its own.
  return {
    kumi_job = function(job)
      base.wait_while_busy(job, env)
      local found, marks = scan(job, env, v)
      job.step(0)
      base.wait_while_busy(job, env)
      found = rescan_changed(env, v, found, marks)
      return delete_write(rows.the_song(), env, input, fence, desc, found)
    end,
  }
end


local function write_note(column, note, v)
  column.note_value, column.instrument_value, column.volume_value = note.note, v, note.volume
  column.panning_value, column.delay_value = note.panning, note.delay
  column.effect_number_value, column.effect_amount_value = note.fxn, note.fxa
end

local function delete_restore(song, env, restore, desc)
  local material = restore.material
  if type(material) ~= "table" or type(material.file) ~= "string" or type(material.index) ~= "number" then bad("material: what the delete kept") end
  local label = material.name ~= "" and material.name or string.format("Instrument %02X", material.index - 1)
  if not io.exists(material.file) then
    fail("failed", string.format("the copy of %s Kumi kept is gone (Renoise empties its temporary folder as it quits)", q(label)))
  end
  local at = env.instrument_after and env.instrument_after(material.index, material.after) or nil
  if not at then
    fail("stale", string.format("the instruments changed in a way Kumi couldn't follow since it deleted %s, so it can't tell where it goes: nothing was put back", q(label)))
  end
  at = math.max(1, math.min(at or 1, #song.instruments + 1))
  -- (back already: the producer's undo brought it, with its notes)
  if at <= #song.instruments and base.same_table(places_.instrument_print(song:instrument(at)), material.print) then
    base.note_restored_at(env.journal, "instrument-delete", places_.journal_place("instrument", { at }))
    return { restored = { ref = places_.ref_of(env, "instrument", { at }) }, display = { title = q(label) .. " is back already" }, undo = { desc = desc, entry = false }, unwritten = true }
  end
  if #song.instruments >= 255 then fail("changed", "the song has 255 instruments now: there's no room to put it back") end
  local material_ = import("changes_material")
  local pattern_now = material_.pool_now(song, env, material.pool)
  if not pattern_now and #(material.notes or {}) > 0 then
    fail("stale", string.format("the song's patterns changed in a way Kumi couldn't follow since it deleted %s, so it can't tell where its notes go: nothing was put back", q(label)))
  end
  local before = song.selected_instrument_index
  song:describe_undo(desc)
  song:insert_instrument_at(at)
  local loaded = with_selected(song, at, function() return renoise.app():load_instrument(material.file) end)
  keep_selection(song, at, before)
  if not loaded then
    pcall(function() song:delete_instrument_at(at) end)
    fail("failed", string.format("Renoise wouldn't load the copy of %s back: nothing was put back", q(label)))
  end
  -- The notes, where Renoise cleared them and nothing came since (a cell the producer filled is theirs).
  local moved = function(t)
    if type(restore.layout) ~= "number" or not env.shifted then return t end
    return (env.shifted(t, restore.layout))
  end
  local skipped = 0
  for _, note in ipairs(material.notes or {}) do
    local p = pattern_now and pattern_now(note.p) or nil
    local t = moved(note.t)
    local column
    if p and t and t <= rows.track_count(song) and note.line <= song:pattern(p).number_of_lines then
      local pattern_track = song:pattern(p):track(t)
      if not pattern_track.is_alias then column = pattern_track:line(note.line):note_column(note.col) end
    end
    if column and column.is_empty then write_note(column, note, at - 1) else skipped = skipped + 1 end
  end
  -- The devices aimed at it, where nothing re-aimed them since.
  local relinked, unlinked = 0, 0
  for _, link in ipairs(material.links or {}) do
    local t = moved(link.t)
    local done = false
    if t and t <= rows.track_count(song) then
      local track = song:track(t)
      local device = link.d <= #track.devices and track:device(link.d) or nil
      if device and device.device_path == link.path then
        local parameter = device:parameter(link.k)
        if parameter and parameter.name == "Instrument" and (parameter.value < 0 or parameter.value == at - 1 or parameter.value >= #song.instruments) then
          done = pcall(function() parameter.value = at - 1 end)
        end
      end
    end
    if done then relinked = relinked + 1 else unlinked = unlinked + 1 end
  end
  base.note_restored_at(env.journal, "instrument-delete", places_.journal_place("instrument", { at }))
  local parts = {}
  if #(material.notes or {}) > 0 then
    parts[#parts + 1] = skipped == 0 and string.format("Its %d %s %s back.", #material.notes, #material.notes == 1 and "note is" or "notes are", "put")
      or string.format("%d of its %d notes are back; %d weren't put back (their pattern, track or cell changed in Renoise since).", #material.notes - skipped, #material.notes, skipped)
  end
  if unlinked > 0 then parts[#parts + 1] = string.format("%d %s aimed at it couldn't be aimed again: look at them.", unlinked, unlinked == 1 and "device" or "devices") end
  return {
    restored = { ref = places_.ref_of(env, "instrument", { at }) },
    display = { title = q(label) .. " put back", note = #parts > 0 and table.concat(parts, " ") or nil },
    undo = { desc = desc, entry = true },
  }
end

base.register({
  kind = "instrument-delete", custom = true, shape = "deleted", by_place = true,
  preview = delete_preview, apply = delete_apply, restore = delete_restore,
})

-- instrument-load {path, index? | ref?}: an instrument file (.xrni), a multi-sample (.sfz) or a sample file into a new slot
-- (at index, the end by default) or into an empty instrument (ref). A path in Renoise's own library may be written
-- "library:Instruments/Drums/...".

local SAMPLE_FILES = { wav = true, flac = true, aif = true, aiff = true, ogg = true, mp3 = true, caf = true, mp4 = true, m4a = true }
M.SAMPLE_FILES = SAMPLE_FILES

-- Renoise's own library: where its scripts' libraries are, two folders up (package.path names them), or where Renoise installs.
local function library_root()
  local candidates = {}
  for entry in tostring(package.path):gmatch("[^;]+") do
    local root = entry:match("^(.-)[/\\]Scripts[/\\]Libraries[/\\]")
    if root then candidates[#candidates + 1] = root .. "/Library" end
  end
  candidates[#candidates + 1] = "/Applications/Renoise.app/Contents/Resources/Library"
  for _, path in ipairs(candidates) do
    if io.exists(path) then return path end
  end
  return nil
end
M.library_root = library_root

local function absolute(path) return path:match("^/") or path:match("^%a:[/\\]") or path:match("^\\\\") end

-- A path Kumi may load: absolute (or in the library), there, of a kind Renoise loads, and small enough for one callback.
local function load_path(input, kinds)
  local path = input.path
  if type(path) ~= "string" or path == "" or path:find("[%c]") then bad("path: an instrument file's absolute path") end
  local inside = path:match("^library:(.*)$")
  if inside then
    local root = library_root()
    if not root then bad("Kumi can't find Renoise's library folder here: give the file's absolute path") end
    if inside:find("%.%.") then bad("a library path stays inside the library") end
    path = root .. "/" .. inside:gsub("^[/\\]+", "")
  end
  if not absolute(path) then bad("path must be absolute (or library:Instruments/...)") end
  if not io.exists(path) then bad("there's no file at " .. path) end
  local stat = io.stat(path)
  if stat and stat.type and stat.type ~= "file" then bad(path .. " isn't a file") end
  local extension = (path:match("%.([%w]+)$") or ""):lower()
  if not kinds[extension] then bad("Renoise loads instruments (.xrni), multi-samples (.sfz) and sample files (.wav, .flac, .aif, .ogg, .mp3) here; not ." .. extension) end
  if stat and stat.size and stat.size > M.MAX_LOAD_BYTES then
    fail("failed", string.format("%s is %.0f MB, more than Kumi loads in one go (%d MB): load it in Renoise", path, stat.size / 1048576, M.MAX_LOAD_BYTES / 1048576))
  end
  return path, extension
end
M.load_path = load_path

local LOADS = { xrni = true, sfz = true }
for extension in pairs(SAMPLE_FILES) do LOADS[extension] = true end

local function file_title(path) return path:match("([^/\\]+)%.[^.]+$") or path end

-- Loads the file into the instrument at `i` (selected for the call): Renoise's load for its kind. True when it took.
local function load_into(song, i, path, extension)
  return with_selected(song, i, function()
    if extension == "xrni" then return renoise.app():load_instrument(path) end
    if extension == "sfz" then return renoise.app():load_instrument_multi_sample(path) end
    -- (a sample file: one sample, named by the file, in the empty instrument)
    local instrument = song:instrument(i)
    local sample = instrument:insert_sample_at(1)
    local ok = sample.sample_buffer:load_from(path)
    if ok and instrument.name == "" then instrument.name = file_title(path) end
    return ok
  end)
end
M.load_into = load_into

local function load_plan(song, env, input)
  local path, extension = load_path(input, LOADS)
  if input.ref ~= nil and input.index ~= nil then bad("give index (a new instrument) or ref (an empty one), not both") end
  local plan = { path = path, extension = extension, count = #song.instruments }
  if input.ref ~= nil then
    local n, objects, label = places_.locate(song, env, "instrument", input, "instrument-load")
    if not places_.is_empty(objects.instrument) then
      bad(string.format("%s isn't empty: Kumi loads into a new instrument or an empty one (give index, or leave it out)", q(label)))
    end
    plan.into, plan.label = n[1], label
  else
    if plan.count >= 255 then bad("a song has at most 255 instruments") end
    plan.index = index_input(input, plan.count)
  end
  return plan
end

local function load_preview(song, env, input)
  local plan = load_plan(song, env, input)
  local title = plan.into and string.format("Loaded %s into %s", q(file_title(plan.path)), q(plan.label))
    or string.format("Loaded %s as instrument %02X", q(file_title(plan.path)), plan.index - 1)
  local fence = { instruments = plan.count, index = plan.index, into = plan.into }
  if plan.into then fence.print = places_.instrument_print(song:instrument(plan.into)) end
  local display = { title = title, before = { instruments = plan.count }, after = { instruments = plan.count + (plan.into and 0 or 1) } }
  if plan.index and plan.index <= plan.count then display.note = "The instruments after it move up one; Renoise renumbers the notes that play them." end
  local target = plan.into and places_.target_of(env, "instrument", { plan.into }) or song_target(env)
  return { target = target, prior = { instruments = plan.count }, fence = fence, display = display }
end

local function load_apply(song, env, input, fence, desc)
  local ok, plan = pcall(load_plan, song, env, input)
  if not ok then base.gone_is_changed(plan) end
  if plan.count ~= fence.instruments or plan.into ~= fence.into or plan.index ~= fence.index then fail("changed", "the instruments changed since the preview") end
  if plan.into and not base.same_table(places_.instrument_print(song:instrument(plan.into)), fence.print) then fail("changed", q(plan.label) .. " changed since the preview") end
  local before = song.selected_instrument_index
  local i = plan.into or plan.index
  song:describe_undo(desc)
  if not plan.into then song:insert_instrument_at(i) end
  local loaded_ok, loaded = pcall(load_into, song, i, plan.path, plan.extension)
  if not plan.into then keep_selection(song, i, before) end
  local instrument = song:instrument(i)
  if not loaded_ok or not loaded or places_.is_empty(instrument) then
    -- (nothing half-done stays: the new instrument goes, an empty one is emptied)
    pcall(function()
      if plan.into then instrument:clear() else song:delete_instrument_at(i) end
    end)
    local why = loaded_ok and "" or (": " .. tostring(type(loaded) == "table" and loaded.message or loaded):gsub("^[^%s:]*%.lua:%d+: ", ""))
    fail("failed", string.format("Renoise wouldn't load %s%s", plan.path, why))
  end
  local n = { i }
  local label = places_.instrument_label(i, instrument)
  local done = {
    applied = { index = i, name = instrument.name, print = places_.instrument_print(instrument), path = plan.path },
    target = places_.target_of(env, "instrument", n),
    display = { title = string.format("Loaded %s as %s", q(file_title(plan.path)), q(label)), before = { instruments = plan.count }, after = { instruments = #song.instruments } },
    undo = { desc = desc, entry = true }, prior = { instruments = plan.count }, place = places_.journal_place("instrument", n),
  }
  if plan.into then
    done.material = { into = places_.place_of(env, "instrument", n) }
  else
    done.material = { made = places_.place_of(env, "instrument", n) }
    done.created = { ref = places_.ref_of(env, "instrument", n), kind = "instrument" }
    done.shape = "made"
  end
  return done
end

local function load_restore(song, env, restore, desc)
  local material = restore.material
  if type(material) ~= "table" then bad("material: where the change loaded") end
  if material.made then return remove_made(song, env, restore, desc, "instrument-load", "the loaded instrument") end
  if type(material.into) ~= "table" then bad("material: where the change loaded") end
  local n, why = places_.follow(env, material.into)
  if not n then places_.follow_failed(why, "instrument", (restore.applied or {}).name) end
  local instrument = song:instrument(n[1])
  local label = places_.instrument_label(n[1], instrument)
  if places_.is_empty(instrument) then
    return { restored = { emptied = true }, display = { title = q(label) .. " is empty already" }, undo = { desc = desc, entry = false }, unwritten = true }
  end
  if not restore.discard and not base.same_table(places_.instrument_print(instrument), (restore.applied or {}).print) then
    fail("changed", string.format("%s changed in Renoise since Kumi loaded it", q(label)))
  end
  song:describe_undo(desc)
  instrument:clear()
  base.note_restored_at(env.journal, "instrument-load", places_.journal_place("instrument", n))
  return { restored = { emptied = true }, display = { title = string.format("Emptied %s again (put back)", q(label)) }, undo = { desc = desc, entry = true } }
end

base.register({
  kind = "instrument-load", custom = true, by_place = true,
  preview = load_preview, apply = load_apply, restore = load_restore,
})

-- instrument-duplicate {ref, index?}: a copy of the instrument (samples and all), right after it unless index says.

local function duplicate_plan(song, env, input)
  local n, objects, label = places_.locate(song, env, "instrument", input, "instrument-duplicate")
  local count = #song.instruments
  if count >= 255 then bad("a song has at most 255 instruments") end
  local index = input.index == nil and n[1] + 1 or index_input(input, count)
  return { from = n[1], index = index, count = count, label = label, print = places_.instrument_print(objects.instrument), bytes = samples_bytes(objects.instrument) }
end

local function duplicate_preview(song, env, input)
  local plan = duplicate_plan(song, env, input)
  local display = { title = string.format("Duplicated %s as instrument %02X", q(plan.label), plan.index - 1), before = { instruments = plan.count }, after = { instruments = plan.count + 1 } }
  return {
    target = places_.target_of(env, "instrument", { plan.from }), prior = { instruments = plan.count },
    fence = { instruments = plan.count, from = plan.from, index = plan.index, print = plan.print }, display = display,
  }
end

local function duplicate_apply(song, env, input, fence, desc)
  local ok, plan = pcall(duplicate_plan, song, env, input)
  if not ok then base.gone_is_changed(plan) end
  if plan.count ~= fence.instruments or plan.from ~= fence.from or plan.index ~= fence.index or not base.same_table(plan.print, fence.print) then
    fail("changed", q(plan.label) .. " or the instruments changed since the preview")
  end
  if plan.bytes > M.MAX_INSTRUMENT_BYTES then
    fail("failed", string.format("%s holds %.0f MB, more than Kumi copies in one go", q(plan.label), plan.bytes / 1048576))
  end
  local before = song.selected_instrument_index
  song:describe_undo(desc)
  local made = song:insert_instrument_at(plan.index)
  local source = song:instrument(plan.index <= plan.from and plan.from + 1 or plan.from)
  made:copy_from(source)
  keep_selection(song, plan.index, before)
  local n = { plan.index }
  return {
    applied = { index = plan.index, name = made.name, print = places_.instrument_print(made) },
    created = { ref = places_.ref_of(env, "instrument", n), kind = "instrument" }, target = places_.target_of(env, "instrument", n),
    material = { made = places_.place_of(env, "instrument", n) },
    display = { title = string.format("Duplicated %s as instrument %02X", q(plan.label), plan.index - 1), before = { instruments = plan.count }, after = { instruments = #song.instruments } },
    undo = { desc = desc, entry = true }, prior = { instruments = plan.count }, place = places_.journal_place("instrument", n), shape = "made",
  }
end

base.register({
  kind = "instrument-duplicate", custom = true, shape = "made", by_place = true,
  preview = duplicate_preview, apply = duplicate_apply,
  restore = function(song, env, restore, desc) return remove_made(song, env, restore, desc, "instrument-duplicate", "the copy") end,
})

-- instrument-move {ref, index}: the instrument ends at `index` (from 1), the others keep their order (swaps of neighbours;
-- Renoise renumbers the notes with each).

local function move_plan(song, env, input)
  local n, _, label = places_.locate(song, env, "instrument", input, "instrument-move")
  local count = #song.instruments
  local to = input.index
  if type(to) ~= "number" or to ~= math.floor(to) or to < 1 or to > count then bad(string.format("index is a place from 1 to %d", count)) end
  if math.abs(to - n[1]) > M.MAX_MOVE_SWAPS then
    bad(string.format("that moves it past %d instruments, more than Kumi moves in one step (each place renumbers the whole song's notes): move it in steps", math.abs(to - n[1])))
  end
  return { from = n[1], to = to, count = count, label = label }
end

local function swap_along(song, from, to)
  local step = to > from and 1 or -1
  for k = from, to - step, step do song:swap_instruments_at(k, k + step) end
end

local function move_preview(song, env, input)
  local plan = move_plan(song, env, input)
  local display = { title = string.format("Moved instrument %s to %02X", q(plan.label), plan.to - 1), before = { index = plan.from }, after = { index = plan.to } }
  if plan.to == plan.from then display.unchanged = true end
  return { target = places_.target_of(env, "instrument", { plan.from }), prior = { index = plan.from }, fence = { index = plan.from, count = plan.count }, display = display }
end

local function move_apply(song, env, input, fence, desc)
  local ok, plan = pcall(move_plan, song, env, input)
  if not ok then base.gone_is_changed(plan) end
  if plan.from ~= fence.index or plan.count ~= fence.count then fail("changed", "the instruments changed since the preview") end
  local display = { title = string.format("Moved instrument %s to %02X", q(plan.label), plan.to - 1), before = { index = plan.from }, after = { index = plan.to } }
  local n = { plan.to }
  if plan.to == plan.from then
    display.unchanged = true
    return { applied = { index = plan.to, count = plan.count }, display = display, undo = { desc = desc, entry = false }, prior = { index = plan.from }, target = places_.target_of(env, "instrument", n) }
  end
  local before = song.selected_instrument_index
  song:describe_undo(desc)
  swap_along(song, plan.from, plan.to)
  pcall(function() if song.selected_instrument_index ~= before then song.selected_instrument_index = before end end)
  return {
    applied = { index = plan.to, count = plan.count }, material = { at = places_.place_of(env, "instrument", n) },
    target = places_.target_of(env, "instrument", n), display = display, undo = { desc = desc, entry = true },
    prior = { index = plan.from }, place = places_.journal_place("instrument", n), follows = "instrument",
  }
end

local function move_restore(song, env, restore, desc)
  local material, prior, applied = restore.material, restore.prior, restore.applied
  if type(material) ~= "table" or type(material.at) ~= "table" or type(prior) ~= "table" or type(applied) ~= "table" then bad("material, prior and applied: where the instrument moved") end
  local n, why = places_.follow(env, material.at)
  if not n then places_.follow_failed(why, "instrument", nil) end
  local i = n[1]
  local label = places_.instrument_label(i, song:instrument(i))
  if i == prior.index and #song.instruments == applied.count then
    return { restored = { index = i }, display = { title = q(label) .. " is back at its place already" }, undo = { desc = desc, entry = false }, unwritten = true }
  end
  -- (the place it came from is a place among the others: it means the same only while nothing else moved them)
  if i ~= applied.index or #song.instruments ~= applied.count then
    fail("changed", string.format("the instruments around %s changed in Renoise since Kumi moved it, so its old place isn't the same place: move it yourself", q(label)))
  end
  local before = song.selected_instrument_index
  song:describe_undo(desc)
  swap_along(song, i, prior.index)
  pcall(function() if song.selected_instrument_index ~= before then song.selected_instrument_index = before end end)
  base.note_restored(env.journal, "instrument-move", places_.journal_place("instrument", { prior.index }), applied, base.KINDS["instrument-move"])
  return { restored = { index = prior.index }, display = { title = string.format("Moved %s back to %02X", q(label), prior.index - 1) }, undo = { desc = desc, entry = true } }
end

base.register({
  kind = "instrument-move", custom = true, by_place = true,
  preview = move_preview, apply = move_apply, restore = move_restore,
  same = function(a, b) return a.index == b.index end,
})

-- instrument_save {ref, path, overwrite?, epoch}: the instrument written to an .xrni file (an action: nothing in the song
-- changes, so no undo). A file already there is replaced only when the request says overwrite.
function M.save(params, env)
  local song = base.context(params, env)
  local input = type(params) == "table" and params or {}
  local n, objects, label = places_.locate(song, env, "instrument", input, "instrument_save")
  local path = input.path
  if type(path) ~= "string" or path == "" or path:find("[%c]") or not absolute(path) then bad("path: the absolute path of the .xrni file to write") end
  if not path:lower():match("%.xrni$") then path = path .. ".xrni" end
  local folder = path:match("^(.*)[/\\][^/\\]+$")
  if not folder or not io.exists(folder) then bad("there's no folder " .. tostring(folder) .. " to save into") end
  if io.exists(path) and input.overwrite ~= true then
    bad(string.format("there's a file at %s already: Kumi replaces it only when you say so (overwrite: true)", path))
  end
  local bytes = samples_bytes(objects.instrument)
  if bytes > M.MAX_INSTRUMENT_BYTES then
    fail("failed", string.format("%s holds %.0f MB, more than Kumi saves in one go: save it from Renoise", q(label), bytes / 1048576))
  end
  local saved = with_selected(song, n[1], function() return renoise.app():save_instrument(path) end)
  if not saved or not io.exists(path) then fail("failed", "Renoise wouldn't save " .. q(label) .. " to " .. path) end
  local stat = io.stat(path)
  return { epoch = env.epoch(), layout = env.layout(), saved = path, bytes = stat and stat.size or json.null, name = label }
end

return M
