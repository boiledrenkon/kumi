-- Kumi's changes to the song's sequence: making a slot, deleting one, giving one another pattern, a section's start and name, a
-- track's mute in a slot. A slot is `E:seq:n`; a track's mute in it is `E:slot:track.n`.
--
-- Renoise removes the last pattern of the pool when no slot uses it and nothing is in it (measured, 3.5.4: whatever its name or
-- length; a pattern earlier in the pool, or with something in it, stays, unused). So a slot deleted over such a pattern keeps
-- the pattern's name and length (`material.gone`) to make it again.

local import = ...
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")

local bad, fail = base.bad, base.fail
local q = base.q
local M = {}

local function slot_count(song) return #song.sequencer.pattern_sequence end

local function seq_of(song, env, reference, what)
  if type(reference) ~= "string" then bad(what .. ": ref must be a sequence slot ref") end
  local s = refs.expect(reference, "seq", 1, env.epoch())[1]
  if s < 1 or s > slot_count(song) then fail("gone", "that sequence slot isn't in the song any more") end
  return s
end

local function pattern_label(song, p)
  local name = song:pattern(p).name
  if name == nil or name == "" then name = string.format("Pattern %02X", p - 1) end
  return name
end

local function slot_text(s) return "slot " .. s end
local function slot_title(s) return "Slot " .. s end

local function with_pattern(song, p) return "pattern " .. q(pattern_label(song, p)) end

local function seq_target(env, s) return { ref = refs.make(env.epoch(), "seq", s), kind = "seq" } end

local function used_elsewhere(song, p, s)
  for k, value in ipairs(song.sequencer.pattern_sequence) do
    if value == p and k ~= s then return true end
  end
  return false
end

-- Renoise would remove this pattern if the slot let go of it.
local function goes_with(song, p, s)
  return p == #song.patterns and not used_elsewhere(song, p, s) and song:pattern(p).is_empty
end

-- slot-insert

