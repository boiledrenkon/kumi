-- Kumi's changes to an instrument's phrases (M4): phrases made and deleted, their settings and key mappings, the instrument's
-- phrase playback (off, program, keymap), a phrase's lines written and cleared (in Renoise's own cell text, as a pattern's: see
-- changes_notes.lua), and phrase scripts: written and committed (with the compile and runtime errors Renoise reports), and
-- rendered to lines. Records keep the phrase by its place (instruments.lua); a phrase's lines are undone only while nothing in
-- that phrase changed since Kumi's step (the plain rule of a pattern's lines, counted per phrase).

local import = ...
local json = import("json")
local base = import("changes_base")
local places_ = import("instruments")
local instruments_ = import("changes_instruments")
local notes_ = import("changes_notes")

local bad, fail = base.bad, base.fail
local q, close = base.q, base.close
local whole, is_boolean, named = instruments_.whole, instruments_.is_boolean, instruments_.named
local cells = notes_.cells
local M = {}

-- The most writes a phrase change makes, and a phrase delete may need to put back, in one callback.
M.MAX_WRITES = base.SLICE_WRITES
-- How long a script's apply waits for Renoise to compile it (it compiles in its player, after the commit), and a render for
-- its result.
M.COMPILE_WAIT_MS = 600
M.RENDER_WAIT_MS = 5000
M.MAX_SCRIPT_BYTES = 64 * 1024

local KEY_TRACKING = named({ none = 1, transpose = 2, offset = 3 })
local PLAYBACK = named({ pattern = 1, script = 2 })

local function phrase_label_of(n, objects) return places_.label_of("phrase", n, objects) end

-- Settings

local function bool_field(field, says, on, off)
  return { get = function(p) return p[field] end, set = function(p, v) p[field] = v end, field = field, check = is_boolean, says = says,
    text = function(v) return v and on or off end }
end

