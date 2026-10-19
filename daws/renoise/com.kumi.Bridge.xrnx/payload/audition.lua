-- Playing a render out loud (M5 contract §2.4, §6.3). Renoise's Lua can't play a file without changing the song: every way to put
-- one into an instrument (inserting it, load_from, even set_sample_data with undo off) clears the producer's redo, and the insert
-- is an undo step (measured, 3.5.4). What it can do with nothing changed is play the song: so an audition plays the part of the
-- song the file was rendered from, with Renoise's own transport (no undo step, the song not modified: measured), and puts back
-- what playing moved (the edit position, which follow mode drags along, and the loops it turned off so the part plays through).

local import = ...
local json = import("json")
local rows = import("rows")
local base = import("changes_base")
local listening = import("listening")
local log = import("log")

local fail, bad = base.fail, base.bad

local M = {}

-- A move of the playhead farther than this from where the audition's own playing would have it is the producer's.
M.MOVED_BEATS = 2
M.SETTLE_MS = 300

local S

function M.init(state)
  state.audition = state.audition or {}
  S = state.audition
  return S
end

local function now_ms() return os.clock() * 1000 end

local function playing()
  return S.now
end
M.playing = playing

local function number(value, name)
  if type(value) ~= "number" or value ~= value or value == math.huge or value == -math.huge then bad(name .. " must be a number") end
  return value
end

-- What playing moved, back as it was. `keep_position`: the producer took over the transport, which stays theirs.
local function put_back(env, song, audition, keep_position)
  local transport = song.transport
  local saved = audition.saved
  env.quiet(function()
    if saved.loop_pattern and not transport.loop_pattern then pcall(function() transport.loop_pattern = true end) end
    if saved.loop_block and not transport.loop_block_enabled then pcall(function() transport.loop_block_enabled = true end) end
    if saved.loop_range then pcall(function() transport.loop_sequence_range = saved.loop_range end) end
    if not keep_position and saved.edit then
      pcall(function() transport.edit_pos = renoise.SongPos(saved.edit[1], saved.edit[2]) end)
    end
  end)
end

