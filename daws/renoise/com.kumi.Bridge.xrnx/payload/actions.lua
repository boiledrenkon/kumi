-- M4 (n-song): what Kumi asks Renoise to do that changes nothing of the song: audition a note on an instrument (heard out
-- loud, ended after its time), select or show things (the selected track, instrument, sequence slot, line, device, sample,
-- phrase; the middle frame, the lower frame, the pattern matrix), and save the song to its own file. None is an undo step
-- (measured in 3.5.4: selection isn't; a save writes a file), and none leaves a HISTORY row.
--
-- Measured in Renoise 3.5.4 (lab, 2026-10-19): `trigger_instrument_note_on(i, track, note | {notes}, volume)` plays with the
-- transport stopped, on any track; setting `selected_pattern_index` reassigns the selected slot's pattern (so a pattern is shown
-- by selecting a slot that plays it); `save_song_as` over another file overwrites it with no prompt, and into a folder that
-- doesn't exist shows a modal error (so Kumi checks the folder, and never writes over another file).

local import = ...
local refs = import("refs")
local rows = import("rows")

local M = {}

local function fail(code, message) error({ code = code, message = message }, 0) end
local function bad(message) fail("bad-params", message) end
local function q(text) return "\226\128\156" .. tostring(text) .. "\226\128\157" end

-- The auditions playing: {instrument, track, notes, sample?, until (ms)}; each is ended by `tick` once its time is up.
M.playing = {}
M.DEFAULT_MS, M.MOST_MS = 1000, 10000

local SEMITONES = { C = 0, ["C#"] = 1, D = 2, ["D#"] = 3, E = 4, F = 5, ["F#"] = 6, G = 7, ["G#"] = 8, A = 9, ["A#"] = 10, B = 11 }
local NAMES = { "C-", "C#", "D-", "D#", "E-", "F-", "F#", "G-", "G#", "A-", "A#", "B-" }

local function note_of(value)
  if type(value) == "number" then
    if value ~= math.floor(value) or value < 0 or value > 119 then bad("a note is 0 to 119 (48 is C-4), or a name such as C-4") end
    return value
  end
  if type(value) ~= "string" then bad("a note is a name such as C-4 or C#5, or a number 0 to 119") end
  local letter, sharp, octave = value:upper():match("^([A-G])([-#]?)(%d)$")
  local semitone = letter and SEMITONES[letter .. (sharp == "#" and "#" or "")]
  if not semitone then bad("a note is a name such as C-4 or C#5 (E and B have no sharp), or a number 0 to 119; got " .. value) end
  return tonumber(octave) * 12 + semitone
end

local function note_name(value) return NAMES[value % 12 + 1] .. math.floor(value / 12) end

-- (Kumi names refs as `…Ref`; the host passes them as they come)
local function named_refs(params)
  params = type(params) == "table" and params or {}
  local out = {}
  for k, v in pairs(params) do out[k] = v end
  for _, name in ipairs({ "instrument", "track", "sample", "device", "phrase", "sequence", "pattern" }) do
    if out[name] == nil then out[name] = out[name .. "Ref"] end
  end
  return out
end

local function epoch_check(params, env)
  if params.epoch ~= nil and params.epoch ~= env.epoch() then
    fail("stale", "those refs are from an earlier epoch (" .. tostring(params.epoch) .. "; now " .. env.epoch() .. "): read again")
  end
end

local function stop_one(song, playing)
  pcall(function()
    if playing.sample then
      song:trigger_sample_note_off(playing.instrument, playing.sample, playing.track, playing.notes[1])
    else
      song:trigger_instrument_note_off(playing.instrument, playing.track, playing.notes)
    end
  end)
end

