-- M4 (n-song): Kumi's changes to the order of the pattern sequence: cloning slots (clone_range), moving a slot, making slots'
-- patterns unique (make_range_unique).
--
-- Measured in Renoise 3.5.4 (lab, 2026-10-19): `clone_range(a, b)` puts copies of slots a..b right after b, each with a NEW
-- pattern (a copy named "#" + the name, at the end of the pool), the slots' mutes and section starts copied (a section's name
-- with "#" in front); `make_range_unique(a, b)` gives every slot in the range whose pattern another slot plays a copy of it (the
-- first such slot too: [1, 2, 1] becomes [3, 2, 4]); a slot made in the middle takes the section start and mutes of the slot it
-- pushes up, and deleting a slot that starts a section passes the start to the next one (M3). A pattern is removed from the pool
-- when a slot lets go of it while it is the pool's last, empty and played by no other slot.
--
-- Slots have no identity, and Renoise's sequence notifier names the wrong slot when patterns repeat (measured), so these records
-- find their slots by the sequence they left (`applied.sequence`, pattern numbers read through the pool's log since: places.lua)
-- and base.follow_slot: a sequence changed since by one block of slots made or deleted elsewhere is followed; anything else is
-- `stale`, with nothing touched. Their target is the song (a sequence ref would go stale with any sequence change), and a restore
-- drops a record's slot refs (bridge.lua).

local import = ...
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")
local patterns_ = import("changes_patterns")

local bad, fail = base.bad, base.fail
local q = base.q
local M = {}

-- The kinds whose records find their slots this way (bridge.lua drops the slot refs of their restores).
M.KINDS = { ["slot-clone"] = true, ["slot-move"] = true, ["slot-unique"] = true }

local function slot_count(song) return #song.sequencer.pattern_sequence end

local function sequence_of(song)
  local out = {}
  for s, p in ipairs(song.sequencer.pattern_sequence) do out[s] = p end
  return out
end

local function same_list(a, b)
  if type(a) ~= "table" or type(b) ~= "table" or #a ~= #b then return false end
  for i = 1, #a do
    if a[i] ~= b[i] then return false end
  end
  return true
end

local function seq_of(song, env, reference, what)
  if type(reference) ~= "string" then bad(what .. ": a sequence slot ref") end
  local s = refs.expect(reference, "seq", 1, env.epoch())[1]
  if s < 1 or s > slot_count(song) then fail("gone", "that sequence slot isn't in the song any more") end
  return s
end

local function song_target(env) return { ref = refs.make(env.epoch(), "song", 1), kind = "song" } end

-- A slot's own state: its section start and name, and the tracks muted in it.
local function state_of(song, s)
  local sequencer = song.sequencer
  local start = sequencer:sequence_is_start_of_section(s)
  local mutes = {}
  for t = 1, rows.track_count(song) do
    if sequencer:track_sequence_slot_is_muted(t, s) then mutes[#mutes + 1] = t end
  end
  return { start = start, name = start and sequencer:sequence_section_name(s) or "", mutes = mutes }
end

local function set_state(song, s, state, sections_only)
  local sequencer = song.sequencer
  if sequencer:sequence_is_start_of_section(s) ~= state.start then sequencer:set_sequence_is_start_of_section(s, state.start) end
  if state.start and sequencer:sequence_section_name(s) ~= state.name then sequencer:set_sequence_section_name(s, state.name) end
  if sections_only then return end
  local muted = {}
  for _, t in ipairs(state.mutes) do muted[t] = true end
  for t = 1, rows.track_count(song) do
    if sequencer:track_sequence_slot_is_muted(t, s) ~= (muted[t] == true) then sequencer:set_track_sequence_slot_is_muted(t, s, muted[t] == true) end
  end
end

local function same_state(a, b)
  return a.start == b.start and a.name == b.name and same_list(a.mutes, b.mutes)
end

-- The kept sequence's patterns where they are now (the pool's log since `mark`): a list with false for a pattern gone, or nil
-- when what happened to the pool can't be told.
local function kept_now(env, kept, mark)
  if type(kept) ~= "table" or not env.pattern_at then return nil end
  local out = {}
  for i, p in ipairs(kept) do
    local now, why = env.pattern_at(p, mark)
    if why == "unknown" then return nil end
    out[i] = now or false
  end
  return out
end

-- Once Renoise's redo put a change back (`restore.redone`: the journal held it, the sequence and the pool changed by nothing but
-- Renoise's undo and redo since, and the sequence is the one the change left), the journal's own copy of the sequences before and
-- after it, numbered as now. Nil otherwise.
local function redone_sequences(song, restore)
  local entry = restore.redone
  if not entry or entry.redo_patterns ~= #song.patterns or type(entry.applied) ~= "table" or type(entry.prior) ~= "table" then return nil end
  local now, after, before = sequence_of(song), entry.applied.sequence, entry.prior.sequence
  if type(after) ~= "table" or type(before) ~= "table" or not same_list(now, after) then return nil end
  for _, p in ipairs(before) do
    if type(p) ~= "number" then return nil end
  end
  return after, before
end

local function stale(what)
  fail("stale", "the sequence changed in Renoise since Kumi " .. what .. " in a way that doesn't say where its slots went: nothing was changed")
end

local function slots_text(a, b)
  if a == b then return "slot " .. a end
  return string.format("slots %d to %d", a, b)
end

-- What a pattern Kumi made holds, to tell whether anyone changed it since: its name, length, slot colours, aliases and
-- automation points (its lines are told by the pattern's count of changed lines, `touch`).
local function pattern_print(song, p)
  local pattern = song:pattern(p)
  local parts = { pattern.name, tostring(pattern.number_of_lines) }
  for t = 1, rows.track_count(song) do
    local pattern_track = pattern:track(t)
    local color = pattern_track.color
    if color then parts[#parts + 1] = string.format("c%d:%d,%d,%d", t, color[1], color[2], color[3]) end
    if pattern_track.alias_pattern_index > 0 then parts[#parts + 1] = string.format("a%d:%d", t, pattern_track.alias_pattern_index) end
    for k, automation in ipairs(pattern_track.automation) do
      local points = {}
      for i, point in ipairs(automation.points) do points[i] = string.format("%.9g/%.9g/%.9g", point.time, point.value, point.scaling) end
      parts[#parts + 1] = string.format("u%d:%d:%d:%s", t, k, automation.playmode, table.concat(points, ";"))
    end
  end
  local text, a, b = table.concat(parts, "|"), 5381, 0
  for i = 1, #text do
    local byte = text:byte(i)
    a = (a * 33 + byte) % 4294967296
    b = (b * 65599 + byte) % 4294967296
  end
  return string.format("%08x%08x", a, b)
end

local function keep_pattern(song, env, p)
  return { print = pattern_print(song, p), touch = env.pattern_touch_mark and env.pattern_touch_mark(p) or nil }
end

-- Whether a pattern Kumi made is as it made it: nil, or why not.
local function pattern_changed(song, env, p, kept)
  if type(kept) ~= "table" then return "Kumi doesn't know what it held" end
  if pattern_print(song, p) ~= kept.print then return "it was changed" end
  if env.pattern_touched_since then
    local since = env.pattern_touched_since(p, kept.touch)
    if since == nil then return "Kumi can't tell whether its lines changed (the tool watched the song anew)" end
    if since then return "its lines changed" end
  end
  return nil
end

local function used_by_others(song, p, mine)
  for s, value in ipairs(song.sequencer.pattern_sequence) do
    if value == p and not mine[s] then return true end
  end
  return false
end

-- The journal follows these by the whole sequence (see changes_base.lua, `follows = "sequence"`).
-- (the host keeps the preview's prior: the sequence before is kept in `applied` too, for the restore)
local function journal_fields(prior_sequence, after_sequence)
  return { sequence = prior_sequence }, { sequence = after_sequence, before = prior_sequence }
end

-- slot-clone

local function clone_range_of(song, env, input)
  local a = seq_of(song, env, input.ref, "slot-clone")
  local b = input.last ~= nil and seq_of(song, env, input.last, "slot-clone") or a
  if b < a then bad("the last slot comes before the first") end
  return a, b
end

local function clone_title(song, a, b)
  if a == b then return string.format("Cloned slot %d (pattern %s)", a, q(patterns_.pattern_label(song, song.sequencer.pattern_sequence[a]))) end
  return "Cloned " .. slots_text(a, b)
end

local CLONE_NOTE = "Renoise copies each slot's pattern into a new one (\"#\" and its name), with the slot's mutes and section."

local function clone_preview(song, env, input)
  local a, b = clone_range_of(song, env, input)
  local n, range = slot_count(song), {}
  for s = a, b do range[#range + 1] = song.sequencer.pattern_sequence[s] end
  return {
    target = song_target(env), prior = { slots = n },
    fence = { slots = n, patterns = #song.patterns, range = range },
    display = { title = clone_title(song, a, b), before = { slots = n }, after = { slots = n + b - a + 1 }, note = CLONE_NOTE },
  }
end

local function clone_apply(song, env, input, fence, desc)
  local ok, a, b = pcall(clone_range_of, song, env, input)
  if not ok then base.gone_is_changed(a) end
  local before = sequence_of(song)
  local range = {}
  for s = a, b do range[#range + 1] = before[s] end
  if #before ~= fence.slots or #song.patterns ~= fence.patterns or not same_list(range, fence.range) then
    fail("changed", "the sequence changed since the preview")
  end
  local title = clone_title(song, a, b)
  local pool_before = #song.patterns
  song:describe_undo(desc)
  song.sequencer:clone_range(a, b)
  local after, k = sequence_of(song), b - a + 1
  local made = {}
  for i = 1, k do
    local s = b + i
    local p = after[s]
    made[i] = {
      state = state_of(song, s),
      -- (a pattern of its own when Renoise made one: past the pool's end before the clone)
      pattern = p > pool_before and keep_pattern(song, env, p) or nil,
    }
  end
  local prior, applied = journal_fields(before, after)
  applied.pool, applied.at, applied.count, applied.made = env.pattern_mark and env.pattern_mark() or nil, b + 1, k, made
  return {
    applied = applied, target = song_target(env),
    created = { ref = refs.make(env.epoch(), "seq", b + 1), kind = "seq" },
    display = { title = title, before = { slots = #before }, after = { slots = #after }, note = CLONE_NOTE },
    undo = { desc = desc, entry = true }, prior = prior, place = {}, follows = "sequence",
  }
end

-- The block of slots a record made, where it is now: its first slot, or nil.
local function block_now(kept, now, at, count)
  local first
  for i = 0, count - 1 do
    local s = base.follow_slot(kept, now, at + i)
    if not s or (first and s ~= first + i) then return nil end
    first = first or s
  end
  return first
end

local function clone_restore(song, env, restore, desc)
  local applied = restore.applied
  if type(applied) ~= "table" or type(applied.sequence) ~= "table" or type(applied.before) ~= "table" or type(applied.made) ~= "table" then
    bad("applied: the sequence before and after the clone")
  end
  local now = sequence_of(song)
  local kept, before = redone_sequences(song, restore)
  if not kept then kept, before = kept_now(env, applied.sequence, applied.pool), kept_now(env, applied.before, applied.pool) end
  if not kept or not before then stale("cloned slots") end
  local title = string.format("Removed the cloned %s (put back)", applied.count == 1 and "slot" or "slots")
  if same_list(now, before) then
    return { restored = { done = true }, display = { title = title }, undo = { desc = desc, entry = false }, unwritten = true }
  end
  local first = block_now(kept, now, applied.at, applied.count)
  if not first then stale("cloned slots") end
  local mine, count = {}, applied.count
  for i = 0, count - 1 do mine[first + i] = true end
  -- What the producer made of them since stays: Kumi takes back only slots and patterns as the clone left them.
  for i, made in ipairs(applied.made) do
    local s = first + i - 1
    if not restore.discard and not same_state(state_of(song, s), made.state) then
      fail("changed", string.format("slot %d's mutes or section changed in Renoise since Kumi cloned it", s))
    end
    if made.pattern then
      local why = pattern_changed(song, env, now[s], made.pattern)
      if why then fail("changed", string.format("the pattern %s that slot %d plays: %s since Kumi made it", q(patterns_.pattern_label(song, now[s])), s, why)) end
    end
  end
  if slot_count(song) <= count then fail("changed", "the cloned slots are all the song has now") end
  song:describe_undo(desc)
  -- (the slot after the block: deleting a section's start passes it on, so it is put back as it is now)
  local after_block = first + count <= slot_count(song) and state_of(song, first + count) or nil
  local kept_patterns = 0
  for i = count, 1, -1 do
    local s = first + i - 1
    local p = song.sequencer.pattern_sequence[s]
    local made = applied.made[i]
    if made.pattern and p == #song.patterns and not used_by_others(song, p, { [s] = true }) then
      -- (emptied first: Renoise then removes it from the pool as the slot lets go of it)
      song:pattern(p):clear()
    elseif made.pattern then
      kept_patterns = kept_patterns + 1
    end
    song.sequencer:delete_sequence_at(s)
  end
  if after_block then set_state(song, first, after_block, true) end
  base.note_restored(env.journal, "slot-clone", {}, applied, base.KINDS["slot-clone"])
  local display = { title = title }
  if kept_patterns > 0 then
    display.note = string.format("%s Renoise made for %s in the pool, unused: patterns were made after %s.",
      kept_patterns == 1 and "The pattern" or (kept_patterns .. " patterns"), kept_patterns == 1 and "it stays" or "them stay",
      kept_patterns == 1 and "it" or "them")
  end
  return { restored = { done = true }, display = display, undo = { desc = desc, entry = true } }
end

-- slot-move: a slot goes to another place (`index`, its place once moved), with its own section start and mutes. Renoise has no
-- move: the slot is made again at its new place (insert_sequence_at), set as it was, and the old one deleted, with the slots
-- Renoise touches on the way put back as they were.

local function move_to(song, from, to)
  local sequencer = song.sequencer
  local p, own = sequencer.pattern_sequence[from], state_of(song, from)
  local insert_at = to > from and to + 1 or to
  local delete_at = to > from and from or from + 1
  local pushed = insert_at <= slot_count(song) and state_of(song, insert_at) or nil
  sequencer:insert_sequence_at(insert_at, p)
  set_state(song, insert_at, own)
  if pushed then set_state(song, insert_at + 1, pushed) end
  local next_slot = delete_at + 1 <= slot_count(song) and state_of(song, delete_at + 1) or nil
  sequencer:delete_sequence_at(delete_at)
  if next_slot then set_state(song, delete_at, next_slot, true) end
end

local function moved(list, from, to)
  local out = {}
  for i, p in ipairs(list) do out[i] = p end
  local p = table.remove(out, from)
  table.insert(out, to, p)
  return out
end

local function move_plan(song, env, input)
  local s = seq_of(song, env, input.ref, "slot-move")
  local to = input.index
  local n = slot_count(song)
  if type(to) ~= "number" or to ~= math.floor(to) or to < 1 or to > n then bad(string.format("index is the slot's place once moved, from 1 to %d", n)) end
  return s, to
end

local function move_title(song, s, to, p)
  return string.format("Moved slot %d (pattern %s) to place %d", s, q(patterns_.pattern_label(song, p)), to)
end

local function move_preview(song, env, input)
  local s, to = move_plan(song, env, input)
  local sequence = sequence_of(song)
  local display = { title = move_title(song, s, to, sequence[s]), before = { index = s }, after = { index = to } }
  if s == to then display.unchanged = true end
  return { target = song_target(env), prior = { index = s }, fence = { sequence = sequence, state = state_of(song, s) }, display = display }
end

local function move_apply(song, env, input, fence, desc)
  local ok, s, to = pcall(move_plan, song, env, input)
  if not ok then base.gone_is_changed(s) end
  local before = sequence_of(song)
  if not same_list(before, fence.sequence) or not same_state(state_of(song, s), fence.state or {}) then
    fail("changed", "the sequence changed since the preview")
  end
  local display = { title = move_title(song, s, to, before[s]), before = { index = s }, after = { index = to } }
  if s == to then
    display.unchanged = true
    return { applied = { index = to }, display = display, undo = { desc = desc, entry = false }, prior = { index = s } }
  end
  song:describe_undo(desc)
  move_to(song, s, to)
  local after = sequence_of(song)
  if not same_list(after, moved(before, s, to)) then
    fail("failed", "Renoise didn't move the slot as asked (Renoise's own undo takes back what it did)")
  end
  local prior, applied = journal_fields(before, after)
  prior.index, applied.index, applied.from = s, to, s
  applied.pool = env.pattern_mark and env.pattern_mark() or nil
  return {
    applied = applied, target = song_target(env), display = display, undo = { desc = desc, entry = true },
    prior = prior, place = {}, follows = "sequence",
  }
end

local function move_restore(song, env, restore, desc)
  local applied = restore.applied
  if type(applied) ~= "table" or type(applied.sequence) ~= "table" or type(applied.before) ~= "table" or type(applied.index) ~= "number" then
    bad("applied: the sequence before and after the move")
  end
  local now = sequence_of(song)
  local kept, before = kept_now(env, applied.sequence, applied.pool), kept_now(env, applied.before, applied.pool)
  if not kept or not before then stale("moved the slot") end
  if same_list(now, before) then
    return { restored = { index = applied.from }, display = { title = "The slot is back already" }, undo = { desc = desc, entry = false }, unwritten = true }
  end
  local at = base.follow_slot(kept, now, applied.index)
  -- (where it goes back: the gap it left, followed through what the producer made or deleted since, among the slots without it)
  local without_kept, without_now = moved(kept, applied.index, #kept), moved(now, at or 1, #now)
  without_kept[#without_kept], without_now[#without_now] = nil, nil
  local back = at and base.follow_slot(without_kept, without_now, applied.from, true) or nil
  if not at or not back then stale("moved the slot") end
  local title = string.format("Moved slot %d back to place %d", at, back)
  if at == back then
    return { restored = { index = back }, display = { title = title }, undo = { desc = desc, entry = false }, unwritten = true }
  end
  song:describe_undo(desc)
  move_to(song, at, back)
  base.note_restored(env.journal, "slot-move", {}, applied, base.KINDS["slot-move"])
  return { restored = { index = back }, display = { title = title }, undo = { desc = desc, entry = true } }
end

-- slot-unique

local function unique_title(a, b) return "Made the patterns of " .. slots_text(a, b) .. " unique" end

local function unique_plan(song, env, input)
  local a = seq_of(song, env, input.ref, "slot-unique")
  local b = input.last ~= nil and seq_of(song, env, input.last, "slot-unique") or a
  if b < a then bad("the last slot comes before the first") end
  local sequence, played = sequence_of(song), {}
  for _, p in ipairs(sequence) do played[p] = (played[p] or 0) + 1 end
  local shared = 0
  for s = a, b do
    if played[sequence[s]] > 1 then shared = shared + 1 end
  end
  return a, b, sequence, shared
end

local function unique_preview(song, env, input)
  local a, b, sequence, shared = unique_plan(song, env, input)
  local display = { title = unique_title(a, b), before = { patterns = #song.patterns }, after = { patterns = #song.patterns + shared } }
  if shared == 0 then display.unchanged = true else display.note = "Each of those slots whose pattern another slot plays gets a copy of it (\"#\" and its name)." end
  return { target = song_target(env), prior = { slots = #sequence }, fence = { sequence = sequence, patterns = #song.patterns }, display = display }
end

local function unique_apply(song, env, input, fence, desc)
  local ok, a, b, sequence, shared = pcall(unique_plan, song, env, input)
  if not ok then base.gone_is_changed(a) end
  if not same_list(sequence, fence.sequence) or #song.patterns ~= fence.patterns then fail("changed", "the sequence changed since the preview") end
  local display = { title = unique_title(a, b), before = { patterns = #song.patterns } }
  if shared == 0 then
    display.unchanged, display.after = true, { patterns = #song.patterns }
    return { applied = { sequence = sequence }, display = display, undo = { desc = desc, entry = false }, prior = { sequence = sequence } }
  end
  local pool_before = #song.patterns
  song:describe_undo(desc)
  song.sequencer:make_range_unique(a, b)
  local after = sequence_of(song)
  local changed = {}
  for s = a, b do
    if after[s] ~= sequence[s] then
      changed[#changed + 1] = { slot = s, from = sequence[s], to = after[s], pattern = after[s] > pool_before and keep_pattern(song, env, after[s]) or nil }
    end
  end
  display.after = { patterns = #song.patterns }
  local prior, applied = journal_fields(sequence, after)
  applied.pool, applied.changed = env.pattern_mark and env.pattern_mark() or nil, changed
  return {
    applied = applied, target = song_target(env), display = display, undo = { desc = desc, entry = true },
    prior = prior, place = {}, follows = "sequence",
  }
end

local function unique_restore(song, env, restore, desc)
  local applied = restore.applied
  if type(applied) ~= "table" or type(applied.sequence) ~= "table" or type(applied.before) ~= "table" or type(applied.changed) ~= "table" then
    bad("applied: the sequence before and after")
  end
  local now = sequence_of(song)
  local redone_after, redone_before = redone_sequences(song, restore)
  local kept, before = redone_after, redone_before
  if not kept then kept, before = kept_now(env, applied.sequence, applied.pool), kept_now(env, applied.before, applied.pool) end
  if not kept or not before then stale("made the patterns unique") end
  local title = "Slots play their shared patterns again (put back)"
  if same_list(now, before) then
    return { restored = { done = true }, display = { title = title }, undo = { desc = desc, entry = false }, unwritten = true }
  end
  -- each changed slot where it is now, playing the copy Kumi made, and the pattern it played before, where that is now
  local todo = {}
  for _, change in ipairs(applied.changed) do
    local s = base.follow_slot(kept, now, change.slot)
    if not s then stale("made the patterns unique") end
    local copy, original
    if redone_after and redone_before then
      copy, original = redone_after[change.slot], redone_before[change.slot]
    else
      copy, original = env.pattern_at(change.to, applied.pool), env.pattern_at(change.from, applied.pool)
    end
    if not copy or now[s] ~= copy then fail("changed", string.format("slot %d plays another pattern now", s)) end
    if not original then fail("changed", string.format("the pattern slot %d played before is gone since", s)) end
    if change.pattern then
      local why = pattern_changed(song, env, copy, change.pattern)
      if why then fail("changed", string.format("the copy %s that slot %d plays: %s since Kumi made it", q(patterns_.pattern_label(song, copy)), s, why)) end
    end
    todo[#todo + 1] = { slot = s, copy = copy, original = original, made = change.pattern ~= nil }
  end
  -- (the copies from the pool's end down: each removed as its slot lets go of it, which leaves the one before last)
  table.sort(todo, function(x, y) return x.copy > y.copy end)
  song:describe_undo(desc)
  local kept_patterns = 0
  for _, item in ipairs(todo) do
    local mine = { [item.slot] = true }
    if item.made and item.copy == #song.patterns and not used_by_others(song, item.copy, mine) then
      song:pattern(item.copy):clear()
    elseif item.made then
      kept_patterns = kept_patterns + 1
    end
    song.sequencer:set_pattern(item.slot, item.original)
  end
  base.note_restored(env.journal, "slot-unique", {}, applied, base.KINDS["slot-unique"])
  local display = { title = title }
  if kept_patterns > 0 then
    display.note = string.format("%s stay in the pool, unused: patterns were made after them.", kept_patterns == 1 and "The copy" or (kept_patterns .. " copies"))
  end
  return { restored = { done = true }, display = display, undo = { desc = desc, entry = true } }
end

-- (the journal compares the whole sequence; see changes_base.lua)
local function sequence_spec(spec)
  spec.custom = true
  spec.same = function(a, b) return same_list(a.sequence, b.sequence) end
  base.register(spec)
end

-- The patterns a change made, where they are once Renoise's redo made them again (`redo_marks`): their counts of changed lines
-- start again there (the journal's `retouched_at`).
local function clone_marks(entry)
  local out, applied = {}, entry.applied
  for i, made in ipairs(type(applied.made) == "table" and applied.made or {}) do
    if type(made.pattern) == "table" then out[#out + 1] = { mark = made.pattern.touch, p = applied.sequence[applied.at + i - 1] } end
  end
  return out
end

local function unique_marks(entry)
  local out, applied = {}, entry.applied
  for _, change in ipairs(type(applied.changed) == "table" and applied.changed or {}) do
    if type(change.pattern) == "table" then out[#out + 1] = { mark = change.pattern.touch, p = applied.sequence[change.slot] } end
  end
  return out
end

sequence_spec { kind = "slot-clone", preview = clone_preview, apply = clone_apply, restore = clone_restore, redo_marks = clone_marks }
sequence_spec { kind = "slot-move", preview = move_preview, apply = move_apply, restore = move_restore }
sequence_spec { kind = "slot-unique", preview = unique_preview, apply = unique_apply, restore = unique_restore, redo_marks = unique_marks }

return M
