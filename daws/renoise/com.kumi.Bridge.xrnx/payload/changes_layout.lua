-- Kumi's changes to the order of the tracks: making a track (sequencer, send or group), moving one, putting one into a group
-- or out of it. Renoise's groups are positional: a group track holds the tracks just before it (its `members` count), a swap
-- moves a track and leaves the groups' counts, and `add_track_to_group` / `remove_track_from_group` move a track by swaps
-- (measured, 3.5.4). Every move here is made of swaps, so it is put back by the same swaps the other way.

local import = ...
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")
local json = import("json")

local bad, fail = base.bad, base.fail
local q = base.q
local M = {}

local function master_index(song) return song.sequencer_track_count + 1 end

local function is_group(track) return track.type == renoise.Track.TRACK_TYPE_GROUP end

local function track_label(index, track) return base.named(index, track) end

-- What tells a track apart from what the preview saw.
local function signature(index, track)
  -- (the name is the track's own: a track named by its place is "", since that name moves with the place)
  return { name = base.track_id(track), kind = rows.track_type(track), devices = math.max(#track.devices - 1, 0), index = index }
end

-- A track holds nothing of anyone's: no device but its mixer, no lines, and (a group) no members.
local function untouched(song, index, track)
  if #track.devices > 1 then return false end
  if is_group(track) and #track.members > 0 then return false end
  for p = 1, #song.patterns do
    local pattern_track = song:pattern(p):track(index)
    if not pattern_track.is_alias and not pattern_track.is_empty then return false end
  end
  return true
end

-- track-add

local KINDS = { sequencer = true, send = true, group = true }

local function add_plan(song, env, input)
  local kind = input.type
  if kind == nil then kind = "sequencer" end
  if not KINDS[kind] then bad("type must be \"sequencer\", \"send\" or \"group\"") end
  if input.name ~= nil and (type(input.name) ~= "string" or #input.name > 200) then bad("name must be text of at most 200 characters") end
  local color
  if input.color ~= nil then
    color = base.rgb_of(input.color)
    if not color then bad("color must be \"#rrggbb\"") end
  end
  local master, count = master_index(song), rows.track_count(song)
  local index, group = input.index, nil
  if input.group ~= nil then
    if kind ~= "sequencer" then bad("only a sequencer track is made inside a group") end
    if index ~= nil then bad("give index or group, not both: a track made inside a group is made at the group's place") end
    local at = refs.expect(input.group, "track", 1, env.epoch())[1]
    local candidate = base.track_at(song, at)
    if not is_group(candidate) then bad("group must be the ref of a group track") end
    group, index = at, at
  end
  if kind == "send" then
    if index == nil then index = count + 1 end
    if type(index) ~= "number" or index ~= math.floor(index) or index <= master or index > count + 1 then
      bad(string.format("a send track's index is from %d to %d (after the master track)", master + 1, count + 1))
    end
  else
    if index == nil then index = master end
    if type(index) ~= "number" or index ~= math.floor(index) or index < 1 or index > master then
      bad(string.format("a %s track's index is from 1 to %d (before the master track)", kind, master))
    end
  end
  return { kind = kind, index = index, name = input.name, color = color, group = group, master = master, count = count }
end

local function kind_word(kind) return kind == "group" and "group" or kind == "send" and "send track" or "track" end

local function add_title(plan)
  local what = kind_word(plan.kind)
  if plan.name and plan.name ~= "" then return string.format("Added %s %s", what, q(plan.name)) end
  return string.format("Added a %s at place %d", what, plan.index)
end

local function add_preview(song, env, input)
  local plan = add_plan(song, env, input)
  local display = { title = add_title(plan), before = { tracks = plan.count }, after = { tracks = plan.count + 1, index = plan.index } }
  if plan.kind == "sequencer" and not plan.group then
    -- (a track made just before a group's tracks' end joins the group: say so)
    local parents = rows.group_parents(song)
    local next_track = plan.index <= plan.count and song:track(plan.index) or nil
    if next_track and is_group(next_track) and #next_track.members > 0 then
      display.note = "It joins the group " .. track_label(plan.index, next_track) .. ": it is made just before it."
    elseif parents[plan.index] then
      display.note = "It is made inside the group " .. track_label(parents[plan.index], song:track(parents[plan.index])) .. "."
    end
  end
  return { target = { ref = refs.make(env.epoch(), "track", plan.index), kind = "track" }, prior = { tracks = plan.count }, fence = { tracks = plan.count, master = plan.master }, display = display }
end

local function add_apply(song, env, input, fence, desc)
  local plan = add_plan(song, env, input)
  if plan.count ~= fence.tracks or plan.master ~= fence.master then
    fail("changed", "the tracks changed since the preview")
  end
  song:describe_undo(desc)
  if plan.kind == "group" then
    song:insert_group_at(plan.index)
  else
    song:insert_track_at(plan.index)
  end
  if plan.group and not song:track(plan.index).group_parent then song:add_track_to_group(plan.index, plan.index + 1) end
  local track = song:track(plan.index)
  if plan.name and plan.name ~= "" then track.name = plan.name end
  if plan.color then track.color = plan.color end
  local parents = rows.group_parents(song)
  local applied = { index = plan.index, kind = rows.track_type(track), name = track.name, id = base.track_id(track) }
  if parents[plan.index] then applied.group = parents[plan.index] end
  local title = string.format("Added %s %s", kind_word(applied.kind), q(track_label(plan.index, track)))
  return {
    applied = applied, material = { track = plan.index },
    created = { ref = refs.make(env.epoch(), "track", plan.index), kind = "track" },
    display = { title = title, before = { tracks = plan.count }, after = { tracks = rows.track_count(song), index = plan.index }, note = parents[plan.index] and ("It is in the group " .. track_label(parents[plan.index], song:track(parents[plan.index]))) or nil },
    undo = { desc = desc, entry = true },
    prior = { tracks = plan.count }, place = { track = plan.index }, shape = "made",
  }
end

local function add_restore(song, env, restore, desc)
  local created = restore.created
  if type(created) ~= "table" or type(created.ref) ~= "string" then bad("created: the track the change made") end
  local index = refs.expect(created.ref, "track", 1, env.epoch())[1]
  local track = base.track_at(song, index)
  local applied = restore.applied
  if type(applied) ~= "table" then bad("applied: what the change wrote") end
  if (applied.id and base.track_id(track) ~= applied.id) or (not applied.id and track.name ~= applied.name)
    or rows.track_type(track) ~= applied.kind then
    fail("changed", track_label(index, track) .. " is not the track Kumi made any more")
  end
  if is_group(track) and #track.members > 0 then
    fail("changed", "tracks were put in the group " .. track_label(index, track) .. " since: Kumi won't delete them")
  end
  if not restore.discard and not untouched(song, index, track) then
    fail("changed", track_label(index, track) .. " has devices or lines in it since Kumi made it")
  end
  song:describe_undo(desc)
  song:delete_track_at(index)
  base.note_restored(env.journal, "track-add", { track = index })
  return {
    restored = { deleted = true },
    display = { title = string.format("Removed %s %s (put back)", kind_word(applied.kind), q(track_label(index, track))) },
    undo = { desc = desc, entry = true },
  }
end

base.register({
  kind = "track-add", custom = true, shape = "made",
  preview = add_preview, apply = add_apply, restore = add_restore,
})

-- track-move: a sequencer or send track goes to another place among the tracks it can pass: those with its own parent (the
-- same group, or none) and no group of their own, since a swap past a group's tracks would change who is in it.

local function movable_range(song, index, track)
  local kind = rows.track_type(track)
  if kind == "master" then bad("the master track stays last") end
  if kind == "group" then bad("a group is moved in Renoise; Kumi moves sequencer and send tracks") end
  local parents = rows.group_parents(song)
  local function ok(j)
    local other = song:track(j)
    return rows.track_type(other) == kind and parents[j] == parents[index]
  end
  local low, high = index, index
  while low > 1 and ok(low - 1) do low = low - 1 end
  while high < rows.track_count(song) and ok(high + 1) do high = high + 1 end
  return low, high
end

local function move_wanted(song, index, track, input)
  local low, high = movable_range(song, index, track)
  local to = input.index
  if type(to) ~= "number" or to ~= math.floor(to) or to < low or to > high then
    bad(string.format("index must be a whole number from %d to %d: %s can't pass the other tracks or groups", low, high, track_label(index, track)))
  end
  return to
end

local function move_preview(song, env, input)
  local index, track = base.find(song, env, input.ref, "track-move")
  local to = move_wanted(song, index, track, input)
  local display = { title = string.format("Moved track %s to place %d", q(track_label(index, track)), to), before = { index = index }, after = { index = to } }
  if to == index then display.unchanged = true end
  return { target = base.target(env, index), prior = { index = index }, fence = signature(index, track), display = display }
end

local function group_counts(song)
  local counts = {}
  for i = 1, rows.track_count(song) do
    local track = song:track(i)
    if is_group(track) then counts[i] = #track.members end
  end
  return counts
end

-- The track at `from` goes to `to` by neighbours' swaps. A track that a swap leaves just before an empty group is made one of
-- its tracks by Renoise itself (measured, 3.5.4: not when the group has tracks, nor when the swap passes the group), which no
-- move of Kumi's means: each such membership is taken off again, the first of the group's tracks leaving it where it stands.
local function swaps(song, from, to)
  local step = to > from and 1 or -1
  for i = from, to - step, step do
    local before = group_counts(song)
    song:swap_tracks_at(i, i + step)
    for group, count in pairs(group_counts(song)) do
      if before[group] and count == before[group] + 1 then song:remove_track_from_group(group - count) end
    end
  end
end

local function move_apply(song, env, input, fence, desc)
  local ok, index, track = pcall(base.find, song, env, input.ref, "track-move")
  if not ok then base.gone_is_changed(index) end
  local now = signature(index, track)
  if not base.same_table(now, fence) then fail("changed", track_label(index, track) .. " changed since the preview") end
  local to = move_wanted(song, index, track, input)
  local name = track_label(index, track)
  local display = { title = string.format("Moved track %s to place %d", q(name), to), before = { index = index }, after = { index = to } }
  if to == index then
    display.unchanged = true
    return {
      applied = { index = to, signature = now }, material = { track = to, back = { track = index } }, display = display,
      undo = { desc = desc, entry = false }, prior = { index = index },
    }
  end
  song:describe_undo(desc)
  swaps(song, index, to)
  local after = signature(to, song:track(to))
  if after.name ~= now.name or after.kind ~= now.kind then fail("failed", "Renoise didn't move " .. name) end
  return {
    -- (the places go back under `track`, which the tool carries through the tracks made and deleted since)
    applied = { index = to, signature = after }, material = { track = to, back = { track = index } }, display = display,
    target = base.target(env, to), undo = { desc = desc, entry = true }, prior = { index = index }, place = { track = to }, follows = "track",
  }
end

local function move_restore(song, env, restore, desc)
  local material, applied, prior = restore.material, restore.applied, restore.prior
  if type(material) ~= "table" or type(material.track) ~= "number" then bad("material: where the track is") end
  if type(applied) ~= "table" or type(prior) ~= "table" or type(applied.signature) ~= "table" then bad("prior and applied: the places the track moved between") end
  local at = material.track
  local track = base.track_at(song, at)
  local function is_it(candidate)
    if candidate < 1 or candidate > rows.track_count(song) then return false end
    local other = song:track(candidate)
    return base.track_id(other) == applied.signature.name and rows.track_type(other) == applied.signature.kind
  end
  local from = type(material.back) == "table" and material.back.track or prior.index
  if not is_it(at) then
    fail(restore.discard and "gone" or "changed", "the track isn't where Kumi moved it to any more: it was moved or renamed in Renoise since")
  end
  if from == at then
    return { restored = { index = from }, display = { title = "The track is at place " .. from .. " already" }, undo = { desc = desc, entry = false } }
  end
  local low, high = movable_range(song, at, track)
  if from < low or from > high then fail("changed", "the tracks between were changed in Renoise: Kumi can't move the track back past them") end
  song:describe_undo(desc)
  swaps(song, at, from)
  base.note_restored(env.journal, "track-move", { track = from }, applied, base.KINDS["track-move"])
  return { restored = { index = from }, display = { title = string.format("Moved track %s back to place %d", q(track_label(from, song:track(from))), from) }, undo = { desc = desc, entry = true } }
end

base.register({
  kind = "track-move", custom = true,
  preview = move_preview, apply = move_apply, restore = move_restore,
  same = function(a, b) return a.index == b.index end,
})

-- track-group: a sequencer track before a group goes into it (last among its tracks), or one of its tracks comes out of it
-- (to just before its tracks). Both are put back by the other: Renoise moves the track by swaps either way, and the tracks
-- it passed keep their order.

local function group_wanted(song, env, index, track, input)
  if rows.track_type(track) ~= "sequencer" then bad("only a sequencer track is put into a group or taken out of one") end
  local parents = rows.group_parents(song)
  local group = parents[index]
  local function flat(target)
    local holder = song:track(target)
    if parents[target] then bad("the group " .. track_label(target, holder) .. " is inside another group: Kumi works with groups at the top") end
    for j = target - #holder.members, target - 1 do
      if is_group(song:track(j)) then bad("the group " .. track_label(target, holder) .. " holds another group: Kumi works with groups at the top") end
    end
    return target - #holder.members
  end
  if input.group == json.null then
    if not group then bad(track_label(index, track) .. " is not in a group") end
    return { into = false, group = group, run_start = flat(group) }
  end
  if input.group == nil then bad("group: the ref of a group track, or null to take the track out of its group") end
  if group then bad(track_label(index, track) .. " is in a group already: take it out first") end
  local target = refs.expect(input.group, "track", 1, env.epoch())[1]
  local holder = base.track_at(song, target)
  if not is_group(holder) then bad("group must be the ref of a group track") end
  local run_start = flat(target)
  if index >= run_start then bad(track_label(index, track) .. " is not before the group's tracks: only a track before them goes in (it can be put back there)") end
  for j = index + 1, run_start - 1 do
    if is_group(song:track(j)) or parents[j] then bad("a group or its tracks are between " .. track_label(index, track) .. " and the group: move it next to them first") end
  end
  return { into = true, group = target, run_start = run_start }
end

local function group_title(name, holder_name, into)
  if into then return string.format("Put %s into the group %s", q(name), q(holder_name)) end
  return string.format("Took %s out of the group %s", q(name), q(holder_name))
end

local function group_preview(song, env, input)
  local index, track = base.find(song, env, input.ref, "track-group")
  local want = group_wanted(song, env, index, track, input)
  local holder = song:track(want.group)
  local now = signature(index, track)
  now.group = want.into and 0 or want.group
  local display = { title = group_title(track_label(index, track), track_label(want.group, holder), want.into) }
  display.before = { group = want.into and json.null or track_label(want.group, holder) }
  display.after = { group = want.into and track_label(want.group, holder) or json.null }
  local prior = { index = index, group = want.into and 0 or want.group }
  if not want.into then prior.ordinal = index - want.run_start + 1 end
  return { target = base.target(env, index), prior = prior, fence = now, display = display }
end

local function group_apply(song, env, input, fence, desc)
  local ok, index, track = pcall(base.find, song, env, input.ref, "track-group")
  if not ok then base.gone_is_changed(index) end
  local want = group_wanted(song, env, index, track, input)
  local now = signature(index, track)
  now.group = want.into and 0 or want.group
  if not base.same_table(now, fence) then fail("changed", track_label(index, track) .. " changed since the preview") end
  local holder = song:track(want.group)
  local holder_name = track_label(want.group, holder)
  local name = track_label(index, track)
  song:describe_undo(desc)
  local final
  local ordinal = not want.into and index - want.run_start + 1 or nil
  if want.into then
    song:add_track_to_group(index, want.group)
    final = want.group - 1
  else
    song:remove_track_from_group(index)
    final = want.run_start
  end
  local parents = rows.group_parents(song)
  local placed = song:track(final)
  if placed.name ~= track.name or (parents[final] ~= nil) ~= want.into or (want.into and parents[final] ~= want.group) then
    fail("failed", "Renoise didn't " .. (want.into and "put " or "take ") .. name .. (want.into and " into " or " out of ") .. "the group")
  end
  local after = signature(final, placed)
  return {
    applied = { index = final, signature = after, into = want.into },
    material = { track = final, groupRef = refs.make(env.epoch(), "track", want.group), back = { track = index } },
    display = { title = group_title(name, holder_name, want.into) }, target = base.target(env, final),
    undo = { desc = desc, entry = true },
    prior = { index = index, group = want.into and 0 or want.group, ordinal = ordinal },
    place = { track = final }, follows = final ~= index and "track" or nil,
  }
end

local function group_restore(song, env, restore, desc)
  local material, applied, prior = restore.material, restore.applied, restore.prior
  if type(material) ~= "table" or type(material.track) ~= "number" or type(material.groupRef) ~= "string" then bad("material: where the track and its group are") end
  if type(applied) ~= "table" or type(prior) ~= "table" or type(applied.signature) ~= "table" then bad("prior and applied: what the change did") end
  local at = material.track
  local track = base.track_at(song, at)
  local group = refs.expect(material.groupRef, "track", 1, env.epoch())[1]
  local holder = base.track_at(song, group)
  if base.track_id(track) ~= applied.signature.name or rows.track_type(track) ~= applied.signature.kind or not is_group(holder) then
    fail("changed", "the track isn't where Kumi left it: it was moved or renamed in Renoise since")
  end
  local parents = rows.group_parents(song)
  local name = track_label(at, track)
  local holder_name = track_label(group, holder)
  if applied.into then
    -- put back: out of the group (Renoise moves it to just before the group's tracks), then back to where it was
    if parents[at] ~= group then fail("changed", name .. " is not in the group " .. holder_name .. " any more") end
    local run_start = group - #holder.members
    local from = type(material.back) == "table" and material.back.track or prior.index
    if from > run_start then fail("changed", "the tracks before the group changed since: Kumi can't put the track back") end
    for j = from, run_start - 1 do
      if is_group(song:track(j)) or (parents[j] ~= nil and j ~= at) then
        fail("changed", "a group or its tracks are where the track was: Kumi can't put it back")
      end
    end
    song:describe_undo(desc)
    song:remove_track_from_group(at)
    swaps(song, run_start, from)
    base.note_restored(env.journal, "track-group", { track = from }, applied, base.KINDS["track-group"])
    return { restored = { index = from }, display = { title = group_title(name, holder_name, false) .. " (put back)" }, undo = { desc = desc, entry = true } }
  end
  -- taken out: back in as the last of its group's tracks, then to its place among them
  if parents[at] ~= nil then fail("changed", name .. " is in a group now") end
  local run_start = group - #holder.members
  if at ~= run_start - 1 then fail("changed", name .. " is not just before the group's tracks any more") end
  local ordinal = prior.ordinal or (#holder.members + 1)
  song:describe_undo(desc)
  song:add_track_to_group(at, group)
  swaps(song, group - 1, at + ordinal - 1)
  base.note_restored(env.journal, "track-group", { track = at + ordinal - 1 }, applied, base.KINDS["track-group"])
  return { restored = { index = at + ordinal - 1 }, display = { title = group_title(name, holder_name, true) .. " (put back)" }, undo = { desc = desc, entry = true } }
end

base.register({
  kind = "track-group", custom = true,
  preview = group_preview, apply = group_apply, restore = group_restore,
  same = function(a, b) return a.index == b.index and a.into == b.into end,
})

return M
