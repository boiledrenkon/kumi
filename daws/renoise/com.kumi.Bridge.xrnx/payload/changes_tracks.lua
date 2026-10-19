-- Kumi's changes to a track: its volume, pan, mute, solo, name and colour; and deleting it, with what undo needs to make it
-- again (see changes_base.lua for how a change goes).

local import = ...
local json = import("json")
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")
local material_ = import("changes_material")

local fail, bad, the_song = base.fail, base.bad, base.the_song
local close, db_of, hex_of, rgb_of = base.close, base.db_of, base.hex_of, base.rgb_of
local find, is_master, named, target, stage_of = base.find, base.is_master, base.named, base.target, base.stage_of
local same_table = base.same_table
local NOT_UNDOABLE = base.NOT_UNDOABLE

local M = {}

-- The track properties

-- A track property is found by the track's ref, and found again by its place.
local function track_reread(song, place)
  if place.track >= 1 and place.track <= rows.track_count(song) then return song:track(place.track) end
  return nil
end

local function prop(spec)
  spec.locate = function(song, env, input)
    local index, track = find(song, env, input.ref, spec.kind)
    if is_master(track) and not spec.master then bad("the master track has no " .. spec.noun) end
    return { object = track, name = named(index, track), place = { track = index }, target = target(env, index), id = base.track_id(track) }
  end
  spec.reread = track_reread
  if spec.watch then
    spec.watch = function(env, loc) env.watch_params(loc.place.track) end
  end
  base.register(spec)
end

prop {
  kind = "track-volume", noun = "volume", master = true, undoable = true, watch = true,
  read = function(track, source)
    local stage = stage_of(source)
    local value = (stage == "post" and track.postfx_volume or track.prefx_volume).value
    return { stage = stage, volume = value, volumeDb = db_of(value) }
  end,
  wanted = function(track, input)
    local stage = stage_of(input)
    local db = input.volumeDb
    if type(db) ~= "number" or db ~= db then bad("volumeDb must be a number") end
    local parameter = stage == "post" and track.postfx_volume or track.prefx_volume
    local top = db_of(parameter.value_max) or 3
    if db < -150 or db > top + 1e-6 then bad(string.format("volumeDb must be from -150 (silent) to %.1f", top)) end
    local value = db <= -150 and 0 or math.min(10 ^ (db / 20), parameter.value_max)
    return { stage = stage, volume = value, volumeDb = db_of(value) }
  end,
  write = function(track, state)
    (state.stage == "post" and track.postfx_volume or track.prefx_volume).value = state.volume
  end,
  same = function(a, b) return a.stage == b.stage and close(a.volume, b.volume) end,
  title = function(name, _, to) return name .. " volume " .. base.db_text(to.volumeDb) .. " dB" end,
}

prop {
  kind = "track-pan", noun = "pan", master = true, undoable = true, watch = true,
  read = function(track, source)
    local stage = stage_of(source)
    local value = (stage == "post" and track.postfx_panning or track.prefx_panning).value
    return { stage = stage, pan = value * 2 - 1 }
  end,
  wanted = function(_, input)
    local stage = stage_of(input)
    local pan = input.pan
    if type(pan) ~= "number" or pan ~= pan or pan < -1 or pan > 1 then bad("pan must be a number from -1 (left) to 1 (right)") end
    return { stage = stage, pan = pan }
  end,
  write = function(track, state)
    (state.stage == "post" and track.postfx_panning or track.prefx_panning).value = (state.pan + 1) / 2
  end,
  same = function(a, b) return a.stage == b.stage and close(a.pan, b.pan) end,
  title = function(name, _, to) return name .. " pan " .. base.pan_text(to.pan) end,
}

prop {
  kind = "track-mute", noun = "mute", master = false, undoable = false,
  read = function(track)
    local state = track.mute_state
    return { mute = state ~= renoise.Track.MUTE_STATE_ACTIVE, muteState = state }
  end,
  wanted = function(_, input)
    if type(input.mute) ~= "boolean" then bad("mute must be true or false") end
    local T = renoise.Track
    local state = input.mute and T.MUTE_STATE_MUTED or T.MUTE_STATE_ACTIVE
    return { mute = input.mute, muteState = state }
  end,
  write = function(track, state) track.mute_state = state.muteState end,
  same = function(a, b) return a.muteState == b.muteState end,
  title = function(name, _, to) return name .. (to.mute and " muted" or " unmuted") end,
}

prop {
  kind = "track-solo", noun = "solo", master = false, undoable = false,
  read = function(track) return { solo = track.solo_state == true } end,
  wanted = function(_, input)
    if type(input.solo) ~= "boolean" then bad("solo must be true or false") end
    return { solo = input.solo }
  end,
  write = function(track, state) track.solo_state = state.solo end,
  same = function(a, b) return a.solo == b.solo end,
  title = function(name, _, to) return name .. (to.solo and " soloed" or " unsoloed") end,
}

prop {
  kind = "track-rename", noun = "name", master = false, undoable = true,
  read = function(track) return { name = track.name } end,
  wanted = function(_, input)
    local name = input.name
    if type(name) ~= "string" then bad("name must be text") end
    if #name > 1024 or name:find("[%c]") then bad("name must be one line of text") end
    return { name = name }
  end,
  write = function(track, state) track.name = state.name end,
  same = function(a, b) return a.name == b.name end,
  title = function(_, from, to)
    if from.name == "" then return "Named track " .. base.q(to.name) end
    return "Renamed track " .. base.q(from.name) .. " \226\134\146 " .. base.q(to.name)
  end,
}

