-- Shared by Kumi's changes to a Renoise song (M3; see changes.lua): the helpers, the property framework (a change of one
-- value of an object, found again from the request), the journal of what the producer's undo could take back.
--
-- Every change is written in ONE synchronous callback named by `describe_undo(desc)` (`desc` carries the host's nonce), so the
-- producer's own undo takes it back whole. Kumi's own undo never calls Renoise's: it writes the prior values back as a new
-- step. Each kind knows what it would do (`preview`), how to do it (`apply`, which reads what the preview fenced again and
-- refuses `changed` before it writes), and how to put it back (`restore`, which refuses `changed` if the fields it touched no
-- longer hold what the apply wrote). Mute, solo, loop and selection are not Renoise undo steps: they answer `entry: false`.

local import = ...
local json = import("json")
local refs = import("refs")
local rows = import("rows")

local M = {}

local fail, the_song = rows.fail, rows.the_song
M.fail, M.the_song = fail, the_song

local function bad(message) fail("bad-params", message) end
M.bad = bad

-- The kinds of change, by name. A kind is either a property (a value of an object: `locate`, `read`, `wanted`, `write`, `same`,
-- `title`) or custom (`preview`, `apply`, `restore` of its own).
M.KINDS = {}
function M.register(spec) M.KINDS[spec.kind] = spec end


local MINUS = "\226\136\146" -- the true minus sign, U+2212
local INFINITY = "\226\136\158" -- U+221E

-- Writes that make one slice of a restore (about a second of Renoise's time), and the most a journalled change may hold.
M.SLICE_WRITES = 15000
-- The wait between the numbered steps of a restore. Renoise made one undo step of two callbacks 10 ms apart (measured), and
-- kept callbacks 300 ms apart apart.
M.STEP_GAP_MS = 400
M.MAX_MATERIAL_BYTES = 8 * 1024 * 1024
M.JOURNAL_MAX = 64

-- Text

local function trimmed(value)
  local text = string.format("%.1f", value)
  return (text:gsub("%.0$", ""))
end

-- dB with the true minus sign, as the titles show it: `−6`, `+3`, `0`, `−6.5`; `−∞` at silence.
function M.db_text(db)
  if db == nil then return MINUS .. INFINITY end
  local rounded = math.floor(db * 10 + 0.5) / 10
  if rounded == 0 then return "0" end
  if rounded < 0 then return MINUS .. trimmed(-rounded) end
  return "+" .. trimmed(rounded)
end

function M.pan_text(pan)
  local percent = math.floor(math.abs(pan) * 100 + 0.5)
  if percent == 0 then return "centre" end
  if pan < 0 then return percent .. "% left" end
  return percent .. "% right"
end

local function close(a, b)
  if type(a) ~= "number" or type(b) ~= "number" then return a == b end
  return math.abs(a - b) <= 1e-6 * math.max(1, math.abs(a), math.abs(b))
end

local function db_of(linear)
  if type(linear) == "number" and linear > 0 then return 20 * math.log10(linear) end
  return nil
end

local function hex_of(color)
  if type(color) ~= "table" or not color[1] then return nil end
  return string.format("#%02x%02x%02x", color[1], color[2], color[3])
end

local function rgb_of(text)
  local r, g, b = tostring(text):match("^#(%x%x)(%x%x)(%x%x)$")
  if not r then return nil end
  return { tonumber(r, 16), tonumber(g, 16), tonumber(b, 16) }
end

-- The song and the track a request names

local function need_text(params, name)
  local value = params[name]
  if type(value) ~= "string" or value == "" then bad(name .. " must be a string") end
  return value
end

local function need_object(params, name)
  local value = params[name]
  if type(value) ~= "table" then bad(name .. " must be an object") end
  return value
end

-- The song, if the request's epoch is the song's and Renoise isn't busy.
local function context(params, env)
  local epoch = params.epoch
  if type(epoch) ~= "string" or epoch == "" then bad("epoch: the epoch the refs were read in") end
  if epoch ~= env.epoch() then
    fail("stale", "those refs are from an earlier epoch (" .. epoch .. "; now " .. env.epoch() .. "): the song changed, read again")
  end
  local song = the_song()
  if song.rendering then fail("busy", "Renoise is rendering the song; nothing changed") end
  return song
end

local function track_at(song, index)
  if index < 1 or index > rows.track_count(song) then fail("gone", "that track isn't in the song any more") end
  return song:track(index)
end

local function find(song, env, reference, what)
  if type(reference) ~= "string" then bad(what .. ": ref must be a track ref") end
  local index = refs.expect(reference, "track", 1, env.epoch())[1]
  local track = track_at(song, index)
  return index, track
end

local function is_master(track) return track.type == renoise.Track.TRACK_TYPE_MASTER end

local function named(index, track)
  local name = track.name
  if name == nil or name == "" then return "Track " .. string.format("%02d", index) end
  return name
end

local function target(env, index)
  return { ref = refs.make(env.epoch(), "track", index), kind = "track" }
end

local function stage_of(source)
  local stage = type(source) == "table" and source.stage or nil
  if stage == nil then return "pre" end
  if stage ~= "pre" and stage ~= "post" then bad("stage must be \"pre\" or \"post\"") end
  return stage
end


M.MINUS, M.INFINITY = MINUS, INFINITY
M.close, M.db_of, M.hex_of, M.rgb_of = close, db_of, hex_of, rgb_of
M.need_text, M.need_object, M.context = need_text, need_object, context
M.track_at, M.find, M.is_master, M.named, M.target, M.stage_of = track_at, find, is_master, named, target, stage_of

-- The properties of an object

local NOT_UNDOABLE = "Renoise's own undo won't take this back; Kumi can."
M.NOT_UNDOABLE = NOT_UNDOABLE

function M.same_table(a, b)
  if type(a) ~= "table" or type(b) ~= "table" then return a == b end
  for k, v in pairs(a) do
    if not M.same_table(v, b[k]) then return false end
  end
  for k in pairs(b) do
    if a[k] == nil then return false end
  end
  return true
end

-- What a change is about: found from the request by `spec.locate(song, env, input)` as {object, name, place, target}, where
-- `place` is what the journal remembers to find it again (`spec.reread(song, place)`).
function M.prop_preview(song, env, spec, input)
  local loc = spec.locate(song, env, input)
  local now = spec.read(loc.object, input)
  local wanted = spec.wanted(loc.object, input)
  local display = { title = spec.title(loc.name, now, wanted), before = now, after = wanted }
  if spec.same(now, wanted) then display.unchanged = true end
  if not spec.undoable then display.note = NOT_UNDOABLE end
  return { target = loc.target, prior = now, fence = now, display = display }
end

local function gone_is_changed(err)
  if type(err) == "table" and err.code == "gone" then
    error({ code = "changed", message = err.message .. ": it went since the preview" }, 0)
  end
  error(err, 0)
end
M.gone_is_changed = gone_is_changed

function M.prop_apply(song, env, spec, input, fence, desc)
  local ok, loc = pcall(spec.locate, song, env, input)
  if not ok then gone_is_changed(loc) end
  local now = spec.read(loc.object, fence)
  if not spec.same(now, fence) then
    fail("changed", loc.name .. "'s " .. spec.noun .. " changed since the preview")
  end
  local wanted = spec.wanted(loc.object, input)
  if spec.undoable then song:describe_undo(desc) end
  if spec.watch then spec.watch(env, loc) end
  if not spec.same(now, wanted) then spec.write(loc.object, wanted) end
  local after = spec.read(loc.object, wanted)
  if not spec.same(after, wanted) then fail("failed", "Renoise kept " .. loc.name .. "'s " .. spec.noun .. " as it was") end
  local display = { title = spec.title(loc.name, now, after), before = now, after = after }
  if not spec.undoable then display.note = NOT_UNDOABLE end
  return {
    applied = after,
    display = display,
    undo = { desc = desc, entry = spec.undoable },
    spec = spec, place = loc.place, prior = now,
  }
end

function M.prop_restore(song, env, spec, restore, desc)
  local reference = restore.target and restore.target.ref
  local ok, loc = pcall(spec.locate, song, env, { ref = reference })
  if not ok then
    if type(loc) == "table" and loc.code == "failed" then loc.code = "gone" end
    error(loc, 0)
  end
  local now = spec.read(loc.object, restore.prior)
  if not spec.same(now, restore.prior) then
    if not restore.discard and not spec.same(now, restore.applied) then
      fail("changed", loc.name .. "'s " .. spec.noun .. " changed in Renoise since Kumi set it")
    end
    if spec.undoable then song:describe_undo(desc) end
    if spec.watch then spec.watch(env, loc) end
    spec.write(loc.object, restore.prior)
    local after = spec.read(loc.object, restore.prior)
    if not spec.same(after, restore.prior) then fail("failed", "Renoise kept " .. loc.name .. "'s " .. spec.noun .. " as it was") end
  end
  local after = spec.read(loc.object, restore.prior)
  local display = { title = spec.title(loc.name, now, after) .. " (put back)", before = now, after = after }
  return { restored = after, display = display, undo = { desc = desc, entry = spec.undoable }, place = loc.place }
end

-- The journal: the changes this tool made that a producer's undo could take back. An entry: {desc, kind, place (what finds the
-- object again: {track}, {track, device}…), spec (with `reread(song, place)` and `read`, to look at it again), prior, applied,
-- state ("applied" | "undone"), shape? ("deleted": the change removed an object; "made": it made one)}.

