-- M4 (n-song): Kumi's changes to a pattern itself (its name and number of lines) and to a pattern's track (its slot colour in the
-- pattern matrix). A pattern is named by its ref (`E:pattern:p`) or by a sequence slot's (`E:seq:s`, the pattern it plays); a
-- pattern's track by the pattern and a track ref, kept as `E:ptrack:p.t`. Records hold them as refs: a pattern pool that changed
-- since (a pattern made or deleted) makes them `stale`, and nothing is written.
--
-- Measured in Renoise 3.5.4 (lab, 2026-10-19): the name, the number of lines and the slot colour are undo steps; a pattern
-- made shorter keeps the lines past its end (they show again when it is made longer), and its automation points too.

local import = ...
local json = import("json")
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")

local bad, fail, the_song = base.bad, base.fail, base.the_song
local q = base.q
local M = {}

function M.pattern_label(song, p)
  local name = song:pattern(p).name
  if name == nil or name == "" then name = string.format("Pattern %02X", p - 1) end
  return name
end

-- The pattern a ref names: a pattern's, or a sequence slot's (the pattern it plays).
function M.pattern_of(song, env, reference, what)
  if type(reference) ~= "string" then bad(what .. ": a pattern ref, or the ref of a sequence slot that plays it") end
  local parsed = refs.parse(reference)
  if parsed and parsed.kind == "seq" and #parsed.n == 1 then
    local s = refs.expect(reference, "seq", 1, env.epoch())[1]
    local sequence = song.sequencer.pattern_sequence
    if s > #sequence then fail("gone", "that sequence slot isn't in the song any more") end
    return sequence[s]
  end
  local p = refs.expect(reference, "pattern", 1, env.epoch())[1]
  if p > #song.patterns then fail("gone", "that pattern isn't in the song any more") end
  return p
end

local function pattern_target(env, p) return { ref = refs.make(env.epoch(), "pattern", p), kind = "pattern" } end

local function pattern_reread(song, place)
  if place.pattern < 1 or place.pattern > #song.patterns then return nil end
  return song:pattern(place.pattern)
end

-- pattern-name

base.register({
  kind = "pattern-name", noun = "name", undoable = true,
  locate = function(song, env, input)
    local p = M.pattern_of(song, env, input.ref, "pattern-name")
    return { object = song:pattern(p), name = M.pattern_label(song, p), place = { pattern = p }, target = pattern_target(env, p) }
  end,
  reread = pattern_reread,
  read = function(pattern) return { name = pattern.name } end,
  wanted = function(_, input) return { name = base.text_of(input.name, "name", 256) } end,
  write = function(pattern, state) pattern.name = state.name end,
  same = function(a, b) return a.name == b.name end,
  title = function(_, from, to)
    if to.name == "" then return "Pattern " .. q(from.name) .. " unnamed" end
    if from.name == "" then return "Named the pattern " .. q(to.name) end
    return "Renamed pattern " .. q(from.name) .. " \226\134\146 " .. q(to.name)
  end,
})

-- slot-color: the colour of a pattern's track in the pattern matrix (every slot that plays the pattern shows it).

-- A pattern's track: a ptrack ref, or a pattern (or slot) ref and a track ref.
function M.pattern_track_of(song, env, input, what)
  local parsed = type(input.ref) == "string" and refs.parse(input.ref) or nil
  local p, t
  if parsed and parsed.kind == "ptrack" then
    local numbers = refs.expect(input.ref, "ptrack", 2, env.epoch())
    p, t = numbers[1], numbers[2]
    if p > #song.patterns then fail("gone", "that pattern isn't in the song any more") end
    base.track_at(song, t)
  else
    p = M.pattern_of(song, env, input.ref, what)
    t = base.find(song, env, input.track, what)
  end
  return p, t, song:track(t)
end

local function color_text(color) return color and color or "none" end

