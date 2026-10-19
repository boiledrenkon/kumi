-- Kumi's changes to the lines of a pattern: the note and effect columns of one track in one pattern (notes-write, notes-clear).
-- Cells are written and read as Renoise's own text ("C-4", "01", "40", ".."), cell by cell, so a restore puts back exactly the
-- cells Kumi changed and leaves the rest of the pattern to the producer.

local import = ...
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")

local bad, fail, the_song = base.bad, base.fail, base.the_song
local q = base.q
local M = {}

-- The most writes one change makes: about a second of Renoise's time, the callback's budget (a whole 512-line pattern of 12 note
-- columns is 43,000: two changes).
M.MAX_WRITES = base.MAX_APPLY_WRITES
local NOTE_WRITES, EFFECT_WRITES = 7, 2

local NOTE_FIELDS = { "note", "instrument", "volume", "panning", "delay", "fx" }
local EMPTY_NOTE = { note = "---", instrument = "..", volume = "..", panning = "..", delay = "..", fx = "0000" }
local EMPTY_EFFECT = { number = "00", amount = "00" }

-- Text, as the tracker writes it

local function text_of(value, name, shape)
  if type(value) ~= "string" then bad(name .. ": text such as " .. shape) end
  return value:upper()
end

-- The delay column: 00 is no delay, FF is 255 (the latest a note can be delayed), unlike the instrument's FF (none).
local function delay_text(value)
  if value == "" or value == ".." then return ".." end
  if type(value) == "number" then
    if value < 0 or value > 255 or value ~= math.floor(value) then bad("delay: a whole number from 0 to 255, or two hex digits") end
    return value == 0 and ".." or string.format("%02X", value)
  end
  local text = text_of(value, "delay", "10 or ..")
  if not text:match("^%x%x$") then bad("delay: two hex digits (00 to FF), or .. for none; got " .. value) end
  return text == "00" and ".." or text
end