function M.journal_new()
  return { entries = {}, events = {} }
end

function M.journal_reset(journal)
  journal.entries, journal.events = {}, {}
end

function M.remember(journal, entry)
  local entries = journal.entries
  entries[#entries + 1] = entry
  while #entries > M.JOURNAL_MAX do table.remove(entries, 1) end
end

local function queue(journal, desc, how)
  local events = journal.events
  local last = events[#events]
  if last and last.how == how then
    last.descs[#last.descs + 1] = desc
  else
    events[#events + 1] = { how = how, descs = { desc } }
  end
end

-- The events to tell Kumi of, once.
function M.journal_take(journal)
  local events = journal.events
  journal.events = {}
  return events
end

function M.journal_active(journal)
  return #journal.entries > 0
end

-- An object that the change removed comes back (the producer's undo) or goes again (redo); one that the change made goes (undo)
-- or comes back (redo). `type_` is what happened ("insert" or "remove") to the object at `at`.
local function structural(journal, entry, type_, at, undoing, where)
  local returns = (entry.shape == "deleted") == (type_ == "insert")
  if undoing and where == at then
    if returns and entry.state == "applied" then
      entry.state = "undone"
      queue(journal, entry.desc, "undo")
      return true
    elseif not returns and entry.state == "undone" then
      entry.state = "applied"
      queue(journal, entry.desc, "redo")
      return true
    end
  end
  return false
end

-- A track made or deleted at `index`: the places the journal knows move with them, and the changes that removed or made a
-- track are told when the producer's undo or redo does it again.
function M.journal_track_event(journal, type_, index, undoing)
  for i = #journal.entries, 1, -1 do
    local entry = journal.entries[i]
    local place = entry.place
    if place and place.track then
      if entry.shape and place.device == nil then
        -- (an object that is a track itself: undone when it is made or removed again at its place)
        if not structural(journal, entry, type_, index, undoing, place.track) then
          if type_ == "insert" then
            if place.track >= index then place.track = place.track + 1 end
          elseif place.track > index then
            place.track = place.track - 1
          end
        end
      elseif type_ == "insert" then
        if place.track >= index then place.track = place.track + 1 end
      elseif place.track == index then
        table.remove(journal.entries, i) -- (the track is gone: nothing left to compare)
      elseif place.track > index then
        place.track = place.track - 1
      end
    end
  end
end

-- A device made, deleted or swapped on a track: the places of the devices the journal knows move with it, and a change that
-- made or removed a device is told when the producer's undo or redo does it again.
function M.journal_device_event(journal, track, type_, index, index2, undoing)
  for i = #journal.entries, 1, -1 do
    local entry = journal.entries[i]
    local place = entry.place
    if place and place.device and place.track == track then
      local handled = false
      if entry.shape and type_ ~= "swap" then handled = structural(journal, entry, type_, index, undoing, place.device) end
      if not handled then
        if type_ == "insert" then
          if place.device >= index then place.device = place.device + 1 end
        elseif type_ == "remove" then
          if place.device == index and not entry.shape then
            table.remove(journal.entries, i) -- (the device is gone: nothing left to compare)
          elseif place.device > index then
            place.device = place.device - 1
          end
        elseif type_ == "swap" then
          if place.device == index then place.device = index2 elseif place.device == index2 then place.device = index end
        end
      end
    end
  end
end

-- A pattern made or deleted: the patterns the journal knows are numbered by their place and move with it.
function M.journal_pattern_event(journal, type_, index)
  for i = #journal.entries, 1, -1 do
    local place = journal.entries[i].place
    if place and place.pattern then
      if type_ == "insert" then
        if place.pattern >= index then place.pattern = place.pattern + 1 end
      elseif type_ == "remove" then
        if place.pattern == index then
          table.remove(journal.entries, i) -- (the pattern is gone: nothing left to compare)
        elseif place.pattern > index then
          place.pattern = place.pattern - 1
        end
      end
    end
  end
end

-- After a producer's undo or redo: the journalled properties are read again and compared with what Kumi wrote and what it
-- replaced.
function M.journal_check(journal)
  local song = renoise.song()
  if not song then return end
  for _, entry in ipairs(journal.entries) do
    local spec = entry.spec
    if entry.follows == "device" then
      -- (a device moved: its place is followed through the swaps, and is where it started or where Kumi put it)
      local now = entry.place.device
      if entry.state == "applied" and now == entry.prior.index and now ~= entry.applied.index then
        entry.state = "undone"
        queue(journal, entry.desc, "undo")
      elseif entry.state == "undone" and now == entry.applied.index then
        entry.state = "applied"
        queue(journal, entry.desc, "redo")
      end
    elseif spec and spec.reread and not entry.shape then
      local ok, now = pcall(function()
        local object = spec.reread(song, entry.place)
        if object == nil then return nil end
        return spec.read(object, entry.applied)
      end)
      if ok and now ~= nil then
        if entry.state == "applied" and spec.same(now, entry.prior) and not spec.same(now, entry.applied) then
          entry.state = "undone"
          queue(journal, entry.desc, "undo")
        elseif entry.state == "undone" and spec.same(now, entry.applied) then
          entry.state = "applied"
          queue(journal, entry.desc, "redo")
        end
      end
    end
  end
end

-- Kumi put a change back itself: the change is not the producer's to undo any more (the producer's undo of that restore then
-- brings it back, which is told as a redo).
function M.note_restored(journal, kind, place, applied, spec)
  for _, entry in ipairs(journal.entries) do
    if entry.kind == kind and entry.state == "applied" and
      (entry.shape or (M.same_table(entry.place, place) and spec and spec.same(entry.applied, applied))) then
      entry.state = "undone"
      if entry.shape and place then entry.place = place end
      return
    end
  end
end

return M