base.register({
  kind = "slot-color", noun = "slot colour", undoable = true,
  locate = function(song, env, input)
    local p, t, track = M.pattern_track_of(song, env, input, "slot-color")
    return {
      object = song:pattern(p):track(t), name = string.format("%s on %s", q(M.pattern_label(song, p)), q(base.named(t, track))),
      place = { pattern = p, track = t }, target = { ref = refs.make(env.epoch(), "ptrack", p, t), kind = "pattern-track" },
      id = base.track_id(track),
    }
  end,
  reread = function(song, place)
    if place.pattern > #song.patterns or place.track > rows.track_count(song) then return nil end
    return song:pattern(place.pattern):track(place.track)
  end,
  read = function(pattern_track) return { color = base.hex_of(pattern_track.color) or false } end,
  wanted = function(_, input)
    if input.color == json.null or input.color == false then return { color = false } end
    local color = base.rgb_of(input.color)
    if not color then bad("color must be #rrggbb, or null for none") end
    return { color = base.hex_of(color) }
  end,
  write = function(pattern_track, state)
    if state.color then pattern_track.color = base.rgb_of(state.color) else pattern_track.color = nil end
  end,
  same = function(a, b) return a.color == b.color end,
  title = function(name, _, to)
    if not to.color then return "Slot colour of " .. name .. " cleared" end
    return "Slot colour of " .. name .. " " .. color_text(to.color)
  end,
  watch = function(env, loc)
    if env.watch_pattern_track then env.watch_pattern_track(loc.place.pattern, loc.place.track, "color_observable") end
  end,
})

-- pattern-length: lines past a shorter pattern's end are kept by Renoise, hidden; a longer pattern shows what was kept there.
-- Kumi's undo puts the length back only while those lines (the ones between the two lengths, in every track, with their
-- automation points) are as the change left them: the producer's lines written into a pattern Kumi made longer would go out of
-- sight.

-- The lines from `from` to `to` of every track of the pattern, and the automation points in them, as a short fingerprint.
local function region_print(song, p, from, to)
  if from > to then return "none" end
  local pattern = song:pattern(p)
  local a, b = 5381, 0
  local function add(text)
    for i = 1, #text do
      local byte = text:byte(i)
      a = (a * 33 + byte) % 4294967296
      b = (b * 65599 + byte) % 4294967296
    end
  end
  for t = 1, rows.track_count(song) do
    local pattern_track = pattern:track(t)
    if not pattern_track.is_alias then
      for l = from, to do
        local line = pattern_track:line(l)
        if not line.is_empty then
          add(t .. ":" .. l .. "=")
          for c = 1, 12 do
            local column = line:note_column(c)
            if not column.is_empty then
              add(string.format("n%d %d %d %d %d %d %d %d;", c, column.note_value, column.instrument_value, column.volume_value,
                column.panning_value, column.delay_value, column.effect_number_value, column.effect_amount_value))
            end
          end
          for c = 1, 8 do
            local column = line:effect_column(c)
            if not column.is_empty then add(string.format("e%d %d %d;", c, column.number_value, column.amount_value)) end
          end
        end
      end
      for k, automation in ipairs(pattern_track.automation) do
        for _, point in ipairs(automation.points) do
          if point.time >= from and point.time < to + 1 then add(string.format("a%d:%d %.9g %.9g %.9g;", t, k, point.time, point.value, point.scaling)) end
        end
      end
    end
  end
  return string.format("%08x%08x", a, b)
end
M.region_print = region_print

-- (renoise.Pattern.MAX_NUMBER_OF_LINES)
local MAX_LINES = 512

local function length_input(input)
  local n = input.lines
  if type(n) ~= "number" or n ~= math.floor(n) or n < 1 or n > MAX_LINES then
    bad("lines must be a whole number from 1 to 512")
  end
  return n
end

local function length_title(label, lines) return string.format("%s %d lines", label, lines) end