local function hex2(value, name, empty)
  if value == "" or value == ".." then return empty end
  if type(value) == "number" then
    if value < 0 or value > 254 or value ~= math.floor(value) then bad(name .. ": a whole number from 0 to 254, or two hex digits") end
    return string.format("%02X", value)
  end
  local text = text_of(value, name, "01 or ..")
  if not text:match("^%x%x$") then bad(name .. ": two hex digits (00 to FE), or .. for none; got " .. value) end
  if text == "FF" and empty == ".." then return ".." end -- (255 is Renoise's "none" for these)
  return text
end

local function note_text(value)
  local text = text_of(value, "note", "C-4, C#5 or OFF")
  if text == "" or text == "---" then return "---" end
  if text == "OFF" then return "OFF" end
  local letter, sep, octave = text:match("^([A-G])([-#]?)(%d)$")
  if not letter then bad("note: C-4, C#5, OFF or --- (a note name, # or - and an octave 0 to 9); got " .. value) end
  if sep == "" then sep = "-" end
  if sep == "#" and (letter == "E" or letter == "B") then bad("note: there is no " .. text .. " (E and B have no sharp)") end
  return letter .. sep .. octave
end

-- The volume and panning columns take a level (00 to 80) or a command (a letter and a hex digit, such as G3); FF is none.
local function column_text(value, name)
  if value == "" or value == ".." then return ".." end
  local text = text_of(value, name, "40 or ..")
  if not text:match("^%w%x$") then bad(name .. ": two characters such as 40 or G3 (the second a hex digit), or .. for none; got " .. value) end
  if text == "FF" then return ".." end
  return text
end

local function fx_text(value)
  if value == "" then return "0000" end
  local text = text_of(value, "fx", "0A20")
  if not text:match("^%w%w%x%x$") then bad("fx: an effect number and amount such as 0A20, or 0000 for none; got " .. value) end
  return text
end

-- Cells

local function read_note(column)
  return {
    note = column.note_string, instrument = column.instrument_string, volume = column.volume_string,
    panning = column.panning_string, delay = column.delay_string, fx = column.effect_number_string .. column.effect_amount_string,
  }
end

local function read_effect(column)
  return { number = column.number_string, amount = column.amount_string }
end

-- A cell by its place {line, kind = "note" | "effect", col}, as its text with the place in it.
local function read_cell(pattern_track, at)
  local line = pattern_track:line(at.line)
  local state = at.kind == "note" and read_note(line:note_column(at.col)) or read_effect(line:effect_column(at.col))
  state.line, state.kind, state.col = at.line, at.kind, at.col
  return state
end

local function same_cell(a, b)
  if a.kind == "note" then
    for _, field in ipairs(NOTE_FIELDS) do
      if a[field] ~= b[field] then return false end
    end
    return true
  end
  return a.number == b.number and a.amount == b.amount
end

local function is_empty_cell(cell)
  return same_cell(cell, cell.kind == "note" and { kind = "note", note = "---", instrument = "..", volume = "..", panning = "..", delay = "..", fx = "0000" }
    or { kind = "effect", number = "00", amount = "00" })
end

local function same_cells(a, b)
  if type(a) ~= "table" or type(b) ~= "table" or #a ~= #b then return false end
  for i = 1, #a do
    if a[i].line ~= b[i].line or a[i].kind ~= b[i].kind or a[i].col ~= b[i].col or not same_cell(a[i], b[i]) then return false end
  end
  return true
end

local function write_cell(pattern_track, cell)
  local line = pattern_track:line(cell.line)
  if cell.kind == "note" then
    local column = line:note_column(cell.col)
    column:clear()
    if cell.note ~= "---" then column.note_string = cell.note end
    if cell.instrument ~= ".." then column.instrument_string = cell.instrument end
    if cell.volume ~= ".." then column.volume_string = cell.volume end
    if cell.panning ~= ".." then column.panning_string = cell.panning end
    if cell.delay ~= ".." then column.delay_string = cell.delay end
    if cell.fx:sub(1, 2) ~= "00" then column.effect_number_string = cell.fx:sub(1, 2) end
    if cell.fx:sub(3, 4) ~= "00" then column.effect_amount_string = cell.fx:sub(3, 4) end
  else
    local column = line:effect_column(cell.col)
    column:clear()
    if cell.number ~= "00" then column.number_string = cell.number end
    if cell.amount ~= "00" then column.amount_string = cell.amount end
  end
end

local function writes_of(cells)
  local total = 0
  for _, cell in ipairs(cells) do total = total + (cell.kind == "note" and NOTE_WRITES or EFFECT_WRITES) end
  return total
end

-- A short fingerprint of cells that two reads of cells nobody touched agree on.
local function fingerprint(cells)
  local a, b = 5381, 0
  for _, cell in ipairs(cells) do
    local text
    if cell.kind == "note" then
      text = table.concat({ cell.line, "n", cell.col, cell.note, cell.instrument, cell.volume, cell.panning, cell.delay, cell.fx }, "|")
    else
      text = table.concat({ cell.line, "e", cell.col, cell.number, cell.amount }, "|")
    end
    for i = 1, #text do
      local byte = text:byte(i)
      a = (a * 33 + byte) % 4294967296
      b = (b * 65599 + byte) % 4294967296
    end
  end
  return string.format("%08x%08x", a, b)
end

local function cell_order(a, b)
  if a.line ~= b.line then return a.line < b.line end
  if a.kind ~= b.kind then return a.kind == "note" end
  return a.col < b.col
end

-- The columns a track shows

local function read_columns(track)
  local columns = { effect = track.visible_effect_columns }
  if track.max_note_columns > 0 then
    columns.note = track.visible_note_columns
    columns.volume, columns.panning = track.volume_column_visible, track.panning_column_visible
    columns.delay, columns.sampleEffects = track.delay_column_visible, track.sample_effects_column_visible
  end
  return columns
end

-- What the written cells need to be seen: the columns up to the highest used and the sub-columns with something in them.
local function needs_of(cells)
  local needs = { note = 0, effect = 0 }
  for _, cell in ipairs(cells) do
    if cell.kind == "note" then
      if not is_empty_cell(cell) then
        needs.note = math.max(needs.note, cell.col)
        if cell.volume ~= ".." then needs.volume = true end
        if cell.panning ~= ".." then needs.panning = true end
        if cell.delay ~= ".." then needs.delay = true end
        if cell.fx ~= "0000" then needs.sampleEffects = true end
      end
    elseif not is_empty_cell(cell) then
      needs.effect = math.max(needs.effect, cell.col)
    end
  end
  return needs
end

local function raise_columns(track, needs)
  if needs.note > 0 and track.visible_note_columns < needs.note then track.visible_note_columns = needs.note end
  if needs.effect > 0 and track.visible_effect_columns < needs.effect then track.visible_effect_columns = needs.effect end
  if needs.volume and not track.volume_column_visible then track.volume_column_visible = true end
  if needs.panning and not track.panning_column_visible then track.panning_column_visible = true end
  if needs.delay and not track.delay_column_visible then track.delay_column_visible = true end
  if needs.sampleEffects and not track.sample_effects_column_visible then track.sample_effects_column_visible = true end
end

local function set_columns(track, columns)
  pcall(function()
    if columns.note and track.visible_note_columns ~= columns.note then track.visible_note_columns = columns.note end
    if columns.effect and track.visible_effect_columns ~= columns.effect then track.visible_effect_columns = columns.effect end
    if columns.volume ~= nil then track.volume_column_visible = columns.volume end
    if columns.panning ~= nil then track.panning_column_visible = columns.panning end
    if columns.delay ~= nil then track.delay_column_visible = columns.delay end
    if columns.sampleEffects ~= nil then track.sample_effects_column_visible = columns.sampleEffects end
  end)
end

local READ_BUDGET = 40000

-- What is in the track's columns above the ones Kumi found shown, in every pattern: nil when that is too much to look through.
local function in_use(song, index, prior)
  local used, reads = { note = 0, effect = 0 }, 0
  for p = 1, #song.patterns do
    local pattern = song:pattern(p)
    local pattern_track = pattern:track(index)
    if not pattern_track.is_alias and not pattern_track.is_empty then
      for l = 1, pattern.number_of_lines do
        local line = pattern_track:line(l)
        reads = reads + 1
        if not line.is_empty then
          for c = 1, 12 do
            local column = line:note_column(c)
            reads = reads + 5
            if not column.is_empty then
              if c > used.note then used.note = c end
              if column.volume_string ~= ".." then used.volume = true end
              if column.panning_string ~= ".." then used.panning = true end
              if column.delay_string ~= ".." then used.delay = true end
              if column.effect_number_value ~= 0 or column.effect_amount_value ~= 0 then used.sampleEffects = true end
            end
          end
          for c = 1, 8 do
            reads = reads + 1
            if not line:effect_column(c).is_empty and c > used.effect then used.effect = c end
          end
          if reads > READ_BUDGET then return nil end
        end
      end
    end
  end
  return used
end

-- The columns Kumi showed are hidden again when they are still as Kumi left them and nothing is in them.
local function lower_columns(song, index, track, prior, applied)
  if type(prior) ~= "table" or type(applied) ~= "table" then return end
  local now = read_columns(track)
  local used = in_use(song, index, prior)
  if not used then return end
  local back = {}
  if prior.note and now.note == applied.note and applied.note > prior.note then back.note = math.max(prior.note, used.note) end
  if prior.effect and now.effect == applied.effect and applied.effect > prior.effect then back.effect = math.max(prior.effect, used.effect) end
  for _, flag in ipairs({ "volume", "panning", "delay", "sampleEffects" }) do
    if prior[flag] == false and applied[flag] == true and now[flag] == true and not used[flag] then back[flag] = false end
  end
  if next(back) then set_columns(track, back) end
end

-- What a request names

local function pattern_of(song, env, reference)
  if type(reference) ~= "string" then bad("pattern: a pattern ref, or the ref of a sequence slot") end
  local parsed = refs.parse(reference)
  if not parsed then bad("not a ref: " .. reference) end
  if parsed.kind == "seq" and #parsed.n == 1 then
    local slot = refs.expect(reference, "seq", 1, env.epoch())[1]
    local sequence = song.sequencer.pattern_sequence
    if slot > #sequence then fail("gone", "that sequence slot isn't in the song any more") end
    return sequence[slot]
  end
  local p = refs.expect(reference, "pattern", 1, env.epoch())[1]
  if p > #song.patterns then fail("gone", "that pattern isn't in the song any more") end
  return p
end

local function pattern_label(pattern, p)
  local name = pattern.name
  if name == nil or name == "" then name = string.format("Pattern %02X", p - 1) end
  return name
end

local function pattern_track_of(song, p, index)
  local pattern_track = song:pattern(p):track(index)
  if pattern_track.is_alias then
    bad(string.format("that track in pattern %d is an alias of pattern %d: its lines are that pattern's, so write them there", p, pattern_track.alias_pattern_index))
  end
  return pattern_track
end

local function target_of(env, p, index)
  return { ref = refs.make(env.epoch(), "ptrack", p, index), kind = "pattern-track" }
end

-- Where a request writes: the pattern, the track and the pattern's track.
local function locate(song, env, input, kind)
  local p = pattern_of(song, env, input.pattern)
  local index, track = base.find(song, env, input.track, kind)
  return p, index, track, pattern_track_of(song, p, index), song:pattern(p)
end

local function check_line(pattern, line)
  if type(line) ~= "number" or line ~= math.floor(line) or line < 1 or line > pattern.number_of_lines then
    bad(string.format("line must be a whole number from 1 to %d (this pattern's lines)", pattern.number_of_lines))
  end
end

local function cell_at(pattern, track, line, kind, col)
  check_line(pattern, line)
  if type(col) ~= "number" or col ~= math.floor(col) then bad("col must be a whole number") end
  if kind == "note" then
    if track.max_note_columns == 0 then bad("that track has no note columns: only sequencer tracks do") end
    if col < 1 or col > track.max_note_columns then bad(string.format("note col must be from 1 to %d", track.max_note_columns)) end
  else
    if col < 1 or col > track.max_effect_columns then bad(string.format("effect col must be from 1 to %d", track.max_effect_columns)) end
  end
  return { line = line, kind = kind, col = col }
end

-- The cells a request names, with what it gives for each (`given`) and whether to clear first: [key] = want.
local function key_of(at) return at.line * 100 + (at.kind == "note" and 0 or 50) + at.col end

local function parse_cells(pattern, track, input)
  local wants, count = {}, 0
  local function add(at, given, clear)
    local key = key_of(at)
    if wants[key] then bad(string.format("line %d %s column %d is given twice", at.line, at.kind, at.col)) end
    wants[key] = { at = at, given = given, clear = clear }
    count = count + 1
  end
  if type(input.lines) ~= "table" or #input.lines == 0 then bad("lines: a list of {line, notes?, effects?, clear?}") end
  local seen = {}
  for _, entry in ipairs(input.lines) do
    if type(entry) ~= "table" then bad("each of lines is an object {line, notes?, effects?, clear?}") end
    check_line(pattern, entry.line)
    if seen[entry.line] then bad("line " .. tostring(entry.line) .. " is given twice: put its cells in one entry") end
    seen[entry.line] = true
    local any = false
    for _, cell in ipairs(entry.notes or {}) do
      if type(cell) ~= "table" then bad("each of notes is an object {col, note?, instrument?, volume?, panning?, delay?, fx?, clear?}") end
      local at = cell_at(pattern, track, entry.line, "note", cell.col)
      local given = {}
      if cell.note ~= nil then given.note = note_text(cell.note) end
      if cell.instrument ~= nil then given.instrument = hex2(cell.instrument, "instrument", "..") end
      if cell.volume ~= nil then given.volume = column_text(cell.volume, "volume") end
      if cell.panning ~= nil then given.panning = column_text(cell.panning, "panning") end
      if cell.delay ~= nil then given.delay = delay_text(cell.delay) end
      if cell.fx ~= nil then given.fx = fx_text(cell.fx) end
      if next(given) == nil and cell.clear ~= true then bad(string.format("line %d note column %d says nothing to write: give a field, or clear: true", at.line, at.col)) end
      add(at, given, cell.clear == true)
      any = true
    end
    for _, cell in ipairs(entry.effects or {}) do
      if type(cell) ~= "table" then bad("each of effects is an object {col, number?, amount?, clear?}") end
      local at = cell_at(pattern, track, entry.line, "effect", cell.col)
      local given = {}
      if cell.number ~= nil then
        given.number = text_of(cell.number, "number", "0A")
        if not given.number:match("^%w%w$") then bad("number: two characters such as 0A; got " .. cell.number) end
      end
      if cell.amount ~= nil then given.amount = hex2(cell.amount, "amount", "00") end
      if next(given) == nil and cell.clear ~= true then bad(string.format("line %d effect column %d says nothing to write: give a field, or clear: true", at.line, at.col)) end
      add(at, given, cell.clear == true)
      any = true
    end
    if entry.clear ~= true and not any then
      bad("line " .. tostring(entry.line) .. " says nothing to write: give notes or effects, or clear: true")
    end
  end
  return wants, count
end

-- The cells a clear of whole lines takes: every cell with something in it, in all columns (hidden ones too).
local function add_cleared(pattern_track, input, wants)
  for _, entry in ipairs(input.lines) do
    if entry.clear == true then
      local line = pattern_track:line(entry.line)
      if not line.is_empty then
        for c = 1, 12 do
          local at = { line = entry.line, kind = "note", col = c }
          if not wants[key_of(at)] and not line:note_column(c).is_empty then
            wants[key_of(at)] = { at = at, given = {}, clear = true }
          end
        end
        for c = 1, 8 do
          local at = { line = entry.line, kind = "effect", col = c }
          if not wants[key_of(at)] and not line:effect_column(c).is_empty then
            wants[key_of(at)] = { at = at, given = {}, clear = true }
          end
        end
      end
    end
  end
end

-- The plan: for each named cell, what it holds now and what it will hold, in order; only the cells that differ are `changes`.
local function plan_of(pattern_track, wants, merge)
  local keys = {}
  for key in pairs(wants) do keys[#keys + 1] = key end
  table.sort(keys)
  local prior, after = {}, {}
  for _, key in ipairs(keys) do
    local want = wants[key]
    local now = read_cell(pattern_track, want.at)
    local target = { line = want.at.line, kind = want.at.kind, col = want.at.col }
    local base_state
    if want.at.kind == "note" then
      base_state = (merge and not want.clear) and now or EMPTY_NOTE
      for _, field in ipairs(NOTE_FIELDS) do target[field] = want.given[field] or base_state[field] end
    else
      base_state = (merge and not want.clear) and now or EMPTY_EFFECT
      target.number, target.amount = want.given.number or base_state.number, want.given.amount or base_state.amount
    end
    if not same_cell(now, target) then
      prior[#prior + 1] = now
      after[#after + 1] = target
    end
  end
  return prior, after
end

local function counts_of(cells)
  local notes, effects = 0, 0
  for _, cell in ipairs(cells) do
    if not is_empty_cell(cell) then
      if cell.kind == "note" then notes = notes + 1 else effects = effects + 1 end
    end
  end
  return { notes = notes, effects = effects }
end

local function lines_in(cells)
  local seen, n = {}, 0
  for _, cell in ipairs(cells) do
    if not seen[cell.line] then
      seen[cell.line] = true
      n = n + 1
    end
  end
  return n
end

local function lines_text(n) return n == 1 and "1 line" or n .. " lines" end

local function title_of(verb, cells, pattern, p, track, index)
  return string.format("%s %s in %s on %s", verb, lines_text(lines_in(cells)), q(pattern_label(pattern, p)), q(base.named(index, track)))
end

local function preview_of(song, env, input, kind, verb)
  local p, index, track, pattern_track, pattern = locate(song, env, input, kind)
  local wants, count = parse_cells(pattern, track, input)
  add_cleared(pattern_track, input, wants)
  local prior, after = plan_of(pattern_track, wants, input.merge == true)
  if writes_of(after) > M.MAX_WRITES then
    bad(string.format("that is %d writes, more than one change takes (%d): split it into several", writes_of(after), M.MAX_WRITES))
  end
  local columns = read_columns(track)
  local display = {
    title = title_of(verb, #after > 0 and after or prior, pattern, p, track, index),
    before = counts_of(prior), after = counts_of(after),
  }
  if #after == 0 then display.unchanged = true end
  local needs = needs_of(after)
  if needs.note > (columns.note or 0) then display.note = "It shows note column " .. needs.note .. " first." end
  return {
    target = target_of(env, p, index), prior = { cells = prior, columns = columns },
    fence = { cells = #prior, hash = fingerprint(prior), id = base.track_id(track), label = base.named(index, track) },
    display = display,
  }, { p = p, index = index, track = track, pattern_track = pattern_track, pattern = pattern, prior = prior, after = after, columns = columns }
end

local function apply_of(song, env, input, fence, desc, kind, verb)
  local ok, preview, plan = pcall(preview_of, song, env, input, kind, verb)
  if not ok then base.gone_is_changed(preview) end
  if fence.id ~= nil and fence.id ~= preview.fence.id then
    fail("changed", base.moved_text(fence.label or base.named(plan.index, plan.track)))
  end
  if preview.fence.cells ~= fence.cells or preview.fence.hash ~= fence.hash then
    fail("changed", string.format("%s in %s changed since the preview", base.named(plan.index, plan.track), pattern_label(plan.pattern, plan.p)))
  end
  local display = preview.display
  if #plan.after == 0 then
    return {
      applied = { cells = {}, columns = plan.columns }, display = display, undo = { desc = desc, entry = false }, prior = preview.prior,
    }
  end
  song:describe_undo(desc)
  local written = 0
  local wrote, err = pcall(function()
    raise_columns(plan.track, needs_of(plan.after))
    for i, cell in ipairs(plan.after) do
      write_cell(plan.pattern_track, cell)
      written = i
    end
  end)
  if not wrote then
    -- Nothing half-done stays: the cells written go back to what they held, and the columns.
    pcall(function()
      for i = 1, written + 1 do
        if plan.prior[i] then write_cell(plan.pattern_track, plan.prior[i]) end
      end
    end)
    set_columns(plan.track, plan.columns)
    local message = type(err) == "table" and err.message or tostring(err):gsub("^[^%s:]*%.lua:%d+: ", "")
    fail("failed", "Renoise wouldn't take those cells: " .. message)
  end
  local result = {}
  for i, cell in ipairs(plan.after) do result[i] = read_cell(plan.pattern_track, cell) end
  for i, cell in ipairs(result) do
    if not same_cell(cell, plan.after[i]) then
      -- (Renoise read the text as something else: nothing half-done stays)
      for j = 1, #plan.after do write_cell(plan.pattern_track, plan.prior[j]) end
      set_columns(plan.track, plan.columns)
      fail("failed", string.format("Renoise didn't keep line %d column %d as written", cell.line, cell.col))
    end
  end
  return {
    applied = { cells = result, columns = read_columns(plan.track) },
    display = { title = display.title, before = display.before, after = counts_of(result), note = display.note },
    undo = { desc = desc, entry = true },
    prior = preview.prior, place = { pattern = plan.p, track = plan.index }, target = target_of(env, plan.p, plan.index),
  }
end

-- Restore

local function restore_plan(pattern_track, restore)
  local prior, applied = restore.prior and restore.prior.cells, restore.applied and restore.applied.cells
  if type(prior) ~= "table" or type(applied) ~= "table" or #prior ~= #applied then bad("prior and applied: the cells the change wrote") end
  local todo = {}
  for i = 1, #prior do
    local now = read_cell(pattern_track, prior[i])
    if not same_cell(now, prior[i]) then
      if not restore.discard and not same_cell(now, applied[i]) then
        fail("changed", string.format("line %d was changed in Renoise since Kumi wrote it", prior[i].line))
      end
      todo[#todo + 1] = prior[i]
    end
  end
  return todo
end

local function locate_restored(song, env, restore)
  local reference = restore.target and restore.target.ref
  local numbers = refs.expect(reference, "ptrack", 2, env.epoch())
  local p, index = numbers[1], numbers[2]
  if p > #song.patterns then fail("gone", "that pattern isn't in the song any more") end
  local track = base.track_at(song, index)
  local pattern_track = song:pattern(p):track(index)
  if pattern_track.is_alias then fail("changed", "that track in the pattern is an alias now") end
  return p, index, track, pattern_track, song:pattern(p)
end

local function chunks_of(todo)
  local chunks, current, writes = {}, {}, 0
  for _, cell in ipairs(todo) do
    local cost = cell.kind == "note" and NOTE_WRITES or EFFECT_WRITES
    if writes > 0 and writes + cost > base.SLICE_WRITES then
      chunks[#chunks + 1] = current
      current, writes = {}, 0
    end
    current[#current + 1] = cell
    writes = writes + cost
  end
  chunks[#chunks + 1] = current
  return chunks
end

local function restored_answer(env, display, desc, steps)
  local answer = { restored = { done = true }, display = display, undo = { desc = desc, entry = true } }
  if steps then answer.undo.steps = steps end
  return answer
end

local function restore_of(spec_kind, noun)
  return function(song, env, restore, desc)
    local p, index, track, pattern_track, pattern = locate_restored(song, env, restore)
    local todo = restore_plan(pattern_track, restore)
    local label = string.format("%s on %s", q(pattern_label(pattern, p)), q(base.named(index, track)))
    local place = { pattern = p, track = index }
    local spec = base.KINDS[spec_kind]
    if #todo == 0 then
      lower_columns(song, index, track, restore.prior.columns, restore.applied.columns)
      base.note_restored(env.journal, spec_kind, place, restore.applied, spec)
      return { restored = { done = true }, display = { title = noun .. " in " .. label .. " is back already" }, undo = { desc = desc, entry = false } }
    end
    local chunks = chunks_of(todo)
    local display = { title = string.format("%s put back in %s", noun, label), before = counts_of(todo), after = counts_of(restore.prior.cells) }
    local function finish(live_song, live_track)
      lower_columns(live_song, index, live_track, restore.prior.columns, restore.applied.columns)
      base.note_restored(env.journal, spec_kind, place, restore.applied, spec)
    end
    if #chunks == 1 then
      song:describe_undo(desc)
      for _, cell in ipairs(chunks[1]) do write_cell(pattern_track, cell) end
      finish(song, track)
      return restored_answer(env, display, desc)
    end
    -- Too much for one callback: numbered steps, each its own callback and undo step. The job stops when tracks, patterns or lines
    -- were inserted or removed between them (it holds a track's number, and the producer's change may have moved it): the steps done
    -- stay, and undoing again puts back the rest (cells already back are skipped).
    return {
      kumi_job = function(job)
        local total = #chunks
        local done_steps = 0
        local ok, err = pcall(function()
          for k = 1, total do
            if k > 1 then
              job.step(base.STEP_GAP_MS)
              base.wait_while_busy(job, env)
            end
            local live = the_song()
            local live_pattern_track = live:pattern(p):track(index)
            if live_pattern_track.is_alias then fail("changed", "that track in the pattern is an alias now") end
            live:describe_undo(string.format("%s (%d/%d)", desc, k, total))
            for _, cell in ipairs(chunks[k]) do write_cell(live_pattern_track, cell) end
            done_steps = k
            if k == 1 then job.rebase() end
          end
        end)
        if not ok then
          if type(err) == "table" and err.message then
            err = { code = err.code, message = string.format("%s; %d of %d steps are put back: undo again to put back the rest", err.message, done_steps, total) }
          end
          error(err, 0)
        end
        local live = the_song()
        finish(live, live:track(index))
        local answer = restored_answer(env, display, desc, total)
        answer.epoch, answer.layout = env.epoch(), env.layout()
        return answer
      end,
    }
  end
end

-- notes-write

local function same_state(a, b) return same_cells(a.cells, b.cells) end

local function register(kind, verb, noun)
  local spec = {
    kind = kind, custom = true,
    preview = function(song, env, input) return (preview_of(song, env, input, kind, verb)) end,
    apply = function(song, env, input, fence, desc) return apply_of(song, env, input, fence, desc, kind, verb) end,
    restore = restore_of(kind, noun),
    same = same_state,
    reread = function(song, place)
      if place.pattern > #song.patterns or place.track > rows.track_count(song) then return nil end
      local pattern_track = song:pattern(place.pattern):track(place.track)
      if pattern_track.is_alias then return nil end
      return pattern_track
    end,
    read = function(pattern_track, shape)
      local cells = {}
      for i, at in ipairs(shape.cells or {}) do cells[i] = read_cell(pattern_track, at) end
      return { cells = cells }
    end,
  }
  base.register(spec)
end

register("notes-write", "Wrote", "Lines")

-- notes-clear: the cells with something in them in a range of lines (all lines when none is given), in every column.
local function clear_input(song, env, input, kind)
  local p, _, _, pattern_track, pattern = locate(song, env, input, kind)
  local first, last = input.first or 1, input.last or pattern.number_of_lines
  for _, value in ipairs({ first, last }) do
    if type(value) ~= "number" or value ~= math.floor(value) or value < 1 or value > pattern.number_of_lines then
      bad(string.format("first and last are line numbers from 1 to %d", pattern.number_of_lines))
    end
  end
  if first > last then bad("first must not come after last") end
  local lines = {}
  for l = first, last do
    if not pattern_track:line(l).is_empty then lines[#lines + 1] = { line = l, clear = true } end
  end
  return { pattern = input.pattern, track = input.track, lines = lines, epoch = input.epoch }, p
end

local clear = {
  kind = "notes-clear", custom = true,
  same = same_state,
}
clear.reread = base.KINDS["notes-write"].reread
clear.read = base.KINDS["notes-write"].read
clear.preview = function(song, env, input)
  local built = clear_input(song, env, input, "notes-clear")
  if #built.lines == 0 then
    local p, index, track, _, pattern = locate(song, env, input, "notes-clear")
    return {
      target = target_of(env, p, index), prior = { cells = {}, columns = read_columns(track) }, fence = { cells = 0, hash = fingerprint({}) },
      display = { title = title_of("Cleared", {}, pattern, p, track, index), unchanged = true, before = { notes = 0, effects = 0 }, after = { notes = 0, effects = 0 } },
    }
  end
  return (preview_of(song, env, built, "notes-clear", "Cleared"))
end
clear.apply = function(song, env, input, fence, desc)
  local built = clear_input(song, env, input, "notes-clear")
  if #built.lines == 0 then
    local p, index, track, _, pattern = locate(song, env, input, "notes-clear")
    return {
      applied = { cells = {}, columns = read_columns(track) }, undo = { desc = desc, entry = false },
      display = { title = title_of("Cleared", {}, pattern, p, track, index), unchanged = true, before = { notes = 0, effects = 0 }, after = { notes = 0, effects = 0 } },
      prior = { cells = {}, columns = read_columns(track) },
    }
  end
  return apply_of(song, env, built, fence, desc, "notes-clear", "Cleared")
end
clear.restore = restore_of("notes-clear", "Cells")
base.register(clear)
return M