local PHRASE_FIELDS = {
  name = { get = function(p) return p.name end, set = function(p, v) p.name = v end, field = "name",
    check = function(v) return type(v) == "string" and #v <= 64 and not v:find("[%c]") end, says = "name must be one line of text", text = function(v) return "named " .. q(v) end },
  lines = { get = function(p) return p.number_of_lines end, set = function(p, v) p.number_of_lines = v end, field = "number_of_lines",
    check = whole(1, 512), says = "lines is from 1 to 512", text = function(v) return v .. " lines" end },
  lpb = { get = function(p) return p.lpb end, set = function(p, v) p.lpb = v end, field = "lpb", check = whole(1, 256), says = "lpb is from 1 to 256",
    text = function(v) return "LPB " .. v end },
  looping = bool_field("looping", "looping is true or false", "looping", "one-shot"),
  -- (the loop's lines are checked with the length the change gives: see phrase-set's wanted)
  loopStart = { get = function(p) return p.loop_start end, set = function(p, v) p.loop_start = v end, field = "loop_start",
    check = whole(1, 512), says = "loopStart is a line of the phrase", text = function(v) return "loop from line " .. v end },
  loopEnd = { get = function(p) return p.loop_end end, set = function(p, v) p.loop_end = v end, field = "loop_end",
    check = whole(1, 512), says = "loopEnd is a line of the phrase", text = function(v) return "loop to line " .. v end },
  shuffle = { get = function(p) return p.shuffle end, set = function(p, v) p.shuffle = v end, field = "shuffle",
    check = function(v) return type(v) == "number" and v >= 0 and v <= 1 end, says = "shuffle is from 0 to 1", text = function(v) return "shuffle " .. math.floor(v * 100 + 0.5) .. "%" end },
  keyTracking = { get = function(p) return KEY_TRACKING.get(p.key_tracking) end, set = function(p, v) p.key_tracking = KEY_TRACKING.parse(v) end, field = "key_tracking",
    check = function(v) return KEY_TRACKING.parse(v) ~= nil end, says = "keyTracking is none, transpose or offset", text = function(v) return "key tracking " .. v end },
  baseNote = { get = function(p) return p.base_note end, set = function(p, v) p.base_note = v end, field = "base_note", check = whole(0, 119),
    says = "baseNote is a note from 0 to 119 (C-4 is 48)", text = function(v) return "base note " .. v end },
  autoseek = bool_field("autoseek", "autoseek is true or false", "autoseek on", "autoseek off"),
  noteColumns = { get = function(p) return p.visible_note_columns end, set = function(p, v) p.visible_note_columns = v end, field = "visible_note_columns",
    check = whole(1, 12), says = "noteColumns is from 1 to 12", text = function(v) return v .. " note columns" end },
  effectColumns = { get = function(p) return p.visible_effect_columns end, set = function(p, v) p.visible_effect_columns = v end, field = "visible_effect_columns",
    check = whole(0, 8), says = "effectColumns is from 0 to 8", text = function(v) return v .. " effect columns" end },
  instrumentColumn = bool_field("instrument_column_visible", "instrumentColumn is true or false", "sample column shown", "sample column hidden"),
  volumeColumn = bool_field("volume_column_visible", "volumeColumn is true or false", "volume column shown", "volume column hidden"),
  panningColumn = bool_field("panning_column_visible", "panningColumn is true or false", "panning column shown", "panning column hidden"),
  delayColumn = bool_field("delay_column_visible", "delayColumn is true or false", "delay column shown", "delay column hidden"),
  sampleEffectsColumn = bool_field("sample_effects_column_visible", "sampleEffectsColumn is true or false", "sample effects shown", "sample effects hidden"),
  playback = { get = function(p) return PLAYBACK.get(p.playback_mode) end, set = function(p, v) p.playback_mode = PLAYBACK.parse(v) end, field = "playback_mode",
    check = function(v) return PLAYBACK.parse(v) ~= nil end, says = "playback is pattern or script", text = function(v) return "plays its " .. v end },
}
local PHRASE_ORDER = { "name", "lines", "lpb", "looping", "loopStart", "loopEnd", "shuffle", "keyTracking", "baseNote", "autoseek", "noteColumns",
  "effectColumns", "instrumentColumn", "volumeColumn", "panningColumn", "delayColumn", "sampleEffectsColumn", "playback" }

-- Writes settings: the length first, then the loop's ends (within it) in the order that keeps its start before its end, then
-- the rest. A setting Renoise refuses doesn't keep the others from being written (a restore puts back what it can).
local function write_settings(phrase, state)
  local refused
  local function put(fn)
    local ok, err = pcall(fn)
    if not ok then refused = refused or err end
  end
  if state.lines ~= nil then put(function() phrase.number_of_lines = state.lines end) end
  local lines = phrase.number_of_lines
  local start = state.loopStart and math.min(state.loopStart, lines)
  local finish = state.loopEnd and math.min(state.loopEnd, lines)
  put(function()
    if start ~= nil and finish ~= nil and start > phrase.loop_end then
      phrase.loop_end = finish
      phrase.loop_start = start
    else
      if start ~= nil then phrase.loop_start = start end
      if finish ~= nil then phrase.loop_end = finish end
    end
  end)
  for _, key in ipairs(PHRASE_ORDER) do
    if state[key] ~= nil and key ~= "lines" and key ~= "loopStart" and key ~= "loopEnd" then put(function() PHRASE_FIELDS[key].set(phrase, state[key]) end) end
  end
  if refused then error(refused, 0) end
end

local phrase_set = instruments_.settings_kind({
  kind = "phrase-set", of = "phrase", noun = "settings", undoable = true, order = PHRASE_ORDER, fields = PHRASE_FIELDS,
})
phrase_set.write = write_settings
do
  local inner = phrase_set.wanted
  phrase_set.wanted = function(phrase, input)
    local state = inner(phrase, input)
    local lines = state.lines or phrase.number_of_lines
    local start, finish = state.loopStart or math.min(phrase.loop_start, lines), state.loopEnd or math.min(phrase.loop_end, lines)
    if start > lines or finish > lines then bad("the loop must be within the phrase's lines") end
    if start > finish then bad("loopStart must not be after loopEnd") end
    return state
  end
end
instruments_.instrument_spec(phrase_set)

local function settings_of(phrase)
  local out = {}
  for _, key in ipairs(PHRASE_ORDER) do out[key] = PHRASE_FIELDS[key].get(phrase) end
  return out
end

-- A phrase's lines, as exact values (every column, hidden ones too), and how many writes put them back.
local function read_lines(phrase)
  local out, writes = {}, 0
  for l = 1, phrase.number_of_lines do
    local line = phrase:line(l)
    if not line.is_empty then
      local row = { line = l, notes = {}, effects = {} }
      for c = 1, 12 do
        local column = line:note_column(c)
        if not column.is_empty then
          row.notes[#row.notes + 1] = {
            col = c, note = column.note_value, instrument = column.instrument_value, volume = column.volume_value, panning = column.panning_value,
            delay = column.delay_value, fxn = column.effect_number_value, fxa = column.effect_amount_value,
          }
          writes = writes + 7
        end
      end
      for c = 1, 8 do
        local column = line:effect_column(c)
        if not column.is_empty then
          row.effects[#row.effects + 1] = { col = c, number = column.number_value, amount = column.amount_value }
          writes = writes + 2
        end
      end
      out[#out + 1] = row
    end
  end
  return out, writes
end
M.read_lines = read_lines

local function write_lines(phrase, rows)
  for _, row in ipairs(rows or {}) do
    local line = phrase:line(row.line)
    for _, cell in ipairs(row.notes or {}) do
      local column = line:note_column(cell.col)
      column.note_value, column.instrument_value, column.volume_value = cell.note, cell.instrument, cell.volume
      column.panning_value, column.delay_value = cell.panning, cell.delay
      column.effect_number_value, column.effect_amount_value = cell.fxn, cell.fxa
    end
    for _, cell in ipairs(row.effects or {}) do
      local column = line:effect_column(cell.col)
      column.number_value, column.amount_value = cell.number, cell.amount
    end
  end
end

local function lines_print(phrase)
  local rows = read_lines(phrase)
  local text = json.encode(rows, { sort_keys = true }) or ""
  local a, b = 5381, 0
  for i = 1, #text do
    local byte = text:byte(i)
    a = (a * 33 + byte) % 4294967296
    b = (b * 65599 + byte) % 4294967296
  end
  return string.format("%08x%08x", a, b)
end

-- The phrase's mapping (its keys in keymap mode): its note range, or nil.
local function mapping_of(phrase)
  local mapping = phrase.mapping
  if not mapping then return nil end
  return { noteRange = { mapping.note_range[1], mapping.note_range[2] } }
end

-- The mapping of the instrument whose keys start at `low` (mappings don't overlap: a start names one), its index.
local function mapping_index(instrument, low)
  for m = 1, #instrument.phrase_mappings do
    if instrument:phrase_mapping(m).note_range[1] == low then return m end
  end
  return nil
end

-- Where a mapping of [low, high] goes among the instrument's (sorted by key), or nil when it would overlap one.
local function mapping_slot(instrument, low, high, except_low)
  local index = 1
  for m = 1, #instrument.phrase_mappings do
    local range = instrument:phrase_mapping(m).note_range
    if range[1] ~= except_low then
      if not (high < range[1] or low > range[2]) then return nil end
      if range[2] < low then index = m + 1 end
    end
  end
  return index
end

-- Maps a phrase to [low, high] (or unmaps it with nil), as Renoise allows: answers true, or false and why.
local function set_mapping(instrument, phrase, wanted)
  local current = mapping_of(phrase)
  if wanted == nil then
    if not current then return true end
    local m = mapping_index(instrument, current.noteRange[1])
    if not m then return false, "its mapping wasn't found" end
    instrument:delete_phrase_mapping_at(m)
    return true
  end
  local low, high = wanted[1], wanted[2]
  if current then
    if not mapping_slot(instrument, low, high, current.noteRange[1]) then return false, "those keys overlap another phrase's" end
    phrase.mapping.note_range = { low, high }
    return true
  end
  local index = mapping_slot(instrument, low, high)
  if not index then return false, "those keys overlap another phrase's" end
  local made = instrument:insert_phrase_mapping_at(index, phrase)
  made.note_range = { low, high }
  return true
end

local function range_ok(v) return type(v) == "table" and whole(0, 119)(v[1]) and whole(0, 119)(v[2]) and v[1] <= v[2] end

-- phrase-add {ref: instrument, index?, ...settings}

local function add_plan(song, env, input)
  local n, objects, label = places_.locate(song, env, "instrument", input, "phrase-add")
  local instrument = objects.instrument
  local count = #instrument.phrases
  if count >= 126 then bad(q(label) .. " has 126 phrases: Renoise takes no more") end
  local index = input.index == nil and count + 1 or input.index
  if not whole(1, count + 1)(index) then bad(string.format("index is a place from 1 to %d among the phrases", count + 1)) end
  local settings = {}
  for _, key in ipairs(PHRASE_ORDER) do
    if input[key] ~= nil then
      local value = input[key]
      local field = PHRASE_FIELDS[key]
      if not field.check(value) then bad(field.says) end
      settings[key] = value
    end
  end
  -- (a new phrase has 16 lines unless given: its loop must fit what it will have)
  local lines = settings.lines or 16
  if (settings.loopStart or 1) > lines or (settings.loopEnd or 1) > lines then bad("the loop must be within the phrase's lines") end
  if settings.loopStart and settings.loopEnd and settings.loopStart > settings.loopEnd then bad("loopStart must not be after loopEnd") end
  return { n = n, label = label, count = count, index = index, settings = settings }
end

local function phrase_print(phrase)
  return { name = phrase.name, lines = phrase.number_of_lines, empty = phrase.is_empty, mode = phrase.playback_mode, print = lines_print(phrase) }
end

local function add_preview(song, env, input)
  local plan = add_plan(song, env, input)
  local title = string.format("Added phrase %s to %s", input.name and q(input.name) or string.format("%02X", plan.index - 1), q(plan.label))
  return {
    target = places_.target_of(env, "instrument", plan.n), prior = { phrases = plan.count }, fence = { phrases = plan.count, index = plan.index },
    display = { title = title, before = { phrases = plan.count }, after = { phrases = plan.count + 1 } },
  }
end

local function add_apply(song, env, input, fence, desc)
  local ok, plan = pcall(add_plan, song, env, input)
  if not ok then base.gone_is_changed(plan) end
  if plan.count ~= fence.phrases or plan.index ~= fence.index then fail("changed", q(plan.label) .. "'s phrases changed since the preview") end
  local instrument = song:instrument(plan.n[1])
  song:describe_undo(desc)
  local phrase = instrument:insert_phrase_at(plan.index)
  local set, err = pcall(write_settings, phrase, plan.settings)
  if not set then
    pcall(function() instrument:delete_phrase_at(plan.index) end)
    fail("failed", "Renoise wouldn't take those settings: " .. tostring(type(err) == "table" and err.message or err))
  end
  local m = { plan.n[1], plan.index }
  return {
    applied = { index = plan.index, print = phrase_print(phrase) }, created = { ref = places_.ref_of(env, "phrase", m), kind = "phrase" },
    material = { made = places_.place_of(env, "phrase", m) }, target = places_.target_of(env, "phrase", m),
    display = { title = string.format("Added phrase %s to %s", q(places_.phrase_label(plan.index, phrase)), q(plan.label)), before = { phrases = plan.count }, after = { phrases = plan.count + 1 } },
    undo = { desc = desc, entry = true }, prior = { phrases = plan.count }, place = places_.journal_place("phrase", m), shape = "made",
  }
end

local function add_restore(song, env, restore, desc)
  local material = restore.material
  if type(material) ~= "table" or type(material.made) ~= "table" then bad("material: the phrase the change made") end
  local n, why, which = places_.follow(env, material.made)
  if not n then
    if why == "gone" then fail("gone", "the phrase Kumi made isn't in the instrument any more") end
    places_.follow_failed(why, which, nil)
  end
  local objects = places_.resolve(song, "phrase", n)
  local label = phrase_label_of(n, objects)
  if not restore.discard and not base.same_table(phrase_print(objects.phrase), (restore.applied or {}).print) then
    fail("changed", label .. " changed in Renoise since Kumi made it (lines, length, name or script)")
  end
  song:describe_undo(desc)
  objects.instrument:delete_phrase_at(n[2])
  base.note_restored_at(env.journal, "phrase-add", places_.journal_place("phrase", n))
  return { restored = { deleted = true }, display = { title = "Removed " .. label .. " (put back)" }, undo = { desc = desc, entry = true } }
end

base.register({ kind = "phrase-add", custom = true, shape = "made", by_place = true, preview = add_preview, apply = add_apply, restore = add_restore })

-- phrase-delete {ref: phrase}: Kumi keeps its lines, settings, column names and mutes, key mapping and script, and whether it
-- was the instrument's program; the undo makes it again after the phrase that was before it.

local function columns_of(phrase)
  local names, muted = {}, {}
  for c = 1, 12 do
    local name = phrase:column_name(c)
    if name ~= "" then names[tostring(c)] = name end
    if phrase:column_is_muted(c) then muted[#muted + 1] = c end
  end
  return { names = next(names) and names or nil, muted = #muted > 0 and muted or nil }
end

local function delete_preview(song, env, input)
  local n, objects, label = places_.locate(song, env, "phrase", input, "phrase-delete")
  local _, writes = read_lines(objects.phrase)
  local display = { title = "Deleted phrase " .. label, note = "Kumi keeps its lines, settings, keys and script to put back. 0Z commands in patterns name phrases by number: the phrases after it move down one." }
  if writes > M.MAX_WRITES then display.note = "It holds more lines than Kumi puts back in one step: the apply will refuse." end
  return { target = places_.target_of(env, "phrase", n), prior = { deleted = false }, fence = phrase_print(objects.phrase), display = display }
end

local function delete_apply(song, env, input, fence, desc)
  local ok, n, objects, label = pcall(places_.locate, song, env, "phrase", input, "phrase-delete")
  if not ok then base.gone_is_changed(n) end
  local phrase, instrument = objects.phrase, objects.instrument
  if not base.same_table(phrase_print(phrase), fence) then fail("changed", label .. " changed since the preview") end
  local lines, writes = read_lines(phrase)
  if writes > M.MAX_WRITES then fail("failed", label .. " holds more lines than Kumi puts back in one step, so it won't delete it: clear some of its lines first, or delete it in Renoise") end
  local material = {
    index = n[2], settings = settings_of(phrase), lines = lines, columns = columns_of(phrase), mapping = mapping_of(phrase),
    script = phrase.script.paragraphs, program = (instrument.phrase_playback_mode == renoise.Instrument.PHRASES_PLAY_SELECTIVE and instrument.phrase_program == n[2]) or nil,
  }
  song:describe_undo(desc)
  instrument:delete_phrase_at(n[2])
  material.instrument = places_.place_of(env, "instrument", { n[1] })
  material.after = env.list_mark and env.list_mark("phrases", n[1]) or nil
  return {
    applied = { deleted = true }, material = material, target = places_.target_of(env, "instrument", { n[1] }),
    display = { title = "Deleted phrase " .. label }, undo = { desc = desc, entry = true }, prior = { deleted = false },
    place = places_.journal_place("phrase", n), shape = "deleted",
  }
end

local function delete_restore(song, env, restore, desc)
  local material = restore.material
  if type(material) ~= "table" or type(material.instrument) ~= "table" or type(material.settings) ~= "table" then bad("material: what the delete kept") end
  local label = q(material.settings.name ~= "" and material.settings.name or string.format("Phrase %02X", material.index - 1))
  local n, why, which = places_.follow(env, material.instrument)
  if not n then places_.follow_failed(why, which, material.settings.name) end
  local instrument = song:instrument(n[1])
  local at = env.list_after and env.list_after("phrases", n[1], nil, material.index, material.after) or nil
  if not at then fail("stale", string.format("the phrases changed in a way Kumi couldn't follow since it deleted %s: nothing was put back", label)) end
  at = math.max(1, math.min(at or 1, #instrument.phrases + 1))
  if at <= #instrument.phrases then
    local there = instrument:phrase(at)
    if there.name == material.settings.name and there.number_of_lines == material.settings.lines and json.encode(read_lines(there), { sort_keys = true }) == json.encode(material.lines, { sort_keys = true }) then
      base.note_restored_at(env.journal, "phrase-delete", places_.journal_place("phrase", { n[1], at }))
      return { restored = { ref = places_.ref_of(env, "phrase", { n[1], at }) }, display = { title = label .. " is back already" }, undo = { desc = desc, entry = false }, unwritten = true }
    end
  end
  if #instrument.phrases >= 126 then fail("changed", "the instrument has 126 phrases now: there's no room to put it back") end
  song:describe_undo(desc)
  local phrase = instrument:insert_phrase_at(at)
  local notes = {}
  pcall(write_settings, phrase, material.settings)
  write_lines(phrase, material.lines)
  for key, name in pairs((material.columns or {}).names or {}) do pcall(function() phrase:set_column_name(tonumber(key), name) end) end
  for _, c in ipairs((material.columns or {}).muted or {}) do pcall(function() phrase:set_column_is_muted(c, true) end) end
  if type(material.script) == "table" then
    pcall(function()
      phrase.script.paragraphs = material.script
      phrase.script:commit()
    end)
  end
  if material.mapping then
    local mapped, why_not = set_mapping(instrument, phrase, material.mapping.noteRange)
    if not mapped then notes[#notes + 1] = string.format("Its keys (%d-%d) weren't mapped again: %s.", material.mapping.noteRange[1], material.mapping.noteRange[2], why_not) end
  end
  if material.program then
    pcall(function()
      if instrument.phrase_playback_mode == renoise.Instrument.PHRASES_PLAY_SELECTIVE and (instrument.phrase_program == 0 or instrument.phrase_program > #instrument.phrases) then
        instrument.phrase_program = at
      end
    end)
  end
  base.note_restored_at(env.journal, "phrase-delete", places_.journal_place("phrase", { n[1], at }))
  return {
    restored = { ref = places_.ref_of(env, "phrase", { n[1], at }) }, display = { title = label .. " put back", note = #notes > 0 and table.concat(notes, " ") or nil },
    undo = { desc = desc, entry = true },
  }
end

base.register({ kind = "phrase-delete", custom = true, shape = "deleted", by_place = true, preview = delete_preview, apply = delete_apply, restore = delete_restore })

-- phrase-map {ref: phrase, noteRange?: [low, high] | null, keyTracking?, baseNote?, looping?, loopStart?, loopEnd?}: the phrase's
-- keys in keymap mode (mappings may not overlap) and the mapping's own key tracking, base note and loop, which are its own, not
-- the phrase's (lab: setting one leaves the other).

local MAPPING_FIELDS = {
  keyTracking = { get = function(m) return KEY_TRACKING.get(m.key_tracking) end, set = function(m, v) m.key_tracking = KEY_TRACKING.parse(v) end,
    check = function(v) return KEY_TRACKING.parse(v) ~= nil end, says = "keyTracking is none, transpose or offset" },
  baseNote = { get = function(m) return m.base_note end, set = function(m, v) m.base_note = v end, check = whole(0, 119), says = "baseNote is a note from 0 to 119" },
  looping = { get = function(m) return m.looping end, set = function(m, v) m.looping = v end, check = is_boolean, says = "looping is true or false" },
  loopStart = { get = function(m) return m.loop_start end, set = function(m, v) m.loop_start = v end, check = whole(1, 512), says = "loopStart is a line of the phrase" },
  loopEnd = { get = function(m) return m.loop_end end, set = function(m, v) m.loop_end = v end, check = whole(1, 512), says = "loopEnd is a line of the phrase" },
}
local MAPPING_ORDER = { "keyTracking", "baseNote", "looping", "loopStart", "loopEnd" }

local function same_range(a, b)
  if not a or not b then return (not a) == (not b) end
  return a[1] == b[1] and a[2] == b[2]
end

-- The mapping's state for the keys a change names: its range (false when unmapped) and the fields asked.
local function map_state(phrase, keys)
  local mapping = phrase.mapping
  local state = { noteRange = mapping and { mapping.note_range[1], mapping.note_range[2] } or false }
  for _, key in ipairs(MAPPING_ORDER) do
    if keys[key] ~= nil and mapping then state[key] = MAPPING_FIELDS[key].get(mapping) end
  end
  return state
end

local function same_map(a, b)
  if not same_range(a.noteRange or nil, b.noteRange or nil) then return false end
  for _, key in ipairs(MAPPING_ORDER) do
    if (a[key] ~= nil or b[key] ~= nil) and a[key] ~= b[key] then return false end
  end
  return true
end

local function write_map(instrument, phrase, state)
  local done, why = set_mapping(instrument, phrase, state.noteRange or nil)
  if not done then return false, why end
  local mapping = phrase.mapping
  if mapping then
    -- (the loop's ends in the order that keeps its start before its end)
    if state.loopStart and state.loopEnd and state.loopStart > mapping.loop_end then
      mapping.loop_end = state.loopEnd
      mapping.loop_start = state.loopStart
      state = (function() local c = {} for k, v in pairs(state) do c[k] = v end c.loopStart, c.loopEnd = nil, nil return c end)()
    end
    for _, key in ipairs(MAPPING_ORDER) do
      if state[key] ~= nil then MAPPING_FIELDS[key].set(mapping, state[key]) end
    end
  end
  return true
end

local function map_plan(song, env, input)
  local n, objects, label = places_.locate(song, env, "phrase", input, "phrase-map")
  local phrase = objects.phrase
  local current = map_state(phrase, input)
  local wanted = {}
  local range = input.noteRange
  if range == nil then
    wanted.noteRange = current.noteRange
  elseif range == json.null or range == false then
    wanted.noteRange = false
  elseif range_ok(range) then
    wanted.noteRange = { range[1], range[2] }
  else
    bad("noteRange is [low, high], notes from 0 to 119 (C-4 is 48), or null to unmap it")
  end
  local any = range ~= nil
  for _, key in ipairs(MAPPING_ORDER) do
    if input[key] ~= nil then
      if not MAPPING_FIELDS[key].check(input[key]) then bad(MAPPING_FIELDS[key].says) end
      if not wanted.noteRange then bad("the phrase has no keys: give noteRange to map it (its key tracking, base note and loop are the mapping's)") end
      wanted[key], any = input[key], true
    end
  end
  if not any then bad("give noteRange (or null to unmap), keyTracking, baseNote, looping, loopStart or loopEnd") end
  if wanted.loopStart and wanted.loopEnd and wanted.loopStart > wanted.loopEnd then bad("loopStart must not be after loopEnd") end
  for _, key in ipairs({ "loopStart", "loopEnd" }) do
    if wanted[key] and wanted[key] > phrase.number_of_lines then bad(key .. " is a line of the phrase (it has " .. phrase.number_of_lines .. ")") end
  end
  if wanted.noteRange and not mapping_slot(objects.instrument, wanted.noteRange[1], wanted.noteRange[2], current.noteRange and current.noteRange[1] or nil) then
    bad("those keys overlap another phrase's mapping")
  end
  -- (the prior names every field the change sets: what the undo puts back)
  local prior = map_state(phrase, wanted)
  return { n = n, label = label, wanted = wanted, prior = prior, instrument = objects.instrument, phrase = phrase }
end

local function map_title(plan)
  local wanted = plan.wanted
  if not wanted.noteRange then return "Unmapped " .. plan.label end
  local parts = {}
  if not same_range(wanted.noteRange, plan.prior.noteRange or nil) then parts[#parts + 1] = string.format("notes %d-%d", wanted.noteRange[1], wanted.noteRange[2]) end
  if wanted.keyTracking then parts[#parts + 1] = "key tracking " .. wanted.keyTracking end
  if wanted.baseNote then parts[#parts + 1] = "base note " .. wanted.baseNote end
  if wanted.looping ~= nil then parts[#parts + 1] = wanted.looping and "looping" or "one-shot" end
  if wanted.loopStart then parts[#parts + 1] = "loop from line " .. wanted.loopStart end
  if wanted.loopEnd then parts[#parts + 1] = "loop to line " .. wanted.loopEnd end
  if #parts == 0 then parts[1] = string.format("notes %d-%d", wanted.noteRange[1], wanted.noteRange[2]) end
  return string.format("Mapped %s: %s", plan.label, table.concat(parts, ", "))
end

base.register({
  kind = "phrase-map", custom = true, by_place = true,
  preview = function(song, env, input)
    local plan = map_plan(song, env, input)
    local display = { title = map_title(plan), before = plan.prior, after = plan.wanted }
    if same_map(plan.prior, plan.wanted) then display.unchanged = true end
    return { target = places_.target_of(env, "phrase", plan.n), prior = plan.prior, fence = plan.prior, display = display }
  end,
  apply = function(song, env, input, fence, desc)
    local ok, plan = pcall(map_plan, song, env, input)
    if not ok then base.gone_is_changed(plan) end
    if not same_map(plan.prior, fence) then fail("changed", plan.label .. "'s mapping changed since the preview") end
    local display = { title = map_title(plan), before = plan.prior, after = plan.wanted }
    if same_map(plan.prior, plan.wanted) then
      display.unchanged = true
      return { applied = plan.prior, display = display, undo = { desc = desc, entry = false }, prior = plan.prior, target = places_.target_of(env, "phrase", plan.n) }
    end
    song:describe_undo(desc)
    local done, why = write_map(plan.instrument, plan.phrase, plan.wanted)
    if not done then fail("failed", "Renoise didn't map it: " .. tostring(why)) end
    local now = map_state(plan.phrase, plan.wanted)
    if env.watch_undo then
      env.watch_undo(plan.instrument.phrase_mappings_observable)
      local mapping = plan.phrase.mapping
      if mapping then
        for _, field in ipairs({ "note_range", "key_tracking", "base_note", "looping", "loop_start", "loop_end" }) do env.watch_undo(mapping[field .. "_observable"]) end
      end
    end
    return {
      applied = now, target = places_.target_of(env, "phrase", plan.n), display = display,
      undo = { desc = desc, entry = true }, prior = plan.prior, place = places_.journal_place("phrase", plan.n),
    }
  end,
  restore = function(song, env, restore, desc)
    local prior, applied = restore.prior, restore.applied
    if type(prior) ~= "table" or type(applied) ~= "table" then bad("prior and applied: the mapping") end
    local n, objects, label = places_.locate(song, env, "phrase", { place = restore.target and restore.target.place }, "phrase-map")
    local now = map_state(objects.phrase, applied)
    if same_map(now, prior) then
      return { restored = prior, display = { title = label .. "'s mapping is back already" }, undo = { desc = desc, entry = false }, unwritten = true }
    end
    if not restore.discard and not same_map(now, applied) then fail("changed", label .. "'s mapping changed in Renoise since Kumi set it") end
    song:describe_undo(desc)
    local done, why = write_map(objects.instrument, objects.phrase, prior)
    if not done then fail("changed", "its old keys can't be mapped again: " .. tostring(why)) end
    base.note_restored(env.journal, "phrase-map", places_.journal_place("phrase", n), applied, base.KINDS["phrase-map"])
    return { restored = prior, display = { title = label .. "'s mapping put back" }, undo = { desc = desc, entry = true } }
  end,
  reread = places_.reread("phrase"),
  read = function(phrase, shape) return map_state(phrase, shape or {}) end,
  same = same_map,
})

-- phrase-playback {ref: instrument, mode: off | program | keymap, programRef?}: the instrument's phrase playback; in program
-- mode, the phrase every key plays (kept as its place).

local MODES = named({ off = 1, program = 2, keymap = 3 })

local playback = instruments_.settings_kind({
  kind = "phrase-playback", noun = "phrase playback", undoable = true, order = { "mode", "program" },
  fields = {
    mode = { get = function(i) return MODES.get(i.phrase_playback_mode) end, set = function(i, v) i.phrase_playback_mode = MODES.parse(v) end,
      field = "phrase_playback_mode", check = function(v) return MODES.parse(v) ~= nil end, says = "mode is off, program or keymap",
      text = function(v) return v == "off" and "phrases off" or ("phrases by " .. v) end },
    program = { get = function(i) return i.phrase_program end, set = function(i, v) if v >= 1 and v <= 126 then i.phrase_program = v end end, field = "phrase_program",
      text = function(v) return "phrase " .. string.format("%02X", v - 1) end },
  },
})
-- (program mode with no phrase chosen, program 0, is what a new instrument has: lab. A state keeps both as Renoise has them)
playback.read = function(instrument)
  return { mode = MODES.get(instrument.phrase_playback_mode), program = instrument.phrase_program }
end
playback.wanted_at = function(loc, input)
  local instrument = loc.object
  local mode = input.mode
  if MODES.parse(mode) == nil then bad("mode is off, program or keymap") end
  local state = { mode = mode, program = mode == "off" and 0 or 127 }
  if mode == "program" then
    local program
    if input.programRef ~= nil then
      local found = places_.numbers_of(loc.env, "phrase", input.programRef, "programRef")
      if found[1] ~= loc.numbers[1] then bad("programRef must be a phrase of that instrument") end
      program = found[2]
    elseif instrument.phrase_program >= 1 and instrument.phrase_program <= #instrument.phrases then
      program = instrument.phrase_program
    else
      program = 1
    end
    if program > #instrument.phrases then bad("the instrument has no phrase to play: add one first") end
    state.program = program
  elseif input.programRef ~= nil then
    bad("programRef goes with mode program")
  end
  return state
end
playback.wanted = nil
-- (the program first: Renoise sets the mode from it; then the mode, which keeps a program of program mode)
playback.write = function(instrument, state)
  if type(state.program) == "number" and instrument.phrase_program ~= state.program then instrument.phrase_program = state.program end
  local mode = MODES.parse(state.mode)
  if instrument.phrase_playback_mode ~= mode then instrument.phrase_playback_mode = mode end
end
playback.same = function(a, b) return a.mode == b.mode and a.program == b.program end
playback.title = function(name, _, to)
  if to.mode == "program" and type(to.program) == "number" and to.program >= 1 then return string.format("%s plays phrase %02X on every key", name, to.program - 1) end
  if to.mode == "program" then return name .. ": phrases by program, none chosen" end
  return name .. (to.mode == "off" and ": phrases off" or ": phrases by keymap")
end
playback.observables = function(instrument) return { instrument.phrase_playback_mode_observable, instrument.phrase_program_observable } end
-- (the program's phrase on the wire is its place)
playback.wire = function(state, env, loc)
  if type(state) ~= "table" or type(state.program) ~= "number" or state.program < 1 or not loc or not loc.numbers then return state end
  local out = {}
  for k, v in pairs(state) do out[k] = v end
  out.program = { place = places_.place_of(env, "phrase", { loc.numbers[1], state.program }) }
  return out
end
playback.unwire = function(state, env)
  if type(state) ~= "table" or type(state.program) ~= "table" then return state end
  local n, why, which = places_.follow(env, state.program.place)
  if not n then places_.follow_failed(why, which, "the program's phrase") end
  local out = {}
  for k, v in pairs(state) do out[k] = v end
  out.program = n[2]
  return out
end
instruments_.instrument_spec(playback)
do
  local locate = playback.locate
  playback.locate = function(song, env, input)
    local loc = locate(song, env, input)
    loc.env = env
    return loc
  end
end

-- phrase-write {ref: phrase, lines: [...], merge?} and phrase-clear {ref: phrase, first?, last?}: a phrase's cells, as
-- notes-write writes a pattern's (instrument values in a phrase are its samples, counted from 00). The columns the cells need
-- are shown first and hidden again by the undo when nothing else is in them.

local function phrase_columns(phrase)
  return {
    note = phrase.visible_note_columns, effect = phrase.visible_effect_columns, volume = phrase.volume_column_visible,
    panning = phrase.panning_column_visible, delay = phrase.delay_column_visible, sampleEffects = phrase.sample_effects_column_visible,
    instrument = phrase.instrument_column_visible,
  }
end

local function phrase_needs(after)
  local needs = cells.needs_of(after)
  for _, cell in ipairs(after) do
    if cell.kind == "note" and cell.instrument ~= ".." and not cells.is_empty_cell(cell) then needs.instrument = true end
  end
  return needs
end

local function raise(phrase, needs)
  if needs.note > 0 and phrase.visible_note_columns < needs.note then phrase.visible_note_columns = needs.note end
  if needs.effect > 0 and phrase.visible_effect_columns < needs.effect then phrase.visible_effect_columns = needs.effect end
  if needs.volume and not phrase.volume_column_visible then phrase.volume_column_visible = true end
  if needs.panning and not phrase.panning_column_visible then phrase.panning_column_visible = true end
  if needs.delay and not phrase.delay_column_visible then phrase.delay_column_visible = true end
  if needs.sampleEffects and not phrase.sample_effects_column_visible then phrase.sample_effects_column_visible = true end
  if needs.instrument and not phrase.instrument_column_visible then phrase.instrument_column_visible = true end
end

local function set_phrase_columns(phrase, columns)
  pcall(function()
    if columns.note and phrase.visible_note_columns ~= columns.note then phrase.visible_note_columns = columns.note end
    if columns.effect and phrase.visible_effect_columns ~= columns.effect then phrase.visible_effect_columns = columns.effect end
    for key, field in pairs({ volume = "volume_column_visible", panning = "panning_column_visible", delay = "delay_column_visible",
      sampleEffects = "sample_effects_column_visible", instrument = "instrument_column_visible" }) do
      if columns[key] ~= nil and phrase[field] ~= columns[key] then phrase[field] = columns[key] end
    end
  end)
end

-- What is in the phrase's columns (every line): the highest note and effect columns used, and the sub-columns with something.
local function used_columns(phrase)
  local used = { note = 0, effect = 0 }
  for l = 1, phrase.number_of_lines do
    local line = phrase:line(l)
    if not line.is_empty then
      for c = 1, 12 do
        local column = line:note_column(c)
        if not column.is_empty then
          if c > used.note then used.note = c end
          if column.volume_string ~= ".." then used.volume = true end
          if column.panning_string ~= ".." then used.panning = true end
          if column.delay_string ~= ".." then used.delay = true end
          if column.instrument_string ~= ".." then used.instrument = true end
          if column.effect_number_value ~= 0 or column.effect_amount_value ~= 0 then used.sampleEffects = true end
        end
      end
      for c = 1, 8 do
        if not line:effect_column(c).is_empty and c > used.effect then used.effect = c end
      end
    end
  end
  return used
end

local function lower(phrase, prior, applied)
  if type(prior) ~= "table" or type(applied) ~= "table" then return end
  local now, used = phrase_columns(phrase), used_columns(phrase)
  local back = {}
  if prior.note and now.note == applied.note and applied.note > prior.note then back.note = math.max(prior.note, used.note, 1) end
  if prior.effect and now.effect == applied.effect and applied.effect > prior.effect then back.effect = math.max(prior.effect, used.effect) end
  for _, flag in ipairs({ "volume", "panning", "delay", "sampleEffects", "instrument" }) do
    if prior[flag] == false and applied[flag] == true and now[flag] == true and not used[flag] then back[flag] = false end
  end
  if next(back) then set_phrase_columns(phrase, back) end
end

-- What a request's lines name in the phrase: the cells and what they get (cells.parse_cells takes a pattern and a track:
-- a phrase has its lines and 12 note and 8 effect columns).
local function lines_plan(phrase, input)
  local shape_pattern = { number_of_lines = phrase.number_of_lines }
  local shape_track = { max_note_columns = 12, max_effect_columns = 8 }
  local wants = cells.parse_cells(shape_pattern, shape_track, input)
  cells.add_cleared(phrase, input, wants)
  local prior, after = cells.plan_of(phrase, wants, input.merge == true)
  if cells.writes_of(after) > M.MAX_WRITES then
    bad(string.format("that is %d writes, more than one phrase change takes (%d): split it into several", cells.writes_of(after), M.MAX_WRITES))
  end
  return prior, after
end

local function lines_title(verb, cells_, label)
  local n = cells.lines_in(cells_)
  return string.format("%s %s in %s", verb, n == 1 and "1 line" or (n .. " lines"), label)
end

local function clear_input(phrase, input)
  local first, last = input.first or 1, input.last or phrase.number_of_lines
  for _, value in ipairs({ first, last }) do
    if not whole(1, phrase.number_of_lines)(value) then bad(string.format("first and last are lines from 1 to %d", phrase.number_of_lines)) end
  end
  if first > last then bad("first must not come after last") end
  local lines = {}
  for l = first, last do
    if not phrase:line(l).is_empty then lines[#lines + 1] = { line = l, clear = true } end
  end
  return { lines = lines }
end

local function lines_kind(spec)
  local kind, verb, noun, build = spec.kind, spec.verb, spec.noun, spec.build
  local function plan(song, env, input)
    local n, objects, label = places_.locate(song, env, "phrase", input, kind)
    local phrase = objects.phrase
    local request = build(phrase, input)
    local prior, after
    if #(request.lines or {}) == 0 and kind == "phrase-clear" then
      prior, after = {}, {}
    else
      prior, after = lines_plan(phrase, request)
    end
    return { n = n, label = label, phrase = phrase, prior = prior, after = after, columns = phrase_columns(phrase) }
  end
  local function preview(song, env, input)
    local p = plan(song, env, input)
    local display = { title = lines_title(verb, #p.after > 0 and p.after or p.prior, p.label), before = cells.counts_of(p.prior), after = cells.counts_of(p.after) }
    if #p.after == 0 then display.unchanged = true end
    return {
      target = places_.target_of(env, "phrase", p.n), prior = { cells = p.prior, columns = p.columns },
      fence = { cells = #p.prior, hash = cells.fingerprint(p.prior) }, display = display,
    }
  end
  local function apply(song, env, input, fence, desc)
    local ok, p = pcall(plan, song, env, input)
    if not ok then base.gone_is_changed(p) end
    if #p.prior ~= fence.cells or cells.fingerprint(p.prior) ~= fence.hash then fail("changed", p.label .. " changed since the preview") end
    local display = { title = lines_title(verb, #p.after > 0 and p.after or p.prior, p.label), before = cells.counts_of(p.prior), after = cells.counts_of(p.after) }
    if #p.after == 0 then
      display.unchanged = true
      return { applied = { cells = {}, columns = p.columns }, display = display, undo = { desc = desc, entry = false }, prior = { cells = p.prior, columns = p.columns }, target = places_.target_of(env, "phrase", p.n) }
    end
    song:describe_undo(desc)
    local written = 0
    local wrote, err = pcall(function()
      raise(p.phrase, phrase_needs(p.after))
      for i, cell in ipairs(p.after) do
        cells.write_cell(p.phrase, cell)
        written = i
      end
    end)
    if not wrote then
      pcall(function() for i = 1, written + 1 do if p.prior[i] then cells.write_cell(p.phrase, p.prior[i]) end end end)
      set_phrase_columns(p.phrase, p.columns)
      fail("failed", "Renoise wouldn't take those cells: " .. tostring(type(err) == "table" and err.message or err):gsub("^[^%s:]*%.lua:%d+: ", ""))
    end
    local result = {}
    for i, cell in ipairs(p.after) do result[i] = cells.read_cell(p.phrase, cell) end
    for i, cell in ipairs(result) do
      if not cells.same_cell(cell, p.after[i]) then
        for j = 1, #p.after do cells.write_cell(p.phrase, p.prior[j]) end
        set_phrase_columns(p.phrase, p.columns)
        fail("failed", string.format("Renoise didn't keep line %d column %d as written", cell.line, cell.col))
      end
    end
    return {
      applied = {
        cells = result, columns = phrase_columns(p.phrase),
        touch = env.phrase_touch_mark and env.phrase_touch_mark(p.n[1], p.n[2]) or nil, lines = p.phrase.number_of_lines,
      },
      target = places_.target_of(env, "phrase", p.n), display = { title = display.title, before = display.before, after = cells.counts_of(result) },
      undo = { desc = desc, entry = true }, prior = { cells = p.prior, columns = p.columns }, place = places_.journal_place("phrase", p.n),
    }
  end
  local function restore(song, env, restore_, desc)
    local prior, applied = restore_.prior and restore_.prior.cells, restore_.applied and restore_.applied.cells
    if type(prior) ~= "table" or type(applied) ~= "table" or #prior ~= #applied then bad("prior and applied: the cells the change wrote") end
    local n, objects, label = places_.locate(song, env, "phrase", { place = restore_.target and restore_.target.place }, kind)
    local phrase = objects.phrase
    if #applied > 0 then
      if type(restore_.applied.lines) == "number" and restore_.applied.lines ~= phrase.number_of_lines then
        fail("changed", label .. "'s length changed in Renoise since Kumi wrote these lines, so Kumi left them as they are")
      end
      local since = env.phrase_touched_since and env.phrase_touched_since(n[1], n[2], restore_.applied.touch)
      if since == nil then fail("changed", "Kumi can't tell whether " .. label .. " changed in Renoise since (the tool watched the song anew), so Kumi left its lines as they are") end
      if since then
        fail("changed", label .. " changed in Renoise since Kumi's step (an edit, an undo or redo, or another change of Kumi's), so Kumi left its lines as they are: Renoise's own undo can take them back, after the edits made since")
      end
    end
    local todo = {}
    for i = 1, #prior do
      local now = cells.read_cell(phrase, prior[i])
      if not cells.same_cell(now, prior[i]) then
        if not cells.same_cell(now, applied[i]) then fail("changed", string.format("line %d of %s was changed in Renoise since Kumi wrote it", prior[i].line, label)) end
        todo[#todo + 1] = prior[i]
      end
    end
    local place = places_.journal_place("phrase", n)
    if #todo == 0 then
      lower(phrase, restore_.prior.columns, restore_.applied.columns)
      base.note_restored(env.journal, kind, place, restore_.applied, base.KINDS[kind])
      return { restored = { done = true }, display = { title = noun .. " in " .. label .. " are back already" }, undo = { desc = desc, entry = false }, unwritten = true }
    end
    song:describe_undo(desc)
    for _, cell in ipairs(todo) do cells.write_cell(phrase, cell) end
    lower(phrase, restore_.prior.columns, restore_.applied.columns)
    base.note_restored(env.journal, kind, place, restore_.applied, base.KINDS[kind])
    return { restored = { done = true }, display = { title = noun .. " put back in " .. label, before = cells.counts_of(todo), after = cells.counts_of(prior) }, undo = { desc = desc, entry = true } }
  end
  base.register({
    kind = kind, custom = true, by_place = true, preview = preview, apply = apply, restore = restore, unsure_redo = true,
    reread = places_.reread("phrase"),
    read = function(phrase, shape)
      local out = {}
      for i, at in ipairs(shape.cells or {}) do out[i] = cells.read_cell(phrase, at) end
      return { cells = out }
    end,
    same = function(a, b) return cells.same_cells(a.cells, b.cells) end,
  })
end

lines_kind({ kind = "phrase-write", verb = "Wrote", noun = "Lines", build = function(_, input) return input end })
lines_kind({ kind = "phrase-clear", verb = "Cleared", noun = "Cells", build = clear_input })

-- phrase-script {ref: phrase, code, commit?}: the script's text (as its lines), committed for playback unless commit: false;
-- the phrase plays its script. Renoise compiles a committed script in its player, a moment later: the apply waits for that
-- (a job; the one write is its first step) and says what Renoise reported.

local function split_lines(code)
  local out = {}
  for line in (code .. "\n"):gmatch("([^\n]*)\n") do out[#out + 1] = (line:gsub("\r$", "")) end
  if #out > 1 and out[#out] == "" then out[#out] = nil end
  return out
end

local function script_state(phrase)
  local script = phrase.script
  return { mode = PLAYBACK.get(phrase.playback_mode), paragraphs = script.paragraphs }
end

local function same_paragraphs(a, b)
  if type(a) ~= "table" or type(b) ~= "table" or #a ~= #b then return false end
  for k = 1, #a do if a[k] ~= b[k] then return false end end
  return true
end

local function script_plan(song, env, input)
  local n, objects, label = places_.locate(song, env, "phrase", input, "phrase-script")
  if type(input.code) ~= "string" or input.code == "" then bad("code: the script's text (pattrns Lua, returning a pattern or cycle)") end
  if #input.code > M.MAX_SCRIPT_BYTES then bad("code is longer than 64 KB") end
  if input.commit ~= nil and type(input.commit) ~= "boolean" then bad("commit is true or false") end
  return { n = n, label = label, phrase = objects.phrase, paragraphs = split_lines(input.code), state = script_state(objects.phrase), commit = input.commit ~= false }
end

local function script_title(plan) return string.format("Wrote the script of %s (%d %s)", plan.label, #plan.paragraphs, #plan.paragraphs == 1 and "line" or "lines") end

base.register({
  kind = "phrase-script", custom = true, by_place = true,
  preview = function(song, env, input)
    local plan = script_plan(song, env, input)
    local display = { title = script_title(plan), note = plan.commit and "Renoise compiles it as it is committed and says if it can't." or "Not committed: Renoise plays the script it had until it is." }
    if plan.state.mode == "script" and same_paragraphs(plan.state.paragraphs, plan.paragraphs) and plan.commit == false then display.unchanged = true end
    return { target = places_.target_of(env, "phrase", plan.n), prior = plan.state, fence = plan.state, display = display }
  end,
  apply = function(song, env, input, fence, desc)
    local ok, plan = pcall(script_plan, song, env, input)
    if not ok then base.gone_is_changed(plan) end
    if plan.state.mode ~= fence.mode or not same_paragraphs(plan.state.paragraphs, fence.paragraphs) then fail("changed", plan.label .. "'s script changed since the preview") end
    local phrase = plan.phrase
    local script = phrase.script
    local before = { compile = script.compile_error, runtime = script.runtime_error }
    song:describe_undo(desc)
    if phrase.playback_mode ~= renoise.InstrumentPhrase.PLAY_SCRIPT then phrase.playback_mode = renoise.InstrumentPhrase.PLAY_SCRIPT end
    script.paragraphs = plan.paragraphs
    if plan.commit then script:commit() end
    if env.watch_undo then env.watch_undo(script.paragraphs_assignment_observable) env.watch_undo(script.paragraphs_observable) env.watch_undo(phrase.playback_mode_observable) end
    local n = plan.n
    local function answer(compile, runtime, waited)
      local applied = script_state(phrase)
      applied.compileError = compile ~= "" and compile or json.null
      applied.runtimeError = runtime ~= "" and runtime or json.null
      local note
      if compile ~= "" then note = "Renoise couldn't compile it: " .. compile
      elseif runtime ~= "" then note = "It compiled, and fails as it runs: " .. runtime
      elseif plan.commit and not waited then note = "Renoise hadn't compiled it yet when Kumi looked: it says errors in the phrase editor." end
      return {
        applied = applied, target = places_.target_of(env, "phrase", n),
        display = { title = script_title(plan), note = note }, undo = { desc = desc, entry = true },
        prior = plan.state, journal_applied = { mode = applied.mode, paragraphs = applied.paragraphs }, place = places_.journal_place("phrase", n),
      }
    end
    if not plan.commit then return answer("", "", true) end
    -- (the compile lands a moment later: Kumi waits, writing nothing more)
    return {
      kumi_job = function(job)
        local started = env.now_ms()
        local waited = false
        while env.now_ms() - started < M.COMPILE_WAIT_MS do
          job.step(50)
          local live = places_.resolve(renoise.song() or song, "phrase", n).phrase.script
          if live.pending_changes == 0 and (live.compile_error ~= before.compile or live.runtime_error ~= before.runtime or env.now_ms() - started >= 200) then
            waited = true
            break
          end
        end
        local live = places_.resolve(renoise.song() or song, "phrase", n).phrase.script
        return answer(live.compile_error, live.runtime_error, waited)
      end,
    }
  end,
  restore = function(song, env, restore, desc)
    local prior, applied = restore.prior, restore.applied
    if type(prior) ~= "table" or type(applied) ~= "table" then bad("prior and applied: the script") end
    local n, objects, label = places_.locate(song, env, "phrase", { place = restore.target and restore.target.place }, "phrase-script")
    local phrase = objects.phrase
    local now = script_state(phrase)
    if now.mode == prior.mode and same_paragraphs(now.paragraphs, prior.paragraphs) then
      return { restored = { done = true }, display = { title = label .. "'s script is back already" }, undo = { desc = desc, entry = false }, unwritten = true }
    end
    if not restore.discard and (now.mode ~= applied.mode or not same_paragraphs(now.paragraphs, applied.paragraphs)) then
      fail("changed", label .. "'s script changed in Renoise since Kumi wrote it")
    end
    song:describe_undo(desc)
    phrase.script.paragraphs = prior.paragraphs
    phrase.script:commit()
    local mode = PLAYBACK.parse(prior.mode)
    if mode and phrase.playback_mode ~= mode then phrase.playback_mode = mode end
    base.note_restored(env.journal, "phrase-script", places_.journal_place("phrase", n), { mode = applied.mode, paragraphs = applied.paragraphs }, base.KINDS["phrase-script"])
    return { restored = { done = true }, display = { title = label .. "'s script put back" }, undo = { desc = desc, entry = true } }
  end,
  reread = places_.reread("phrase"), read = function(phrase) local s = script_state(phrase) return { mode = s.mode, paragraphs = s.paragraphs } end,
  same = function(a, b) return a.mode == b.mode and same_paragraphs(a.paragraphs, b.paragraphs) end,
})

-- phrase-render {ref: phrase, lpb?, maxEvents?}: the committed script rendered to the phrase's lines (Renoise's render, which
-- answers later); the phrase plays its lines after. Kumi keeps the lines it had and puts them back on undo.

local function render_plan(song, env, input)
  local n, objects, label = places_.locate(song, env, "phrase", input, "phrase-render")
  local phrase = objects.phrase
  if phrase.playback_mode ~= renoise.InstrumentPhrase.PLAY_SCRIPT then bad(label .. " plays its lines, not a script: write its script first (phrase-script)") end
  if phrase.script.compile_error ~= "" then bad(label .. "'s script doesn't compile: " .. phrase.script.compile_error) end
  if input.lpb ~= nil and not whole(1, 256)(input.lpb) then bad("lpb is from 1 to 256") end
  if input.maxEvents ~= nil and not whole(1, 65536)(input.maxEvents) then bad("maxEvents is from 1 to 65536") end
  local lines, writes = read_lines(phrase)
  if writes > M.MAX_WRITES then bad(label .. " holds more lines than Kumi puts back in one step: clear it first") end
  return { n = n, label = label, phrase = phrase, lines = lines, settings = settings_of(phrase) }
end

base.register({
  kind = "phrase-render", custom = true, by_place = true,
  preview = function(song, env, input)
    local plan = render_plan(song, env, input)
    return {
      target = places_.target_of(env, "phrase", plan.n), prior = { lines = plan.lines, settings = plan.settings },
      fence = { print = lines_print(plan.phrase), mode = plan.phrase.playback_mode },
      display = { title = "Rendered the script of " .. plan.label .. " to its lines", note = "Kumi keeps the lines it had, to put back." },
    }
  end,
  apply = function(song, env, input, fence, desc)
    local ok, plan = pcall(render_plan, song, env, input)
    if not ok then base.gone_is_changed(plan) end
    if lines_print(plan.phrase) ~= fence.print or plan.phrase.playback_mode ~= fence.mode then fail("changed", plan.label .. " changed since the preview") end
    local n = plan.n
    local result
    song:describe_undo(desc)
    local options = { lpb = input.lpb, max_events = input.maxEvents }
    local started, err = pcall(function()
      plan.phrase.script:render_to_pattern(options, function(error_text, rendered, skipped)
        result = { error = error_text, rendered = rendered, skipped = skipped }
      end)
    end)
    if not started then fail("failed", "Renoise wouldn't render it: " .. tostring(type(err) == "table" and err.message or err)) end
    return {
      kumi_job = function(job)
        local from = env.now_ms()
        while not result do
          if env.now_ms() - from >= M.RENDER_WAIT_MS then
            error({ code = "uncertain", message = "Renoise didn't say it had rendered " .. plan.label .. " in time: look at its lines; Kumi keeps what it had" }, 0)
          end
          job.step(50)
        end
        if result.error then error({ code = "failed", message = "Renoise couldn't render " .. plan.label .. ": " .. tostring(result.error) }, 0) end
        local phrase = places_.resolve(renoise.song() or song, "phrase", n).phrase
        local display = { title = string.format("Rendered the script of %s to %d events", plan.label, result.rendered or 0) }
        if (result.skipped or 0) > 0 then display.note = string.format("%d events didn't fit its lines (raise lpb to fit them).", result.skipped) end
        return {
          applied = { print = lines_print(phrase), mode = phrase.playback_mode, rendered = result.rendered, skipped = result.skipped },
          material = { at = places_.place_of(env, "phrase", n), lines = plan.lines, settings = plan.settings },
          target = places_.target_of(env, "phrase", n), display = display, undo = { desc = desc, entry = true },
          prior = { lines = plan.lines, settings = plan.settings },
        }
      end,
    }
  end,
  restore = function(song, env, restore, desc)
    local material, applied = restore.material, restore.applied
    if type(material) ~= "table" or type(material.at) ~= "table" or type(applied) ~= "table" then bad("material: what the render replaced") end
    local n, why, which = places_.follow(env, material.at)
    if not n then places_.follow_failed(why, which, nil) end
    local objects = places_.resolve(song, "phrase", n)
    local phrase, label = objects.phrase, places_.label_of("phrase", n, objects)
    local now = lines_print(phrase)
    if not restore.discard and (now ~= applied.print or phrase.playback_mode ~= applied.mode) then fail("changed", label .. " changed in Renoise since Kumi rendered it") end
    song:describe_undo(desc)
    phrase:clear()
    write_lines(phrase, material.lines)
    pcall(write_settings, phrase, material.settings)
    return { restored = { done = true }, display = { title = label .. "'s lines put back as they were before the render" }, undo = { desc = desc, entry = true } }
  end,
})

return M