local function length_note(song, p, old, new)
  if new < old then
    return string.format("Lines %d to %d are kept, hidden: making it longer again shows them.", new + 1, old)
  end
  if new > old and region_print(song, p, old + 1, new) ~= "none" then
    return string.format("Lines %d to %d hold what was kept there, hidden: it shows again.", old + 1, new)
  end
  return nil
end

local function length_locate(song, env, reference)
  local p = M.pattern_of(song, env, reference, "pattern-length")
  return p, song:pattern(p), M.pattern_label(song, p)
end

local function length_preview(song, env, input)
  local p, pattern, label = length_locate(song, env, input.ref)
  local old, new = pattern.number_of_lines, length_input(input)
  local display = { title = length_title(label, new), before = { lines = old }, after = { lines = new }, note = length_note(song, p, old, new) }
  if old == new then display.unchanged = true end
  local low, high = math.min(old, new), math.max(old, new)
  return {
    target = pattern_target(env, p), prior = { lines = old },
    fence = { lines = old, region = region_print(song, p, low + 1, high), name = pattern.name },
    display = display,
  }
end

local function length_apply(song, env, input, fence, desc)
  local ok, p, pattern, label = pcall(length_locate, song, env, input.ref)
  if not ok then base.gone_is_changed(p) end
  local old, new = pattern.number_of_lines, length_input(input)
  local low, high = math.min(old, new), math.max(old, new)
  if old ~= fence.lines or pattern.name ~= fence.name or region_print(song, p, low + 1, high) ~= fence.region then
    fail("changed", label .. " changed since the preview")
  end
  local display = { title = length_title(label, new), before = { lines = old }, after = { lines = new }, note = length_note(song, p, old, new) }
  if old == new then
    display.unchanged = true
    return { applied = { lines = new }, display = display, undo = { desc = desc, entry = false }, prior = { lines = old } }
  end
  song:describe_undo(desc)
  pattern.number_of_lines = new
  if pattern.number_of_lines ~= new then fail("failed", "Renoise kept " .. label .. " at " .. pattern.number_of_lines .. " lines") end
  return {
    applied = { lines = new, region = fence.region, from = low + 1, to = high }, display = display, undo = { desc = desc, entry = true },
    prior = { lines = old }, place = { pattern = p }, target = pattern_target(env, p),
  }
end

local function length_restore(song, env, restore, desc)
  local prior, applied = restore.prior, restore.applied
  if type(prior) ~= "table" or type(applied) ~= "table" or type(prior.lines) ~= "number" or type(applied.lines) ~= "number" then
    bad("prior and applied: the pattern's lines before and after")
  end
  local ok, p, pattern, label = pcall(length_locate, song, env, restore.target and restore.target.ref)
  if not ok then
    if type(p) == "table" and p.code == "failed" then p.code = "gone" end
    error(p, 0)
  end
  local now = pattern.number_of_lines
  local title = length_title(label, prior.lines) .. " (put back)"
  if now == prior.lines then
    return { restored = { lines = now }, display = { title = title }, undo = { desc = desc, entry = false }, unwritten = true }
  end
  if now ~= applied.lines then fail("changed", label .. "'s length changed in Renoise since Kumi set it") end
  if not restore.discard and type(applied.from) == "number" and region_print(song, p, applied.from, applied.to) ~= applied.region then
    fail("changed", string.format("lines %d to %d of %s changed in Renoise since Kumi set its length: putting it back would hide them",
      applied.from, applied.to, label))
  end
  song:describe_undo(desc)
  pattern.number_of_lines = prior.lines
  base.note_restored(env.journal, "pattern-length", { pattern = p }, applied, base.KINDS["pattern-length"])
  return { restored = { lines = prior.lines }, display = { title = title }, undo = { desc = desc, entry = true } }
end

base.register({
  kind = "pattern-length", custom = true,
  preview = length_preview, apply = length_apply, restore = length_restore,
  reread = pattern_reread,
  read = function(pattern) return { lines = pattern.number_of_lines } end,
  same = function(a, b) return a.lines == b.lines end,
})

return M
