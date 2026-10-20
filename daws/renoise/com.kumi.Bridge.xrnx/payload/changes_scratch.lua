-- scratch-keep (M5 contract §9): a scratch copy made a track of the producer's (a goal's best): renamed, turned on (a copy is OFF,
-- processing nothing), out of the scratch journal so no drop or sweep removes it. Kumi's undo deletes it while it is as Kumi left
-- it. The rename is a Renoise undo step; the mute isn't (M3 fact), so Renoise's own undo takes back the name only, and says so.

local import = ...
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")
local scratch = import("scratch")

local fail, bad, q = base.fail, base.bad, base.q

local M = {}

local NOTE = "Renoise's own undo takes back its name, not its mute (mute is no undo step in Renoise); Kumi's undo removes the track."

-- What tells the kept track apart from what Kumi left: its own name and kind, its mute, its devices and the patterns it has lines in.
local function signature(song, index, track)
  local lines = 0
  for p = 1, #song.patterns do
    local pattern_track = song:pattern(p):track(index)
    if pattern_track.is_alias or not pattern_track.is_empty then lines = lines + 1 end
  end
  return { id = base.track_id(track), mute = track.mute_state, devices = #track.devices, patternsWithLines = lines }
end

-- The copy the ref names: its place, the track, its scratch session and copy.
local function locate(song, env, input)
  if type(input.ref) ~= "string" then bad("ref must be a scratch copy's track ref") end
  local index = refs.expect(input.ref, "track", 1, env.epoch())[1]
  local track = base.track_at(song, index)
  local session, copy = scratch.copy_named(track.name)
  if not session or track.type ~= renoise.Track.TRACK_TYPE_SEQUENCER then bad("that track isn't one of Kumi's scratch copies") end
  return index, track, session, copy
end

local function wanted_name(input)
  local name = input.name
  if type(name) ~= "string" or name == "" or #name > 1024 or name:find("[%c]") then bad("name must be one line of text") end
  if name:sub(1, #scratch.PREFIX) == scratch.PREFIX then bad("a kept track takes a name of its own, not " .. q(scratch.PREFIX .. "…")) end
  return name
end

local function preview(song, env, input)
  local index, track, session = locate(song, env, input)
  local name = wanted_name(input)
  local now = signature(song, index, track)
  return {
    target = base.target(env, index),
    prior = { name = track.name, mute = track.mute_state },
    fence = {
      id = now.id, name = track.name, mute = now.mute, devices = now.devices, patternsWithLines = now.patternsWithLines, scratchId = session.id,
    },
    display = { title = "Kept " .. q(name), before = { name = track.name }, after = { name = name }, note = NOTE },
  }
end

local function apply(song, env, input, fence, desc)
  local name = wanted_name(input)
  local ok, index, track, session, copy = pcall(locate, song, env, input)
  if not ok then
    -- (the track at the ref's place is no copy of Kumi's any more: renamed, or another track moved there)
    if type(index) == "table" and index.code == "bad-params" then fail("changed", base.moved_text(fence.name)) end
    base.gone_is_changed(index)
  end
  local now = signature(song, index, track)
  if now.id ~= fence.id or track.name ~= fence.name then fail("changed", base.moved_text(fence.name)) end
  if now.mute ~= fence.mute or now.devices ~= fence.devices or now.patternsWithLines ~= fence.patternsWithLines or session.id ~= fence.scratchId then
    fail("changed", q(fence.name) .. " changed since the preview")
  end
  song:describe_undo(desc)
  local old = track.name
  track.name = name
  track.mute_state = renoise.Track.MUTE_STATE_ACTIVE
  scratch.keep(session, copy)
  local kept = signature(song, index, track)
  return {
    applied = { name = name, mute = kept.mute, id = kept.id },
    created = { ref = refs.make(env.epoch(), "track", index), kind = "track" },
    material = { track = index, copyName = old, scratchId = session.id, devices = kept.devices, patternsWithLines = kept.patternsWithLines },
    display = { title = "Kept " .. q(name), before = { name = old }, after = { name = name }, note = NOTE },
    undo = { desc = desc, entry = true },
    target = base.target(env, index), place = { track = index }, prior = { name = old }, journal_applied = { name = name },
  }
end

local function restore(song, env, record, desc)
  local created, applied, material = record.created, record.applied, record.material
  if type(created) ~= "table" or type(created.ref) ~= "string" then bad("created: the track Kumi kept") end
  if type(applied) ~= "table" or type(material) ~= "table" then bad("applied and material: what the keep wrote") end
  local index = refs.expect(created.ref, "track", 1, env.epoch())[1]
  local track = base.track_at(song, index)
  local label = base.named(index, track)
  if base.track_id(track) ~= applied.id or track.type ~= renoise.Track.TRACK_TYPE_SEQUENCER then
    fail("changed", q(label) .. " is not the track Kumi kept any more")
  end
  local now = signature(song, index, track)
  if not record.discard and (now.mute ~= applied.mute or now.devices ~= material.devices or now.patternsWithLines ~= material.patternsWithLines) then
    fail("changed", q(label) .. " changed in Renoise since Kumi kept it (its devices, lines or mute)")
  end
  song:describe_undo(desc)
  song:delete_track_at(index)
  base.note_restored(env.journal, "scratch-keep", { track = index }, { name = applied.name }, base.KINDS["scratch-keep"])
  return {
    restored = { deleted = true },
    display = { title = "Removed " .. q(label) .. " (put back)" },
    undo = { desc = desc, entry = true },
  }
end

base.register({
  kind = "scratch-keep", custom = true,
  preview = preview, apply = apply, restore = restore,
  -- (the journal reads the name again after the producer's undo or redo: that is the part Renoise's undo takes back)
  reread = function(song, place)
    if place.track >= 1 and place.track <= rows.track_count(song) then return song:track(place.track) end
    return nil
  end,
  read = function(track) return { name = track.name } end,
  same = function(a, b) return type(a) == "table" and type(b) == "table" and a.name == b.name end,
})

return M