-- Ends the audition: `stop` stops the transport (it is still Kumi's), `keep_position` leaves the playhead where the producer put it.
local function finish(env, why, stop, keep_position)
  local audition = S.now
  if not audition then return false end
  S.now = nil
  local song = renoise.song()
  if not song or env.document() ~= audition.document then return true end
  if stop and song.transport.playing then song.transport:stop() end
  put_back(env, song, audition, keep_position)
  log.write("info", "audition " .. audition.id .. " ended: " .. why)
  return true
end

-- audition {previewId, file, fromSeconds?, seconds?, gainDb?}
function M.play(params, env)
  if type(params) ~= "table" then bad("audition needs {previewId, file}") end
  local id = listening.need_id(params.previewId, "previewId", "p")
  local file = params.file
  if type(file) ~= "string" or file == "" then bad("file must be a path") end
  local song = rows.the_song()
  if song.rendering then fail("busy", "Renoise is rendering the song") end
  local reason = env.busy()
  if reason then fail("busy", reason) end
  local origin = listening.origin(file)
  if not origin then fail("failed", "Renoise can only play out loud the parts of the song Kumi rendered") end
  if origin.document ~= env.document() then fail("failed", "That render was of another song") end
  if not env.survives(origin.epoch, "seq") then
    fail("failed", "The song's sequence changed since Kumi rendered that part, so Renoise can't play it as it was")
  end
  local from = params.fromSeconds
  if from == nil or from == json.null then from = 0 end
  from = number(from, "fromSeconds")
  if from < 0 or from >= origin.seconds then bad(string.format("fromSeconds is from 0 to %.3f (the file's length)", origin.seconds)) end
  local length = params.seconds
  if length == nil or length == json.null then length = origin.seconds - from end
  length = number(length, "seconds")
  if length <= 0 then bad("seconds must be more than 0") end
  length = math.min(length, origin.seconds - from)
  local gain = params.gainDb
  if gain ~= nil and gain ~= json.null then number(gain, "gainDb") end
  -- (a new audition stops the one playing, which is Kumi's)
  if S.now then finish(env, "another audition", true) end
  local transport = song.transport
  if transport.playing then fail("failed", "Renoise is playing: Kumi won't move your playhead; stop it and ask again") end
  local timeline = listening.timeline(song, env.pattern_mark)
  local start = origin.start
  if not timeline.slots[start[1]] or start[2] > timeline.slots[start[1]].lines then
    fail("failed", "The song's sequence changed since Kumi rendered that part, so Renoise can't play it as it was")
  end
  local _, first_second = listening.line_start(timeline, start[1], start[2])
  local s, l = listening.line_at_second(timeline, first_second + from)
  local start_beat = listening.line_start(timeline, s, l)
  local edit = transport.edit_pos
  local saved = {
    edit = { edit.sequence, edit.line }, loop_pattern = transport.loop_pattern, loop_block = transport.loop_block_enabled,
  }
  local range = transport.loop_sequence_range
  if type(range) == "table" and range[1] and range[1] > 0 then saved.loop_range = { range[1], range[2] } end
  local notes = {}
  -- (a loop would play a part of it again and again: loops are off while Kumi plays, and back after; they are no undo steps)
  env.quiet(function()
    if saved.loop_pattern then transport.loop_pattern = false end
    if saved.loop_block then transport.loop_block_enabled = false end
    if saved.loop_range then transport.loop_sequence_range = {} end
  end)
  if saved.loop_pattern or saved.loop_block or saved.loop_range then
    notes[#notes + 1] = "Renoise's loop is off while Kumi plays, and back after."
  end
  transport:start_at(renoise.SongPos(s, l))
  S.now = {
    id = id, file = file, document = env.document(), started_ms = now_ms(), stop_ms = now_ms() + length * 1000, saved = saved,
    last_beat = start_beat, last_ms = now_ms(), bpm = transport.bpm,
  }
  if origin.what == "track" then
    notes[#notes + 1] = "Renoise plays the song over that part: the track is heard with the rest of the song."
  end
  if gain and gain ~= 0 then notes[#notes + 1] = "Renoise plays it at the song's own level (it can't change the level of what it plays)." end
  local answer = { previewId = id, playing = true, seconds = length, via = "song" }
  if #notes > 0 then answer.note = table.concat(notes, " ") end
  return answer
end

-- audition_stop {previewId?}
function M.stop(params, env)
  local id = type(params) == "table" and params.previewId or nil
  if id ~= nil and id ~= json.null and (not S.now or S.now.id ~= id) then return { stopped = false } end
  return { stopped = finish(env, "audition_stop", true) }
end

-- From the timer: the audition ends at its length, when the producer stops the transport, and when they move the playhead (then
-- the transport is theirs and keeps playing).
function M.tick(env)
  local audition = S.now
  if not audition then return end
  local song = renoise.song()
  if not song or env.document() ~= audition.document then
    S.now = nil
    return
  end
  local transport = song.transport
  if not transport.playing then
    finish(env, "the transport stopped", false)
    return
  end
  local now = now_ms()
  if now >= audition.stop_ms then
    finish(env, "its length", true)
    return
  end
  -- (the start takes a moment to show in the play position: the first ticks aren't judged)
  if now - audition.started_ms < M.SETTLE_MS then return end
  local beat = transport.playback_pos_beats
  local expected = audition.last_beat + (now - audition.last_ms) / 1000 * transport.bpm / 60
  if beat < audition.last_beat - 0.25 and expected >= transport.song_length_beats - M.MOVED_BEATS then
    -- (Renoise went round from the song's end to its start: the part is over)
    finish(env, "the song's end", true)
    return
  end
  if beat < audition.last_beat - 0.25 or math.abs(beat - expected) > M.MOVED_BEATS then
    finish(env, "the producer moved the playhead", false, true)
    return
  end
  audition.last_beat, audition.last_ms = beat, now
end

-- The tool stops, another song opens, or a render starts (it stops playback): an audition playing ends now.
function M.halt(env, why)
  if S.now then finish(env, why, true) end
end

return M