-- Ends the auditions whose time is up (all of them with `all`).
function M.tick(now_ms, all)
  if #M.playing == 0 then return end
  local song = renoise.song()
  local kept = {}
  for _, playing in ipairs(M.playing) do
    if all or now_ms >= playing.until_ms then
      if song then stop_one(song, playing) end
    else
      kept[#kept + 1] = playing
    end
  end
  M.playing = kept
end

-- audition {instrument, track?, note? | notes?, volume?, durationMs?, sample?, stop?}
function M.audition(params, env)
  local song = rows.the_song()
  params = named_refs(params)
  if song.rendering then fail("busy", "Renoise is rendering the song") end
  local busy = env.busy()
  if busy then fail("busy", busy .. "; nothing was played") end
  epoch_check(params, env)
  if params.stop == true then
    local count = #M.playing
    M.tick(env.now_ms(), true)
    return { epoch = env.epoch(), stopped = count, display = { title = count == 0 and "Nothing was playing" or "Stopped the auditions" } }
  end
  if type(params.instrument) ~= "string" then bad("instrument: the instrument's ref") end
  local i = refs.expect(params.instrument, "instrument", 1, env.epoch())[1]
  if i > #song.instruments then fail("gone", "that instrument isn't in the song any more") end
  local t = song.selected_track_index
  if params.track ~= nil then
    if type(params.track) ~= "string" then bad("track: the ref of the track to play on") end
    t = refs.expect(params.track, "track", 1, env.epoch())[1]
    if t > rows.track_count(song) then fail("gone", "that track isn't in the song any more") end
  end
  local notes = {}
  if params.notes ~= nil then
    if type(params.notes) ~= "table" or #params.notes == 0 or #params.notes > 16 then bad("notes: 1 to 16 notes, played at once") end
    for k, value in ipairs(params.notes) do notes[k] = note_of(value) end
  else
    notes[1] = note_of(params.note == nil and 48 or params.note)
  end
  local volume = params.volume == nil and 1 or params.volume
  if type(volume) ~= "number" or volume ~= volume or volume < 0 or volume > 1 then bad("volume is from 0 to 1") end
  local duration = params.durationMs == nil and M.DEFAULT_MS or params.durationMs
  if type(duration) ~= "number" or duration ~= duration or duration < 1 or duration > M.MOST_MS then
    bad(string.format("durationMs is from 1 to %d", M.MOST_MS))
  end
  local sample
  if params.sample ~= nil then
    if type(params.sample) ~= "string" then bad("sample: the ref of a sample of the instrument") end
    local numbers = refs.expect(params.sample, "sample", 2, env.epoch())
    if numbers[1] ~= i then bad("that sample isn't the instrument's") end
    sample = numbers[2]
    if sample > #song:instrument(i).samples then fail("gone", "that sample isn't in the instrument any more") end
    if #notes > 1 then bad("a sample plays one note at a time") end
  end
  -- (the same notes on the same instrument and track are ended first: a note on twice would hang)
  local kept = {}
  for _, playing in ipairs(M.playing) do
    if playing.instrument == i and playing.track == t then stop_one(song, playing) else kept[#kept + 1] = playing end
  end
  M.playing = kept
  local ok, err = pcall(function()
    if sample then
      song:trigger_sample_note_on(i, sample, t, notes[1], volume, false)
    else
      song:trigger_instrument_note_on(i, t, notes, volume)
    end
  end)
  if not ok then fail("failed", "Renoise wouldn't play it: " .. tostring(err):gsub("^[^%s:]*%.lua:%d+: ", "")) end
  M.playing[#M.playing + 1] = { instrument = i, track = t, notes = notes, sample = sample, until_ms = env.now_ms() + duration }
  local names = {}
  for k, value in ipairs(notes) do names[k] = note_name(value) end
  local instrument = song:instrument(i)
  local label = instrument.name ~= "" and instrument.name or string.format("Instrument %02X", i - 1)
  local track = song:track(t)
  local seconds = duration >= 1000 and string.format("%g s", math.floor(duration / 100 + 0.5) / 10) or string.format("%d ms", duration)
  return {
    epoch = env.epoch(), layout = env.layout(),
    auditioning = {
      instrumentRef = refs.make(env.epoch(), "instrument", i), trackRef = refs.make(env.epoch(), "track", t), notes = names,
      durationMs = duration, sampleRef = sample and refs.make(env.epoch(), "sample", i, sample) or nil,
    },
    display = { title = string.format("Auditioning %s on %s (%s, %s)", table.concat(names, " "), q(label), q(track.name ~= "" and track.name or ("Track " .. t)), seconds) },
  }
end

-- select {track?, device?, instrument?, sample?, phrase?, sequence?, pattern?, line?, frame?, lower?, matrix?}
local FRAMES = {
  pattern = "MIDDLE_FRAME_PATTERN_EDITOR", mixer = "MIDDLE_FRAME_MIXER", phrase = "MIDDLE_FRAME_INSTRUMENT_PHRASE_EDITOR",
  keyzones = "MIDDLE_FRAME_INSTRUMENT_SAMPLE_KEYZONES", sample = "MIDDLE_FRAME_INSTRUMENT_SAMPLE_EDITOR",
  modulation = "MIDDLE_FRAME_INSTRUMENT_SAMPLE_MODULATION", effects = "MIDDLE_FRAME_INSTRUMENT_SAMPLE_EFFECTS",
  plugin = "MIDDLE_FRAME_INSTRUMENT_PLUGIN_EDITOR", midi = "MIDDLE_FRAME_INSTRUMENT_MIDI_EDITOR",
}
local LOWER = { dsp = "LOWER_FRAME_TRACK_DSPS", automation = "LOWER_FRAME_TRACK_AUTOMATION" }

function M.select(params, env)
  local song = rows.the_song()
  params = named_refs(params)
  epoch_check(params, env)
  local epoch = env.epoch()
  local plan, said = {}, {}
  local function ref_of(name, kind, count)
    if params[name] == nil then return nil end
    if type(params[name]) ~= "string" then bad(name .. ": a " .. kind .. " ref") end
    return refs.expect(params[name], kind, count, epoch)
  end
  local track, device = ref_of("track", "track", 1), ref_of("device", "device", 2)
  if device then
    if track and track[1] ~= device[1] then bad("that device isn't on that track") end
    track = { device[1] }
  end
  if track then
    if track[1] > rows.track_count(song) then fail("gone", "that track isn't in the song any more") end
    plan.track = track[1]
    if device then
      if device[2] > #song:track(device[1]).devices then fail("gone", "that device isn't on the track any more") end
      plan.device = device[2]
    end
  end
  local instrument, sample, phrase = ref_of("instrument", "instrument", 1), ref_of("sample", "sample", 2), ref_of("phrase", "phrase", 2)
  for _, inner in ipairs({ sample, phrase }) do
    if inner then
      if instrument and instrument[1] ~= inner[1] then bad("that sample or phrase isn't the instrument's") end
      instrument = { inner[1] }
    end
  end
  if instrument then
    if instrument[1] > #song.instruments then fail("gone", "that instrument isn't in the song any more") end
    plan.instrument = instrument[1]
    if sample and sample[2] > #song:instrument(sample[1]).samples then fail("gone", "that sample isn't in the instrument any more") end
    if phrase and phrase[2] > #song:instrument(phrase[1]).phrases then fail("gone", "that phrase isn't in the instrument any more") end
    plan.sample, plan.phrase = sample and sample[2], phrase and phrase[2]
  end
  local slot = ref_of("sequence", "seq", 1)
  if slot then
    if slot[1] > #song.sequencer.pattern_sequence then fail("gone", "that sequence slot isn't in the song any more") end
    plan.sequence = slot[1]
  end
  local pattern = ref_of("pattern", "pattern", 1)
  if pattern then
    -- (selecting a pattern itself would make the selected slot play it: the first slot that plays it is selected instead)
    if slot then bad("give the sequence slot or the pattern, not both") end
    for s, p in ipairs(song.sequencer.pattern_sequence) do
      if p == pattern[1] then plan.sequence = s break end
    end
    if not plan.sequence then bad("no sequence slot plays that pattern: select a slot") end
  end
  if params.line ~= nil then
    local p = song.sequencer.pattern_sequence[plan.sequence or song.selected_sequence_index]
    local lines = song:pattern(p).number_of_lines
    -- (Renoise's number, from 0)
    if type(params.line) ~= "number" or params.line ~= math.floor(params.line) or params.line < 0 or params.line >= lines then
      bad(string.format("line is a line of that pattern, from 0 to %d", lines - 1))
    end
    plan.line = params.line + 1
  end
  local W = renoise.ApplicationWindow
  if params.frame ~= nil then
    if not FRAMES[params.frame] then bad("frame is pattern, mixer, phrase, keyzones, sample, modulation, effects, plugin or midi") end
    plan.frame = W[FRAMES[params.frame]]
  end
  if params.lower ~= nil then
    if params.lower ~= "hidden" and not LOWER[params.lower] then bad("lower is dsp, automation or hidden") end
    plan.lower = params.lower
  end
  if params.matrix ~= nil then
    if type(params.matrix) ~= "boolean" then bad("matrix must be true or false") end
    plan.matrix = params.matrix
  end
  if next(plan) == nil then bad("give what to select or show") end
  -- Everything is checked: the selection and the frames are set (none of it is an undo step); what Renoise refuses is said.
  local refused = {}
  local function set(what, fn)
    local ok = pcall(fn)
    if not ok then refused[#refused + 1] = what end
    return ok
  end
  if plan.track and set("the track", function() song.selected_track_index = plan.track end) then said[#said + 1] = q(song:track(plan.track).name) end
  if plan.device then set("the device", function() song.selected_track_device_index = plan.device end) end
  if plan.instrument and set("the instrument", function() song.selected_instrument_index = plan.instrument end) then
    local name = song:instrument(plan.instrument).name
    said[#said + 1] = "the instrument " .. q(name ~= "" and name or string.format("%02X", plan.instrument - 1))
  end
  if plan.sample then set("the sample", function() song.selected_sample_index = plan.sample end) end
  if plan.phrase then set("the phrase", function() song.selected_phrase_index = plan.phrase end) end
  if plan.sequence and set("the slot", function() song.selected_sequence_index = plan.sequence end) then said[#said + 1] = "slot " .. plan.sequence end
  -- (Renoise moves the edit line a moment later: measured)
  if plan.line and set("the line", function() song.selected_line_index = plan.line end) then said[#said + 1] = string.format("line %02d", plan.line - 1) end
  local window = renoise.app().window
  if plan.frame and set("the frame", function() window.active_middle_frame = plan.frame end) then
    said[#said + 1] = "the " .. params.frame .. (params.frame == "mixer" and "" or " view")
  end
  if plan.lower then
    set("the lower frame", function()
      if plan.lower == "hidden" then
        window.lower_frame_is_visible = false
      else
        window.lower_frame_is_visible = true
        window.active_lower_frame = W[LOWER[plan.lower]]
      end
    end)
  end
  if plan.matrix ~= nil then set("the pattern matrix", function() window.pattern_matrix_is_visible = plan.matrix end) end
  return {
    epoch = epoch, layout = env.layout(),
    selected = {
      trackRef = plan.track and refs.make(epoch, "track", plan.track) or nil,
      deviceRef = plan.device and refs.make(epoch, "device", plan.track, plan.device) or nil,
      instrumentRef = plan.instrument and refs.make(epoch, "instrument", plan.instrument) or nil,
      sampleRef = plan.sample and refs.make(epoch, "sample", plan.instrument, plan.sample) or nil,
      phraseRef = plan.phrase and refs.make(epoch, "phrase", plan.instrument, plan.phrase) or nil,
      sequenceRef = plan.sequence and refs.make(epoch, "seq", plan.sequence) or nil,
      line = plan.line and plan.line - 1, frame = params.frame, lower = params.lower, matrix = plan.matrix,
    },
    display = {
      title = #said > 0 and ("Selected " .. table.concat(said, ", ")) or "Showed what was asked",
      note = #refused > 0 and ("Renoise didn't take " .. table.concat(refused, ", ") .. ".") or nil,
    },
  }
end

-- project_save {epoch?, version?, path?} (the host's neutral tool): the song to its own file; with `path`, saved as that file
-- (Renoise's Save As: the song is that file from then on). An unsaved song needs `path`; `version` is REAPER's. A path is a whole
-- path to a .xrns that isn't there yet (Renoise writes over another file with no prompt), in a folder that is and that Renoise can
-- write in (it stops at a modal error otherwise): measured in 3.5.4.
local function folder_of(path) return path:match("^(.*)[/\\][^/\\]+$") end

-- A folder Renoise can save into: there, and writable (else Renoise stops at a modal error, its timers with it): tried first.
-- `why` ends the refusal (what to do instead).
local function check_folder(folder, why)
  if not folder or folder == "" or not io.exists(folder) then bad("there is no folder " .. tostring(folder) .. " (Renoise would stop at a dialog)" .. why) end
  local probe = folder .. "/.kumi-save-check"
  local file = io.open(probe, "wb")
  if file then
    file:close()
    os.remove(probe)
  else
    bad("Renoise can't write in " .. folder .. why)
  end
end

local function save_as(path)
  local absolute = path:match("^/") or path:match("^%a:[/\\]")
  if not absolute or not path:lower():match("%.xrns$") then bad("path is a whole path to a .xrns file") end
  local folder = folder_of(path)
  check_folder(folder, "")
  if io.exists(path) then bad(path .. " is there already: Kumi doesn't save over another file") end
  renoise.app():save_song_as(path)
end

function M.project_save(params, env)
  local song = rows.the_song()
  params = type(params) == "table" and params or {}
  if song.rendering then fail("busy", "Renoise is rendering the song") end
  local busy = env.busy()
  if busy then fail("busy", busy .. "; nothing was saved") end
  epoch_check(params, env)
  if params.version ~= nil then bad("version is REAPER's (a new version of the project): Renoise saves the song, or saves it as path") end
  local own, path = song.file_name, params.path
  if path ~= nil and (type(path) ~= "string" or path == "") then bad("path: a whole path to a .xrns file to save the song as") end
  if path == nil or path == own then
    if own == "" then bad("the song has never been saved: give path, a .xrns file to save it as") end
    -- (its own folder may be gone, renamed or read-only since it was opened: checked as Save As checks a new one)
    check_folder(folder_of(own), ": give path, a .xrns file to save the song as somewhere else")
    renoise.app():save_song()
    -- (`dirty`: whether the song has changes no file holds, as REAPER's save answers; none, just saved)
    return { epoch = env.epoch(), saved = own, dirty = false, display = { title = "Saved the song" } }
  end
  save_as(path)
  local display = { title = "Saved the song as " .. q(path:match("([^/\\]+)$")) }
  if own ~= "" then display.note = "The song is that file from now on (it was " .. own .. ")." end
  return { epoch = env.epoch(), saved = path, dirty = false, display = display }
end

return M