prop {
  kind = "track-color", noun = "colour", master = true, undoable = true,
  read = function(track) return { color = hex_of(track.color), colorBlend = track.color_blend } end,
  wanted = function(track, input)
    local color = rgb_of(input.color)
    if not color then bad("color must be #rrggbb") end
    local blend = input.colorBlend
    if blend == nil then
      blend = track.color_blend
    elseif type(blend) ~= "number" or blend ~= math.floor(blend) or blend < 0 or blend > 100 then
      bad("colorBlend must be a whole number from 0 to 100")
    end
    return { color = hex_of(color), colorBlend = blend }
  end,
  write = function(track, state)
    track.color = rgb_of(state.color)
    track.color_blend = state.colorBlend
  end,
  same = function(a, b) return a.color == b.color and a.colorBlend == b.colorBlend end,
  title = function(name) return name .. " colour changed" end,
}


-- Deleting a track, and making it again

local function lines_text(count) return count == 1 and "1 line" or count .. " lines" end

-- What tells a track apart from what the preview saw: its place, name and what it holds.
local function signature(song, index, track)
  local notes = 0
  for p = 1, #song.patterns do
    local pattern_track = song:pattern(p):track(index)
    if not pattern_track.is_empty then notes = notes + 1 end
  end
  return {
    name = track.name,
    index = index,
    kind = rows.track_type(track),
    devices = math.max(#track.devices - 1, 0),
    patternsWithLines = notes,
    noteColumns = track.visible_note_columns,
    effectColumns = track.visible_effect_columns,
  }
end

local function find_to_delete(song, env, input)
  local index, track = find(song, env, input.ref, "track-delete")
  local kind = rows.track_type(track)
  if kind == "master" then bad("the master track can't be deleted") end
  if kind == "group" then bad("a group track is deleted with its members; delete the group in Renoise") end
  return index, track, kind
end

local function delete_preview(song, env, input)
  local index, track = find_to_delete(song, env, input)
  local now = signature(song, index, track)
  local display = { title = "Deleted track " .. base.q(named(index, track)) }
  if now.devices > 0 or now.patternsWithLines > 0 then
    display.note = string.format("It holds %d devices and lines in %d patterns; Kumi keeps them to put back.", now.devices, now.patternsWithLines)
  end
  return { target = target(env, index), prior = now, fence = now, display = display }
end

-- The writes that make a track again, in phases. A phase is one callback; the first makes the track, its properties, devices,
-- columns and slot mutes; the rest write lines (and the automation of those patterns), up to SLICE_WRITES each.
local function plan_phases(material)
  local phases, current, writes = {}, {}, 0
  for _, entry in ipairs(material.patterns or {}) do
    local cost = entry.writes or 1
    if writes > 0 and writes + cost > base.SLICE_WRITES then
      phases[#phases + 1] = current
      current, writes = {}, 0
    end
    current[#current + 1] = entry
    writes = writes + cost
  end
  phases[#phases + 1] = current
  return phases
end

local function gather(song, env, index, track, kind)
  local sequencer = song.sequencer
  local material = {
    index = index, kind = kind, name = track.name, color = hex_of(track.color), colorBlend = track.color_blend,
    muteState = track.mute_state, solo = track.solo_state == true, collapsed = track.collapsed,
    volume = { pre = track.prefx_volume.value, post = track.postfx_volume.value },
    panning = { pre = track.prefx_panning.value, post = track.postfx_panning.value },
    width = track.prefx_width.value,
    outputRouting = track.output_routing, outputDelay = track.output_delay,
    columns = {
      note = track.visible_note_columns, effect = track.visible_effect_columns,
      volume = track.volume_column_visible, panning = track.panning_column_visible, delay = track.delay_column_visible,
      sampleEffects = track.sample_effects_column_visible, names = {}, muted = {},
    },
    devices = {}, automation = {}, patterns = {}, slotMutes = {},
  }
  for c = 1, track.max_note_columns do
    local name = track:column_name(c)
    if name ~= "" then material.columns.names[tostring(c)] = name end
    if track:column_is_muted(c) then material.columns.muted[#material.columns.muted + 1] = c end
  end
  local parents = rows.group_parents(song)
  if parents[index] then
    -- (the group comes after its tracks, so it is one place up once this track is gone: `at` is where the shifts since find it)
    material.group = { at = parents[index] - 1, name = song:track(parents[index]).name }
  end
  for d = 2, #track.devices do material.devices[#material.devices + 1] = material_.read_device(track:device(d), song, index, true, env) end
  material.automation = material_.read_automation(song, index, track)
  local writes, limit = 0, base.MAX_RESTORE_STEPS * base.SLICE_WRITES
  for p = 1, #song.patterns do
    local pattern_track = song:pattern(p):track(index)
    if pattern_track.is_alias then
      material.patterns[#material.patterns + 1] = { pattern = p, alias = pattern_track.alias_pattern_index }
    elseif not pattern_track.is_empty then
      local lines, cost = material_.read_lines(pattern_track, song:pattern(p).number_of_lines)
      if #lines > 0 then material.patterns[#material.patterns + 1] = { pattern = p, lines = lines, writes = cost } end
      writes = writes + cost
      -- (past what can be put back in time there is no point in reading on)
      if writes > limit then
        fail("failed", string.format("%s holds more than Kumi can put back in time (over %d writes), so it won't delete it", named(index, track), limit))
      end
    end
  end
  -- A pattern with automation of this track and no lines still has to be written: it gets an entry of its own.
  local with_entry = {}
  for _, entry in ipairs(material.patterns) do with_entry[entry.pattern] = true end
  for _, saved in ipairs(material.automation) do
    if not with_entry[saved.pattern] then
      with_entry[saved.pattern] = true
      material.patterns[#material.patterns + 1] = { pattern = saved.pattern, lines = {}, writes = 1 }
    end
  end
  table.sort(material.patterns, function(a, b) return a.pattern < b.pattern end)
  -- The patterns, the instruments the notes play and the slots it is muted in are numbers that move: each is kept with what
  -- finds it again (see places.lua and the restore).
  local numbers, instruments = {}, {}
  for _, entry in ipairs(material.patterns) do
    numbers[entry.pattern] = true
    if entry.alias then numbers[entry.alias] = true end
    for _, row in ipairs(entry.lines or {}) do
      for _, cell in ipairs(row.notes) do
        if cell.instrument ~= 255 then instruments[cell.instrument] = true end
      end
    end
  end
  material.pool = material_.keep_pool(song, env, numbers)
  material.instruments = material_.keep_instruments(song, env, instruments)
  for s = 1, #sequencer.pattern_sequence do
    if sequencer:track_sequence_slot_is_muted(index, s) then material.slotMutes[#material.slotMutes + 1] = s end
  end
  if #material.slotMutes > 0 then
    material.sequence = {}
    for s = 1, #sequencer.pattern_sequence do material.sequence[s] = sequencer.pattern_sequence[s] end
  end
  material.writes = writes
  if kind == "send" or kind == "sequencer" then material.routes = material_.read_routes(song, index, nil, env) end
  return material
end

local function delete_apply(song, env, input, fence, desc)
  local ok, index, track, kind = pcall(find_to_delete, song, env, input)
  if not ok then base.gone_is_changed(index) end
  local now = signature(song, index, track)
  if not same_table(now, fence) then
    -- (another track at the ref's place is not the one that was previewed: name that one)
    if type(fence.name) == "string" and (fence.name ~= now.name or fence.kind ~= now.kind) then fail("changed", base.moved_text(fence.name)) end
    fail("changed", named(index, track) .. " changed since the preview")
  end
  local name = named(index, track)
  local material = gather(song, env, index, track, kind)
  local size = #json.encode(material)
  if size > base.MAX_MATERIAL_BYTES then
    fail("failed", string.format("%s holds %.1f MB, more than Kumi can keep to put it back, so it won't delete it", name, size / 1048576))
  end
  local steps = #plan_phases(material)
  if steps > base.MAX_RESTORE_STEPS then
    fail("failed", string.format("%s would take %d steps to put back, more than Kumi takes (%d): it couldn't be undone in time, so it won't delete it",
      name, steps, base.MAX_RESTORE_STEPS))
  end
  song:describe_undo(desc)
  song:delete_track_at(index)
  return {
    applied = { deleted = true },
    material = material,
    display = { title = "Deleted track " .. base.q(name), note = material_.routes_note(#(material.routes or {})) }, target = target(env, index),
    undo = { desc = desc, entry = true },
    prior = { deleted = false }, place = { track = index }, shape = "deleted",
  }
end

-- What Renoise lets a track of this kind have: a send track has no output delay (it raises), no note columns, and the other
-- settings are tried and left as they are if Renoise refuses (they are looks, not content).
local function restore_properties(song, track, index, material)
  track.name = material.name
  local color = rgb_of(material.color)
  if color then track.color = color end
  track.color_blend = material.colorBlend
  track.prefx_volume.value = material.volume.pre
  track.postfx_volume.value = material.volume.post
  track.prefx_panning.value = material.panning.pre
  track.postfx_panning.value = material.panning.post
  local function try(write) pcall(write) end
  try(function() track.prefx_width.value = material.width end)
  if material.outputRouting then try(function() track.output_routing = material.outputRouting end) end
  if material.kind == "sequencer" and type(material.outputDelay) == "number" then
    try(function() track.output_delay = material.outputDelay end)
  end
  try(function() track.collapsed = material.collapsed == true end)
  local columns = material.columns or {}
  if track.max_note_columns > 0 then
    track.visible_note_columns = columns.note
    try(function() track.volume_column_visible = columns.volume == true end)
    try(function() track.panning_column_visible = columns.panning == true end)
    try(function() track.delay_column_visible = columns.delay == true end)
    try(function() track.sample_effects_column_visible = columns.sampleEffects == true end)
  end
  try(function() track.visible_effect_columns = columns.effect end)
  for c, name in pairs(columns.names or {}) do try(function() track:set_column_name(tonumber(c), name) end) end
  for _, c in ipairs(columns.muted or {}) do try(function() track:set_column_is_muted(c, true) end) end
  if material.solo and rows.track_type(track) ~= "master" then try(function() track.solo_state = true end) end
  if material.muteState and rows.track_type(track) ~= "master" then try(function() track.mute_state = material.muteState end) end
end

-- Where the track goes back: its old place, moved by the tracks made and deleted since. A track made at or before the master's
-- place is a sequencer track (the master moves up); one made after it is a send track (measured in Renoise 3.5.4: inserting AT
-- the master's place makes a sequencer track, not a send).
local function restore_index(song, material)
  local master = song.sequencer_track_count + 1
  if material.kind == "send" then
    return math.max(master + 1, math.min(material.index, rows.track_count(song) + 1))
  end
  return math.max(1, math.min(material.index, master))
end

local function make_track(song, material)
  local index = restore_index(song, material)
  song:insert_track_at(index)
  return index
end

-- A member of a group goes back into it. The group is the one found where it was (`group.at`, carried through the tracks made, deleted
-- and swapped since by `moved`): groups are numbered by place ("Group 02"), so a name is no identity. Renoise makes a track inserted
-- just before a group (but not before its first member) a member by itself; otherwise it is added, which puts it last among the
-- members, and moved to its own place by swaps. The track is at `index` afterwards, in the group or not. Answers a note when it
-- stayed outside, and why.
-- Whether group `g` is `ancestor` or inside it.
local function within(parents, g, ancestor)
  while g do
    if g == ancestor then return true end
    g = parents[g]
  end
  return false
end

local function put_in_group(song, env, index, material, moved)
  if not material.group then return nil end
  local count = rows.track_count(song)
  local parents = rows.group_parents(song)
  local group = type(material.group.at) == "number" and moved(material.group.at, index) or nil
  -- (a group comes after its members: one before the track, or not a group, is not the one it was in. Put back at its place, the
  -- track may be in a group around the one it was in: the first member of a group inside another group is.)
  if not group or group <= index or group > count or song:track(group).type ~= renoise.Track.TRACK_TYPE_GROUP then
    if parents[index] then error({ code = "failed", message = "Renoise put the track into a group it wasn't in" }, 0) end
    return string.format("The group %s it was in isn't there any more, so it is back outside any group.", base.q(material.group.name))
  end
  if parents[index] == group then return nil end
  if parents[index] and not within(parents, group, parents[index]) then
    error({ code = "failed", message = "Renoise put the track into a group it wasn't in" }, 0)
  end
  local before = env.layout()
  song:add_track_to_group(index, group)
  -- Renoise moves it by swaps, which the layout's shifts follow: it is found where they took it, then swapped back to its place.
  local at = env.shifted(index, before)
  if not at then error({ code = "failed", message = "Renoise moved the track into its group and Kumi lost where it went" }, 0) end
  while at > index do
    song:swap_tracks_at(at, at - 1)
    at = at - 1
  end
  if rows.group_parents(song)[index] ~= group or env.shifted(index, before) ~= index then
    error({ code = "failed", message = "Renoise didn't put the track back into its group" }, 0)
  end
  return nil
end

-- Phase one, in the callback that made the track. Nothing half-done stays: a track that couldn't be finished is deleted again, where
-- it is (followed through the swaps this callback made itself). Answers the track's place and what to say about where it went.
-- Whether a slot still holds the place it had (Renoise's sequence notifier can't say which slot moved where). It does when the
-- sequence is the one the record saw (its patterns found where they are now, by `pattern_now`); or when the sequence up to it
-- is, and its pattern is none of its neighbours': patterns repeat, and a slot made or deleted before a run of one pattern
-- leaves the sequence up to it as it was. A slot made or deleted after it doesn't move it.
local function slot_now(kept, now, s, pattern_now)
  if type(kept) ~= "table" or s > #kept then return nil end
  local then_ = {}
  for k = 1, #kept do then_[k] = pattern_now(kept[k]) or false end
  return base.follow_slot(then_, now, s)
end

local function add_note(note, more)
  if not more then return note end
  return note and (note .. " " .. more) or more
end

local function restore_first_phase(song, env, material, phases, moved)
  local index = make_track(song, material)
  local made, since = index, env.layout()
  local note
  local tally = material_.new_tally()
  local ok, err = pcall(function()
    local track = song:track(index)
    restore_properties(song, track, index, material)
    for i, saved in ipairs(material.devices or {}) do material_.restore_device(track, i + 1, saved) end
    note = put_in_group(song, env, index, material, moved)
    -- (where a meta device sends its values: after the track has its place, and its devices are all there)
    for i, saved in ipairs(material.devices or {}) do
      if saved.destination then material_.count(tally, material_.restore_destination(song, song:track(index):device(i + 1), saved, index, moved, env)) end
    end
    local sequencer = song.sequencer
    local lost = 0
    local now = {}
    for k = 1, #sequencer.pattern_sequence do now[k] = sequencer.pattern_sequence[k] end
    for _, s in ipairs(material.slotMutes or {}) do
      local at = slot_now(material.sequence, now, s, material.pattern_now)
      if at then
        sequencer:set_track_sequence_slot_is_muted(index, at, true)
      else
        lost = lost + 1
      end
    end
    if lost > 0 then
      note = add_note(note, string.format("Its mute in %s isn't put back: the sequence changed in Renoise since in a way that doesn't say where %s went.",
        lost == 1 and "1 slot" or (lost .. " slots"), lost == 1 and "that slot" or "those slots"))
    end
    local done = {}
    for _, entry in ipairs(phases[1]) do
      material_.write_lines(song, index, entry, material.instrument_map)
      done[entry.pattern] = true
    end
    material_.write_automation(song, song:track(index), index, material.automation, done)
    material_.write_routes(song, material.routes, { track = index }, moved, tally, env)
    note = add_note(note, material_.tally_note(tally))
    note = add_note(note, material.lost_note)
  end)
  if not ok then
    local at = (env.shifted(made, since))
    if at and at >= 1 and at <= rows.track_count(song) then
      pcall(function() song:delete_track_at(at) end)
    else
      err = { code = "uncertain", message = "Putting the track back stopped and Kumi can't tell where the half-made one is: look for it" }
    end
    if type(err) ~= "table" then err = { code = "failed", message = "Renoise wouldn't put the track back: " .. tostring(err):gsub("^[^%s:]*%.lua:%d+: ", "") } end
    error(err, 0)
  end
  return index, note
end

-- Where a track numbered as the song was after the delete (layout `since`) is now, with the deleted track put back at `inserted`
-- (the layout already counts that: it is made in this callback). Nil when it is gone.
local function moved_by(env, since)
  return function(at, inserted)
    if type(since) ~= "number" then return inserted and at >= inserted and at + 1 or at end
    return (env.shifted(at, since))
  end
end

local function restored_answer(env, song, index, material, note)
  local track = song:track(index)
  local display = { title = base.q(named(index, track)) .. " put back", note = note }
  return {
    restored = { ref = refs.make(env.epoch(), "track", index), name = track.name },
    display = display,
  }
end

-- A track with this name and kind is at the place already: it came back some other way, or was never gone. A name Renoise numbers
-- itself ("Track 03", "S01") tells nothing (the next track takes it when this one goes): then the same things on the track do, and
-- an empty track can't be told from another empty one, so it is made again.
local function already_there(song, material, prior)
  local top = rows.track_count(song)
  local want = base.name_id(material.kind, material.name)
  for _, at in ipairs({ material.index, material.indexThen or material.index }) do
    local track = at >= 1 and at <= top and song:track(at) or nil
    if track ~= nil and rows.track_type(track) == material.kind and base.track_id(track) == want then
      if not want:match(":$") then return true end
      if type(prior) == "table" and (prior.devices or 0) + (prior.patternsWithLines or 0) > 0 then
        local now = signature(song, at, track)
        if now.devices == prior.devices and now.patternsWithLines == prior.patternsWithLines and now.noteColumns == prior.noteColumns
          and now.effectColumns == prior.effectColumns then
          return true
        end
      end
    end
  end
  return false
end

-- The record's patterns and instruments where they are now (see places.lua): a copy of the material to write from (with
-- `pattern_now` for the sequence, `instrument_map` and `lost_note`), or a refusal that touches nothing when where they are
-- can't be told. Lines in a pattern deleted since, or one that isn't what it was (its name or length changed), aren't put back,
-- and the answer says so.
local function placed(song, env, material, discard)
  local q = base.q
  local pattern_now, follow = material_.pool_now(song, env, material.pool)
  if not pattern_now then
    fail("stale", string.format("the song's patterns changed in a way Kumi couldn't follow since %s was deleted, so it can't tell where its lines go: nothing was put back", q(material.name)))
  end
  local map, why = material_.instruments_now(song, env, material.instruments)
  if not map then
    if why == "unknown" then
      fail("stale", string.format("the instruments changed in a way Kumi couldn't follow since %s was deleted, so it can't tell which its notes play: nothing was put back", q(material.name)))
    end
    if not discard then
      fail("changed", string.format("the instrument %s that %s's notes play was deleted in Renoise since: Kumi won't put them back playing another", q(why), q(material.name)))
    end
    map = {}
  end
  local copy = {}
  for k, v in pairs(material) do copy[k] = v end
  copy.patterns, copy.automation = {}, {}
  local lost, count = {}, 0
  local function lose(p)
    if not lost[p] then lost[p], count = true, count + 1 end
  end
  for _, entry in ipairs(material.patterns or {}) do
    local p, alias = pattern_now(entry.pattern), entry.alias and pattern_now(entry.alias) or nil
    if p and (entry.alias == nil or alias) then
      local moved = {}
      for k, v in pairs(entry) do moved[k] = v end
      moved.pattern, moved.alias = p, alias
      copy.patterns[#copy.patterns + 1] = moved
    else
      lose(entry.pattern)
    end
  end
  for _, saved in ipairs(material.automation or {}) do
    local p = pattern_now(saved.pattern)
    if p then
      copy.automation[#copy.automation + 1] = { pattern = p, device = saved.device, parameter = saved.parameter, playmode = saved.playmode, points = saved.points }
    else
      lose(saved.pattern)
    end
  end
  if count > 0 then
    copy.lost_note = string.format("Its lines in %s aren't put back: %s deleted or changed in Renoise since.",
      count == 1 and "1 pattern" or (count .. " patterns"), count == 1 and "it was" or "they were")
  end
  copy.pattern_now, copy.instrument_map = follow, map
  return copy
end

local function delete_restore(song, env, restore, desc, journal)
  local material = restore.material
  if type(material) ~= "table" or type(material.name) ~= "string" then bad("material: what the delete removed") end
  material = placed(song, env, material, restore.discard)
  local phases = plan_phases(material)
  if already_there(song, material, restore.prior) and #phases >= 1 and not restore.discard then
    fail("changed", "a track with that name is in the song again")
  end
  if #phases > base.MAX_RESTORE_STEPS then
    fail("failed", string.format("that is %d steps to put back, more than Kumi takes (%d): it can't be undone in time", #phases, base.MAX_RESTORE_STEPS))
  end
  if #phases == 1 then
    song:describe_undo(desc)
    local index, note = restore_first_phase(song, env, material, phases, moved_by(env, restore.layout))
    local answer = restored_answer(env, song, index, material, note)
    answer.undo = { desc = desc, entry = true }
    base.note_restored(env.journal, "track-delete", { track = index })
    return answer
  end
  -- Too much for one callback: the track and its parts in the first, then lines, each its own numbered step.
  return {
    kumi_job = function(job)
      local total = #phases
      local function step(k) return string.format("%s (%d/%d)", desc, k, total) end
      song = the_song()
      song:describe_undo(step(1))
      local index, note = restore_first_phase(song, env, material, phases, moved_by(env, restore.layout))
      -- (from here a stop leaves a half-made track: a tool stopped or reloaded says so, see jobs.lua)
      job.wrote = string.format("Putting %s back", base.q(material.name))
      -- Phase one is one callback, so nothing else can land in it: the shape, the layout and the epoch it moved to (the devices
      -- and the swaps into a group move it) are the ones to hold to from here.
      job.rebase()
      local made = { index = index, since = env.layout(), id = base.track_id(song:track(index)) }
      -- The made track's place now (the tracks made and deleted since are followed), if that place holds a track like it.
      local function made_at()
        local at, why = env.shifted(made.index, made.since)
        if not at then return nil, why end
        local live = the_song()
        if at < 1 or at > rows.track_count(live) or base.track_id(live:track(at)) ~= made.id then return nil, "other" end
        return at
      end
      local ok, err = pcall(function()
        for k = 2, total do
          job.step(base.STEP_GAP_MS)
          base.wait_while_busy(job, env)
          local live = the_song()
          local at = made_at()
          if not at then fail("changed", "the track being put back isn't where it was made any more") end
          live:describe_undo(step(k))
          local done = {}
          for _, entry in ipairs(phases[k]) do
            material_.write_lines(live, at, entry, material.instrument_map)
            done[entry.pattern] = true
          end
          material_.write_automation(live, live:track(at), at, material.automation, done)
        end
      end)
      if not ok then
        -- Nothing half-done stays: the track this restore made goes again, if it is where it was made. When that can't be told
        -- (the producer undid or moved it, a dialog is open), Kumi deletes nothing and says so.
        local at = made_at()
        local code = type(err) == "table" and err.code or nil
        if at and not env.busy() then
          pcall(function() the_song():delete_track_at(at) end)
          job.rebase()
          local text = type(err) == "table" and err.message or tostring(err)
          error({
            code = type(err) == "table" and err.code or "failed",
            message = string.format("Putting %s back stopped (%s): the half-made track is taken out again, and nothing else was touched", base.q(material.name), text),
          }, 0)
        end
        job.rebase()
        local why = at and "a dialog is open, so Kumi left it" or "it isn't where Kumi made it any more, so Kumi deleted nothing"
        error({
          code = "uncertain",
          message = string.format("Putting %s back stopped (%s) and %s: a half-made one may be in the song, look for it", base.q(material.name),
            type(err) == "table" and err.message or tostring(err), why),
        }, 0)
      end
      local live = the_song()
      local answer = restored_answer(env, live, index, material, note)
      answer.undo = { desc = desc, entry = true, steps = total }
      answer.epoch, answer.layout = env.epoch(), env.layout()
      base.note_restored(env.journal, "track-delete", { track = index })
      return answer
    end,
  }
end

base.register({
  kind = "track-delete", custom = true, shape = "deleted",
  preview = delete_preview, apply = delete_apply, restore = delete_restore,
})

-- M4 (n-song): a track's collapse, width, output routing and delay, the columns it shows, and a note column's name and mute.
-- Each is a Renoise undo step (measured in 3.5.4, lab 2026-10-19; a column's mute is one, unlike the track's mute). A group's
-- `group_collapsed` has no observable, so Renoise's undo of it alone isn't seen: Kumi looks again at the next undo it sees.

local flag, text_of = base.flag, base.text_of
local T = renoise.Track

local function number_text(value)
  local text = string.format("%.2f", value):gsub("0+$", ""):gsub("%.$", "")
  return (text:gsub("^%-", base.MINUS))
end

prop(base.settings {
  kind = "track-collapse", noun = "collapse", master = true, undoable = true, named = true,
  fields = {
    {
      name = "collapsed", read = function(t) return t.collapsed end, write = function(t, v) t.collapsed = v end,
      check = flag, text = function(v) return v and "collapsed" or "expanded" end,
    },
    {
      name = "groupCollapsed",
      read = function(t) return t.type == T.TRACK_TYPE_GROUP and t.group_collapsed or false end,
      write = function(t, v) t.group_collapsed = v end,
      check = function(v, name, t)
        if t.type ~= T.TRACK_TYPE_GROUP then bad("groupCollapsed is a group track's (it folds the group's tracks)") end
        return flag(v, name)
      end,
      text = function(v) return v and "with its tracks folded" or "with its tracks shown" end,
    },
  },
})

prop(base.settings {
  kind = "track-width", noun = "width", master = true, undoable = true, named = true, watch = true,
  fields = {
    {
      name = "width",
      read = function(t)
        local w = t.prefx_width
        return w.value / w.value_max * 100
      end,
      write = function(t, v)
        local w = t.prefx_width
        w.value = v / 100 * w.value_max
      end,
      check = function(v)
        if type(v) ~= "number" or v ~= v or v < 0 or v > 100 then bad("width must be a number from 0 to 100 (percent, as Renoise shows it)") end
        return v
      end,
      text = function(v) return "width " .. number_text(v) .. "%" end,
    },
  },
})

-- The routings a track may take are names: the master, a group around it, the audio device's outputs. A group's name moves with
-- its place ("Group 04"), so a record keeps the group as a track ref (the layout carries it) and the name is read from it again.
local function routing_named(track, wanted)
  if type(wanted) ~= "string" or wanted == "" then bad("routing: one of the track's output routings, by name") end
  local available, found = track.available_output_routings, nil
  for _, name in ipairs(available) do
    if name == wanted then return name end
    if name:lower() == wanted:lower() then
      if found then bad(string.format("%q names more than one routing: give it as Renoise writes it", wanted)) end
      found = name
    end
  end
  if found then return found end
  bad(string.format("%s can't go to %q: its routings are %s", track.name, wanted, table.concat(available, ", ")))
end

local function group_named(song, name)
  local found
  for i = 1, rows.track_count(song) do
    local track = song:track(i)
    if track.type == T.TRACK_TYPE_GROUP and track.name == name then
      if found then return nil end
      found = i
    end
  end
  return found
end

prop {
  kind = "track-output", noun = "output routing", master = true, undoable = true,
  read = function(track) return { routing = track.output_routing } end,
  wanted = function(track, input) return { routing = routing_named(track, input.routing) } end,
  write = function(track, state) track.output_routing = state.routing end,
  same = function(a, b) return a.routing == b.routing end,
  title = function(name, _, to) return name .. " routed to " .. base.q(to.routing) end,
  wire = function(state, env)
    if type(state) ~= "table" or type(state.routing) ~= "string" then return state end
    local song = renoise.song()
    local group = song and group_named(song, state.routing)
    if not group then return state end
    return { routing = state.routing, at = refs.make(env.epoch(), "track", group) }
  end,
  unwire = function(state, env)
    if type(state) ~= "table" or type(state.at) ~= "string" then return state end
    local song = the_song()
    local group = refs.expect(state.at, "track", 1, env.epoch())[1]
    if group > rows.track_count(song) or song:track(group).type ~= T.TRACK_TYPE_GROUP then
      fail("changed", "the group " .. base.q(state.routing) .. " isn't where it was any more")
    end
    return { routing = song:track(group).name }
  end,
}

prop {
  kind = "track-delay", noun = "output delay", master = false, undoable = true,
  read = function(track) return { delayMs = track.output_delay } end,
  wanted = function(track, input)
    if track.type ~= T.TRACK_TYPE_SEQUENCER then
      bad("only a sequencer track has an output delay (Renoise refuses one on a group or a send track)")
    end
    local v = input.delayMs
    if type(v) ~= "number" or v ~= v or v < -100 or v > 100 then bad("delayMs must be a number from -100 to 100") end
    return { delayMs = v }
  end,
  write = function(track, state) track.output_delay = state.delayMs end,
  same = function(a, b) return close(a.delayMs, b.delayMs) end,
  title = function(name, _, to) return name .. " delay " .. number_text(to.delayMs) .. " ms" end,
}

-- The columns a track shows. Hiding a column hides what is in it (Renoise keeps it): the preview says so when it holds notes.
local SUBCOLUMNS = {
  { name = "volume", field = "volume_column_visible", word = "volume" },
  { name = "panning", field = "panning_column_visible", word = "panning" },
  { name = "delay", field = "delay_column_visible", word = "delay" },
  { name = "sampleEffects", field = "sample_effects_column_visible", word = "sample effects" },
}

-- (only a sequencer track has note columns; a group raises on them, measured in 3.5.4)
local function note_columns_of(track) return track.type == T.TRACK_TYPE_SEQUENCER and track.max_note_columns or 0 end

local function has_notes(track, name)
  if note_columns_of(track) == 0 then bad(name .. ": this track has no note columns (only a sequencer track has them)") end
end

local columns_fields = {
  {
    name = "noteColumns", read = function(t) return t.visible_note_columns end, write = function(t, v) t.visible_note_columns = v end,
    check = function(v, name, t)
      has_notes(t, name)
      if type(v) ~= "number" or v ~= math.floor(v) or v < 1 or v > note_columns_of(t) then bad(string.format("noteColumns is from 1 to %d", note_columns_of(t))) end
      return v
    end,
    text = function(v) return v == 1 and "1 note column" or (v .. " note columns") end,
  },
  {
    name = "effectColumns", read = function(t) return t.visible_effect_columns end, write = function(t, v) t.visible_effect_columns = v end,
    check = function(v, _, t)
      local low, high = t.min_effect_columns, t.max_effect_columns
      if type(v) ~= "number" or v ~= math.floor(v) or v < low or v > high then bad(string.format("effectColumns is from %d to %d on this track", low, high)) end
      return v
    end,
    text = function(v) return v == 1 and "1 effect column" or (v .. " effect columns") end,
  },
}
for _, sub in ipairs(SUBCOLUMNS) do
  columns_fields[#columns_fields + 1] = {
    name = sub.name, read = function(t) return note_columns_of(t) > 0 and t[sub.field] or false end, write = function(t, v) t[sub.field] = v end,
    check = function(v, name, t)
      has_notes(t, name)
      return flag(v, name)
    end,
    text = function(v) return (v and "the " or "no ") .. sub.word .. " column" end,
  }
end

-- What the track's patterns hold in the columns a change would hide: a note for the preview, or nil (and nil past what is worth
-- reading through).
local HIDDEN_READS = 40000
local function hidden_note(loc, wanted)
  local song, track, index = renoise.song(), loc.object, loc.place.track
  if not song then return nil end
  local notes_from = wanted.noteColumns and wanted.noteColumns < track.visible_note_columns and wanted.noteColumns + 1 or nil
  local effects_from = wanted.effectColumns and wanted.effectColumns < track.visible_effect_columns and wanted.effectColumns + 1 or nil
  local subs = {}
  for _, sub in ipairs(SUBCOLUMNS) do
    if wanted[sub.name] == false and note_columns_of(track) > 0 and track[sub.field] then subs[#subs + 1] = sub end
  end
  if not notes_from and not effects_from and #subs == 0 then return nil end
  local patterns, reads = 0, 0
  for p = 1, #song.patterns do
    local pattern_track = song:pattern(p):track(index)
    local found = false
    if not pattern_track.is_alias and not pattern_track.is_empty then
      for l = 1, song:pattern(p).number_of_lines do
        local line = pattern_track:line(l)
        reads = reads + 1
        if not line.is_empty then
          for c = 1, note_columns_of(track) do
            local column = line:note_column(c)
            reads = reads + 1
            if not column.is_empty then
              if notes_from and c >= notes_from then found = true end
              for _, sub in ipairs(subs) do
                if (sub.name == "volume" and column.volume_string ~= "..") or (sub.name == "panning" and column.panning_string ~= "..")
                  or (sub.name == "delay" and column.delay_string ~= "..")
                  or (sub.name == "sampleEffects" and (column.effect_number_value ~= 0 or column.effect_amount_value ~= 0)) then
                  found = true
                end
              end
            end
          end
          if effects_from then
            for c = effects_from, track.max_effect_columns do
              if not line:effect_column(c).is_empty then found = true end
            end
          end
        end
        if found or reads > HIDDEN_READS then break end
      end
    end
    if found then patterns = patterns + 1 end
    if reads > HIDDEN_READS then return nil end
  end
  if patterns == 0 then return nil end
  return string.format("What it hides holds notes in %s: Renoise keeps them, hidden (and plays them).",
    patterns == 1 and "1 pattern" or (patterns .. " patterns"))
end

local columns_spec = base.settings({
  kind = "track-columns", noun = "columns", master = true, undoable = true, named = true, fields = columns_fields,
})
columns_spec.note = function(loc, _, wanted) return hidden_note(loc, wanted) end
prop(columns_spec)

-- A note column's name and mute: the column is part of what the change is about, so the record keeps it (`col`).
local column_spec = base.settings({
  kind = "track-column", noun = "column", undoable = true, named = true,
  fields = {
    {
      name = "name", read = function(o) return o.track:column_name(o.c) end, write = function(o, v) o.track:set_column_name(o.c, v) end,
      check = function(v, name) return text_of(v, name, 64) end,
      text = function(v) return v == "" and "unnamed" or ("named " .. base.q(v)) end,
    },
    {
      name = "mute", read = function(o) return o.track:column_is_muted(o.c) end, write = function(o, v) o.track:set_column_is_muted(o.c, v) end,
      check = flag, text = function(v) return v and "muted" or "unmuted" end,
    },
  },
})
local column_fields = column_spec.read
column_spec.read = function(object, source)
  local state = column_fields(object, source)
  state.col = object.c
  return state
end
column_spec.locate = function(song, env, input)
  local index, track = find(song, env, input.ref, "track-column")
  if note_columns_of(track) == 0 then bad(named(index, track) .. " has no note columns: only a sequencer track has them") end
  local c = input.col
  if type(c) ~= "number" or c ~= math.floor(c) or c < 1 or c > note_columns_of(track) then
    bad(string.format("col is a note column from 1 to %d", note_columns_of(track)))
  end
  return {
    object = { track = track, c = c }, name = string.format("%s column %d", named(index, track), c), place = { track = index, column = c },
    target = target(env, index), id = base.track_id(track) .. "|" .. c,
  }
end
column_spec.reread = function(song, place)
  if place.track < 1 or place.track > rows.track_count(song) then return nil end
  local track = song:track(place.track)
  if place.column > note_columns_of(track) then return nil end
  return { track = track, c = place.column }
end
column_spec.restore_input = function(restore)
  return { ref = restore.target and restore.target.ref, col = type(restore.prior) == "table" and restore.prior.col or nil }
end
base.register(column_spec)

return M