local function insert_plan(song, env, input)
  local n = slot_count(song)
  local index = input.index
  if index == nil then index = n + 1 end
  if type(index) ~= "number" or index ~= math.floor(index) or index < 1 or index > n + 1 then
    bad(string.format("index is a whole number from 1 to %d", n + 1))
  end
  local pattern
  if input.pattern ~= nil and input.pattern ~= "new" then
    if type(input.pattern) ~= "string" then bad("pattern: a pattern ref, or \"new\" (the default) for an empty pattern of its own") end
    pattern = refs.expect(input.pattern, "pattern", 1, env.epoch())[1]
    if pattern > #song.patterns then fail("gone", "that pattern isn't in the song any more") end
  end
  return { index = index, pattern = pattern, slots = n, patterns = #song.patterns }
end

local function insert_title(song, plan)
  return string.format("Inserted slot %d (%s)", plan.index, plan.pattern and with_pattern(song, plan.pattern) or "a new pattern")
end

local function insert_preview(song, env, input)
  local plan = insert_plan(song, env, input)
  return {
    target = seq_target(env, math.min(plan.index, math.max(plan.slots, 1))), prior = { slots = plan.slots },
    fence = { slots = plan.slots, patterns = plan.patterns },
    display = { title = insert_title(song, plan), before = { slots = plan.slots }, after = { slots = plan.slots + 1, index = plan.index } },
  }
end

local function insert_apply(song, env, input, fence, desc)
  local plan = insert_plan(song, env, input)
  if plan.slots ~= fence.slots or plan.patterns ~= fence.patterns then fail("changed", "the sequence changed since the preview") end
  local sequencer = song.sequencer
  song:describe_undo(desc)
  if plan.pattern then sequencer:insert_sequence_at(plan.index, plan.pattern) else sequencer:insert_new_pattern_at(plan.index) end
  local pattern = sequencer.pattern_sequence[plan.index]
  return {
    -- (a pattern is a number only within an epoch: the record keeps it as a ref, which a renumbered pool refuses)
    applied = { index = plan.index, pattern = refs.make(env.epoch(), "pattern", pattern), slots = plan.slots + 1 },
    created = { ref = refs.make(env.epoch(), "seq", plan.index), kind = "seq" }, target = seq_target(env, plan.index),
    display = {
      title = insert_title(song, plan), before = { slots = plan.slots }, after = { slots = plan.slots + 1, index = plan.index },
    },
    undo = { desc = desc, entry = true }, prior = { slots = plan.slots }, place = { seq = plan.index }, shape = "made", follows = "slots",
  }
end

local function insert_restore(song, env, restore, desc)
  local created, applied = restore.created, restore.applied
  if type(created) ~= "table" or type(created.ref) ~= "string" then bad("created: the slot the change made") end
  if type(applied) ~= "table" then bad("applied: what the change wrote") end
  local s = seq_of(song, env, created.ref, "slot-insert")
  local sequencer = song.sequencer
  if type(applied.pattern) ~= "string" then bad("applied.pattern: the pattern the slot played, as a ref") end
  local made = refs.expect(applied.pattern, "pattern", 1, env.epoch())[1]
  if sequencer.pattern_sequence[s] ~= made then
    fail("changed", "the slot " .. s .. " holds another pattern now: it was changed in Renoise since Kumi made it")
  end
  if not restore.discard then
    if sequencer:sequence_is_start_of_section(s) then fail("changed", "slot " .. s .. " starts a section now: Kumi won't delete it") end
    for t = 1, rows.track_count(song) do
      if sequencer:track_sequence_slot_is_muted(t, s) then fail("changed", "tracks are muted in slot " .. s .. " now: Kumi won't delete it") end
    end
  end
  if slot_count(song) <= 1 then fail("changed", "slot " .. s .. " is the song's only one") end
  song:describe_undo(desc)
  sequencer:delete_sequence_at(s)
  base.note_restored(env.journal, "slot-insert", { seq = s })
  return { restored = { deleted = true }, display = { title = "Removed " .. slot_text(s) .. " (put back)" }, undo = { desc = desc, entry = true } }
end

base.register({ kind = "slot-insert", custom = true, shape = "made", preview = insert_preview, apply = insert_apply, restore = insert_restore })

-- slot-delete

local function delete_info(song, s)
  local sequencer = song.sequencer
  local p = sequencer.pattern_sequence[s]
  local mutes = {}
  for t = 1, rows.track_count(song) do
    if sequencer:track_sequence_slot_is_muted(t, s) then mutes[#mutes + 1] = { track = t } end
  end
  local start = sequencer:sequence_is_start_of_section(s)
  -- (the slot's place is the sequence before it: Renoise's sequence notifier can't say where a slot went, so the place holds
  -- while the slots before it are the same, whatever is made or deleted after)
  local before = {}
  for k = 1, s - 1 do before[k] = sequencer.pattern_sequence[k] end
  local info = {
    index = s, pattern = p, count = slot_count(song), before = before, beside = { sequencer.pattern_sequence[s - 1], sequencer.pattern_sequence[s + 1] },
    section = { start = start, name = start and sequencer:sequence_section_name(s) or "" },
    mutes = mutes,
  }
  -- (deleting a slot where a section starts makes the next slot start it: the next slot's own start is kept to put back)
  if s < slot_count(song) then
    local next_start = sequencer:sequence_is_start_of_section(s + 1)
    info.next = { start = next_start, name = next_start and sequencer:sequence_section_name(s + 1) or "" }
  end
  return info, p
end

local function delete_preview(song, env, input)
  local s = seq_of(song, env, input.ref, "slot-delete")
  if slot_count(song) <= 1 then bad("the song's only slot can't be deleted") end
  local info, p = delete_info(song, s)
  local display = {
    title = string.format("Deleted %s (%s)", slot_text(s), with_pattern(song, p)),
    before = { slots = info.count }, after = { slots = info.count - 1 },
  }
  if goes_with(song, p, s) then
    display.note = "The pattern is empty and no other slot uses it, so Renoise removes it; Kumi keeps its name and length to put back."
  end
  return {
    target = seq_target(env, s), prior = { deleted = false },
    fence = { slots = info.count, pattern = refs.make(env.epoch(), "pattern", p), mutes = #info.mutes, section = info.section.start },
    display = display,
  }
end

local function delete_apply(song, env, input, fence, desc)
  local ok, s = pcall(seq_of, song, env, input.ref, "slot-delete")
  if not ok then base.gone_is_changed(s) end
  if slot_count(song) <= 1 then bad("the song's only slot can't be deleted") end
  local info, p = delete_info(song, s)
  if fence.slots ~= info.count or fence.pattern ~= refs.make(env.epoch(), "pattern", p) or fence.mutes ~= #info.mutes
    or fence.section ~= info.section.start then
    fail("changed", slot_text(s) .. " changed since the preview")
  end
  local pattern = song:pattern(p)
  local before = #song.patterns
  local title = string.format("Deleted %s (%s)", slot_text(s), with_pattern(song, p))
  local saved = { name = pattern.name, lines = pattern.number_of_lines }
  song:describe_undo(desc)
  song.sequencer:delete_sequence_at(s)
  if #song.patterns < before then
    info.gone = saved
    info.pattern = nil
  else
    -- (the pattern stays in the pool: a ref to it, which a renumbered pool refuses)
    info.pattern = refs.make(env.epoch(), "pattern", p)
  end
  return {
    applied = { deleted = true, slots = info.count - 1 }, material = info, target = seq_target(env, s),
    display = { title = title, before = { slots = info.count }, after = { slots = info.count - 1 } },
    undo = { desc = desc, entry = true }, prior = { deleted = false, slots = info.count }, place = { seq = s }, shape = "deleted", follows = "slots",
  }
end

local function delete_restore(song, env, restore, desc)
  local material = restore.material
  if type(material) ~= "table" or type(material.index) ~= "number" or (not material.gone and type(material.pattern) ~= "string") then
    bad("material: what the delete removed")
  end
  local sequencer = song.sequencer
  local n = slot_count(song)
  local index = material.index
  local before = type(material.before) == "table" and material.before or nil
  local holds = before ~= nil and #before == index - 1 and index - 1 <= n
  if holds and before then
    for k = 1, index - 1 do
      if sequencer.pattern_sequence[k] ~= before[k] then holds = false break end
    end
  end
  -- (patterns repeat: a slot whose pattern a neighbour had can't be told from it once a slot is made or deleted before it, unless
  -- the sequence is as the delete left it)
  local beside = type(material.beside) == "table" and material.beside or {}
  if holds and n ~= material.count - 1 then
    local own = not material.gone and type(material.pattern) == "string" and refs.parse(material.pattern) or nil
    local mine = own and own.n[1] or nil
    if mine and (beside[1] == mine or beside[2] == mine) then holds = false end
  end
  if not holds then
    fail("stale", string.format("the sequence before %s changed in Renoise since Kumi deleted it, so Kumi can't tell where it goes: nothing was put back", slot_text(index)))
  end
  -- (the pattern it played, if it stayed in the pool: a ref, `stale` when the pool was renumbered since)
  local kept_pattern = not material.gone and refs.expect(material.pattern, "pattern", 1, env.epoch())[1] or nil
  -- back already: the song is as long as it was and the slot holds the pattern
  if n == material.count and sequencer.pattern_sequence[index] == kept_pattern and not material.gone then
    return { restored = { index = index }, display = { title = slot_title(index) .. " is back already" }, undo = { desc = desc, entry = false } }
  end
  local at = math.max(1, math.min(index, n + 1))
  if kept_pattern and kept_pattern > #song.patterns then
    fail("failed", "the pattern the slot held isn't in the song any more")
  end
  song:describe_undo(desc)
  local p = kept_pattern
  if material.gone then
    sequencer:insert_new_pattern_at(at)
    p = sequencer.pattern_sequence[at]
    local pattern = song:pattern(p)
    if material.gone.name and material.gone.name ~= "" then pattern.name = material.gone.name end
    if material.gone.lines and pattern.number_of_lines ~= material.gone.lines then pattern.number_of_lines = material.gone.lines end
  else
    sequencer:insert_sequence_at(at, p)
  end
  -- (Renoise gives a slot made in the middle the section start and the muted tracks of the slot it pushes up: both slots are
  -- put as they were)
  local section = material.section or {}
  if section.start then
    sequencer:set_sequence_is_start_of_section(at, true)
    sequencer:set_sequence_section_name(at, section.name or "")
  else
    sequencer:set_sequence_is_start_of_section(at, false)
  end
  local next_slot = material.next
  if next_slot and at + 1 <= slot_count(song) then
    sequencer:set_sequence_is_start_of_section(at + 1, next_slot.start == true)
    if next_slot.start then sequencer:set_sequence_section_name(at + 1, next_slot.name or "") end
  end
  local muted = {}
  for _, mute in ipairs(material.mutes or {}) do muted[mute.track] = true end
  for t = 1, rows.track_count(song) do
    if sequencer:track_sequence_slot_is_muted(t, at) ~= (muted[t] == true) then sequencer:set_track_sequence_slot_is_muted(t, at, muted[t] == true) end
  end
  base.note_restored(env.journal, "slot-delete", { seq = at })
  local display = { title = slot_title(at) .. " put back" }
  local lost = tonumber(material.mutesLost) or 0
  if lost > 0 then
    display.note = lost == 1 and "A track muted in it was deleted since, so it isn't there to mute."
      or (lost .. " tracks muted in it were deleted since, so they aren't there to mute.")
  end
  return {
    restored = { ref = refs.make(env.epoch(), "seq", at), pattern = p },
    display = display, undo = { desc = desc, entry = true },
  }
end

base.register({ kind = "slot-delete", custom = true, shape = "deleted", preview = delete_preview, apply = delete_apply, restore = delete_restore })

-- slot-pattern: the slot plays another pattern. The pattern it played must not be one Renoise would then remove.

local function pattern_input(song, env, input)
  if type(input.pattern) ~= "string" then bad("pattern: a pattern ref") end
  local p = refs.expect(input.pattern, "pattern", 1, env.epoch())[1]
  if p > #song.patterns then fail("gone", "that pattern isn't in the song any more") end
  return p
end

do
  local spec = {
    kind = "slot-pattern", noun = "pattern", undoable = true,
    locate = function(song, env, input)
      local s = seq_of(song, env, input.ref, "slot-pattern")
      local loc = {
        object = { sequencer = song.sequencer, s = s }, name = slot_text(s), place = { seq = s }, target = seq_target(env, s),
      }
      if input.pattern ~= nil then
        local p = pattern_input(song, env, input)
        local current = song.sequencer.pattern_sequence[s]
        if p ~= current and goes_with(song, current, s) then
          bad(string.format("%s's pattern %s is empty and no other slot uses it: Renoise would remove it and the change couldn't be put back. Make a slot with the pattern you want (slot-insert), then delete this one",
            loc.name, pattern_label(song, current)))
        end
        loc.object.wanted = p
      end
      return loc
    end,
    reread = function(song, place)
      if place.seq > slot_count(song) then return nil end
      return { sequencer = song.sequencer, s = place.seq }
    end,
    read = function(object) return { pattern = object.sequencer.pattern_sequence[object.s] } end,
    wanted = function(object)
      if not object.wanted then bad("pattern: a pattern ref") end
      return { pattern = object.wanted }
    end,
    write = function(object, state) object.sequencer:set_pattern(object.s, state.pattern) end,
    same = function(a, b) return a.pattern == b.pattern end,
    -- (the pattern in a record is a ref: the pool's numbers move when the producer deletes unused patterns)
    uses_patterns = true,
    wire = function(state, env)
      local out = {}
      for k, v in pairs(state) do out[k] = v end
      out.pattern = refs.make(env.epoch(), "pattern", state.pattern)
      return out
    end,
    unwire = function(state, env)
      if type(state) ~= "table" or type(state.pattern) ~= "string" then bad("prior and applied: the pattern, as a ref") end
      local out = {}
      for k, v in pairs(state) do out[k] = v end
      out.pattern = refs.expect(state.pattern, "pattern", 1, env.epoch())[1]
      return out
    end,
    title = function(name, _, to)
      local label = name:gsub("^%l", string.upper)
      return string.format("%s \226\134\146 %s", label, with_pattern(renoise.song(), to.pattern))
    end,
  }
  base.register(spec)
end

-- slot-section: a slot starts a section, with a name (a name alone starts one)

local function section_spec()
  local function read(object)
    local start = object.sequencer:sequence_is_start_of_section(object.s)
    return { start = start, name = start and object.sequencer:sequence_section_name(object.s) or "" }
  end
  return {
    kind = "slot-section", noun = "section", undoable = true,
    locate = function(song, env, input)
      local s = seq_of(song, env, input.ref, "slot-section")
      return { object = { sequencer = song.sequencer, s = s }, name = slot_text(s), place = { seq = s }, target = seq_target(env, s) }
    end,
    reread = function(song, place)
      if place.seq > slot_count(song) then return nil end
      return { sequencer = song.sequencer, s = place.seq }
    end,
    read = read,
    wanted = function(object, input)
      local now = read(object)
      local start, name = input.start, input.name
      if start ~= nil and type(start) ~= "boolean" then bad("start must be true or false") end
      if name ~= nil and (type(name) ~= "string" or #name > 200) then bad("name must be text of at most 200 characters") end
      if start == nil and name == nil then bad("give start (true or false) or name") end
      if start == false and name ~= nil then bad("a slot that doesn't start a section has no name") end
      if name ~= nil then start = true end
      if start == nil then start = now.start end
      if not start then return { start = false, name = "" } end
      return { start = true, name = name or now.name }
    end,
    write = function(object, state)
      local sequencer = object.sequencer
      if state.start then
        sequencer:set_sequence_is_start_of_section(object.s, true)
        sequencer:set_sequence_section_name(object.s, state.name)
      else
        sequencer:set_sequence_is_start_of_section(object.s, false)
      end
    end,
    same = function(a, b) return a.start == b.start and a.name == b.name end,
    title = function(name, _, to)
      if not to.start then return "No section at " .. name end
      if to.name == "" then return "Section at " .. name end
      return string.format("Section %s at %s", q(to.name), name)
    end,
  }
end

base.register(section_spec())

-- slot-mute: a track is silent in a slot

local function mute_spec()
  return {
    kind = "slot-mute", noun = "mute", undoable = true,
    locate = function(song, env, input)
      local t, s
      local parsed = type(input.ref) == "string" and refs.parse(input.ref) or nil
      if parsed and parsed.kind == "slot" then
        local numbers = refs.expect(input.ref, "slot", 2, env.epoch())
        t, s = numbers[1], numbers[2]
        if s > slot_count(song) then fail("gone", "that sequence slot isn't in the song any more") end
        base.track_at(song, t)
      else
        s = seq_of(song, env, input.ref, "slot-mute")
        if type(input.track) ~= "string" then bad("track: the ref of the track to mute in the slot") end
        t = refs.expect(input.track, "track", 1, env.epoch())[1]
        base.track_at(song, t)
      end
      local track = song:track(t)
      if track.type == renoise.Track.TRACK_TYPE_MASTER then bad("the master track has no mute in a slot") end
      return {
        object = { sequencer = song.sequencer, t = t, s = s }, name = string.format("%s in %s", base.named(t, track), slot_text(s)),
        place = { track = t, seq = s }, target = { ref = refs.make(env.epoch(), "slot", t, s), kind = "slot" },
        id = base.track_id(track) .. "|slot",
      }
    end,
    reread = function(song, place)
      if place.seq > slot_count(song) or place.track > rows.track_count(song) then return nil end
      return { sequencer = song.sequencer, t = place.track, s = place.seq }
    end,
    read = function(object) return { mute = object.sequencer:track_sequence_slot_is_muted(object.t, object.s) } end,
    wanted = function(_, input)
      if type(input.mute) ~= "boolean" then bad("mute must be true or false") end
      return { mute = input.mute }
    end,
    write = function(object, state) object.sequencer:set_track_sequence_slot_is_muted(object.t, object.s, state.mute) end,
    same = function(a, b) return a.mute == b.mute end,
    title = function(name, _, to) return name .. (to.mute and " muted" or " unmuted") end,
  }
end

base.register(mute_spec())

return M
