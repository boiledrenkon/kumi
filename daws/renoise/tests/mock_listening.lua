-- What the mock Renoise does for listening (M5), as Renoise 3.5.4 was measured to do it (m5/renoise.md):
--   song:render   renders in the background (`rendering` true, `transport.playing` true meanwhile, playback stopped after), takes
--                 end_pos inclusive, raises for a line out of range or a second render; the test ends it with `finish_render`,
--                 which writes the WAV (cut short with `fraction`) and calls back (unless `no_callback`).
--   cancel_rendering  is recorded as a crash: it leaves Renoise's undo dead, so the tool must never call it.
--   solo          rewrites mute states: every other track goes OFF but what the soloed one needs (its group parents, a group's
--                 members, the send tracks it feeds, a send's sources), an un-solo sets every track ACTIVE, and a solo is exclusive.
--   samples       have a sample buffer (`buffer = {rate, frames, channels}` in their data) that save_as writes, and slices.
--   transport     plays from start_at at the song's tempo (`playback_pos_beats` moves with the mock clock), follow mode drags the
--                 edit position, and the song's length in beats is told.

return function(M, object)

local function u16(n) return string.char(n % 256, math.floor(n / 256) % 256) end
local function u32(n)
  return string.char(n % 256, math.floor(n / 256) % 256, math.floor(n / 65536) % 256, math.floor(n / 16777216) % 256)
end

-- A WAV of silence: 32-bit float, as Renoise writes them.
function M.write_wav(path, frames, rate, channels)
  channels = channels or 2
  local size = frames * channels * 4
  local f = assert(io.open(path, "wb"))
  f:write("RIFF", u32(36 + size), "WAVE", "fmt ", u32(16), u16(3), u16(channels), u32(rate), u32(rate * channels * 4),
    u16(channels * 4), u16(32), "data", u32(size))
  local chunk = string.rep("\0", 65536)
  local left = size
  while left > 0 do
    local n = math.min(left, #chunk)
    f:write(n == #chunk and chunk or chunk:sub(1, n))
    left = left - n
  end
  f:close()
end

-- The seconds a render from `start` to `stop` (inclusive) lasts: every line at the LPB and BPM in force, which the ZL and ZT
-- commands in any track's effect columns change from the line they are on.
function M.render_seconds(start, stop)
  local d = M.data
  local song = d.song
  local lpb, bpm = d.transport_fields.lpb, d.transport_fields.bpm
  local seconds = 0
  for s = 1, stop.sequence do
    local p = d.sequence[s]
    for l = 1, d.patterns[p].lines do
      for t = 1, #d.tracks do
        local columns = d.tracks[t].effect_columns or 1
        local effects = ((d.patterns[p].tracks[t] or {}).lines or {})[l]
        effects = effects and effects.effects or {}
        for c = 1, columns do
          local e = effects[c]
          if e and e.number and e.number ~= 0 then
            local text = song:pattern(p):track(t):line(l):effect_column(c).number_string
            if text == "ZL" and e.amount > 0 then lpb = e.amount elseif text == "ZT" and e.amount >= 32 then bpm = e.amount end
          end
        end
      end
      local inside = (s > start.sequence or (s == start.sequence and l >= start.line))
      if inside then seconds = seconds + 60 / (bpm * lpb) end
      if s == stop.sequence and l == stop.line then return seconds end
    end
  end
  return seconds
end

local LOGIC = "std::logic_error: "

local function install_render(song)
  local d = M.data
  local fields = M.impl(song)
  fields.rendering_progress = 0
  fields.render = function(_, options, path, done)
    if fields.rendering then error(LOGIC .. "'rendering already is in progress. can only start one render process.'", 2) end
    if type(options) ~= "table" or type(path) ~= "string" or type(done) ~= "function" then error("mock: render(options, path, done)", 2) end
    for _, position in ipairs({ options.start_pos, options.end_pos }) do
      local p = d.sequence[position.sequence]
      if not p or position.line < 1 or position.line > d.patterns[p].lines then
        error(LOGIC .. "'invalid start_pos or end_pos 'line'. line must be [1 - pattern_length].'", 2)
      end
    end
    if M.render_refuses then return false, M.render_refuses end
    local record = { path = path, options = options, done = done, tracks = {}, rate = options.sample_rate or 44100 }
    for k, t in ipairs(d.tracks) do record.tracks[k] = { mute = t.mute_state, solo = t.solo == true, name = t.name, kind = t.kind } end
    record.seconds = M.render_seconds(options.start_pos, options.end_pos)
    record.was_playing = d.transport_fields.playing
    M.renders[#M.renders + 1] = record
    M.pending_render = record
    fields.rendering, fields.rendering_progress = true, 0
    -- (Renoise reads as playing while it renders; playback is stopped once it ends)
    d.transport_fields.playing = true
    M.fire(d.transport_fields.playing_observable)
    return true
  end
  fields.cancel_rendering = function()
    M.crashes[#M.crashes + 1] = "cancel_rendering (it leaves Renoise's undo dead until the app loses focus: measured)"
    fields.rendering = false
  end
end

-- The render in flight ends: `fraction` of its frames written (a render cut short still calls back: measured after a cancel),
-- `no_callback` (it ends with none), `no_file`.
function M.finish_render(options)
  options = options or {}
  local record = assert(M.pending_render, "mock: no render in flight")
  M.pending_render = nil
  local d = M.data
  local frames = math.floor(record.seconds * record.rate + 0.5)
  if options.fraction then frames = math.floor(frames * options.fraction) end
  if not options.no_file then M.write_wav(record.path, frames, record.rate, 2) end
  record.frames = frames
  local fields = M.impl(d.song)
  fields.rendering, fields.rendering_progress = false, 0
  d.transport_fields.playing = false
  M.fire(d.transport_fields.playing_observable)
  if not options.no_callback then
    local ok, err = M.run_callback(record.done)
    if not ok then M.errors[#M.errors + 1] = "render callback: " .. tostring(err) end
  end
  return record
end

function M.render_progress(value)
  M.impl(M.data.song).rendering_progress = value
end

-- Renoise's solo

-- The tracks a solo of track `i` keeps playing (soloed with it).
local function needed(i)
  local tracks = M.data.tracks
  local keep = { [i] = true }
  for g, t in ipairs(tracks) do
    if t.kind == "group" and g > i and g - t.members <= i then keep[g] = true end
  end
  local track = tracks[i]
  if track.kind == "group" then
    for k = i - track.members, i - 1 do keep[k] = true end
  end
  local sends, n = {}, 0
  for k, t in ipairs(tracks) do
    if t.kind == "send" then sends[n], n = k, n + 1 end
  end
  local function receivers(t)
    local out = {}
    for _, device in ipairs(t.devices or {}) do
      if (device.path or ""):find("#Send", 1, true) then
        for _, parameter in ipairs(device.params or {}) do
          if parameter.name:match("^Receiver") and sends[parameter.value] then out[#out + 1] = sends[parameter.value] end
        end
      end
    end
    return out
  end
  for _, k in ipairs(receivers(track)) do keep[k] = true end
  if track.kind == "send" then
    for k, t in ipairs(tracks) do
      for _, r in ipairs(receivers(t)) do
        if r == i then keep[k] = true end
      end
    end
  end
  return keep
end

function M.solo_write(i, value)
  local tracks = M.data.tracks
  local track = tracks[i]
  value = value == true
  if (track.solo == true) == value then return end
  local fired = {}
  local keep = value and needed(i) or nil
  for k, t in ipairs(tracks) do
    if t.kind ~= "master" then
      local solo = value and keep[k] == true or false
      local mute = (not value or keep[k]) and 1 or 2
      if (t.solo == true) ~= solo then
        t.solo = solo
        fired[#fired + 1] = t.solo_state_observable
      end
      if t.mute_state ~= mute then
        t.mute_state = mute
        fired[#fired + 1] = t.mute_state_observable
      end
    end
  end
  M.solo_writes = (M.solo_writes or 0) + 1
  for _, observable in ipairs(fired) do M.fire(observable) end
end

function M.track_extras(i, t, fields, getters, setters)
  if M.data.options.simple_solo then return end
  setters.solo_state = function(value)
    if t.kind == "master" then error("mock: the master track has no solo", 2) end
    for k, candidate in ipairs(M.data.tracks) do
      if candidate == t then return M.solo_write(k, value) end
    end
  end
end

-- Samples

function M.sample_extras(sample, fields, getters)
  local buffer = sample.buffer
  if fields.sample_buffer == nil then
    fields.sample_buffer = object("renoise.SampleBuffer", {
      has_sample_data = buffer ~= nil, sample_rate = buffer and buffer.rate or 44100, number_of_frames = buffer and buffer.frames or 0,
      number_of_channels = buffer and buffer.channels or 2, bit_depth = buffer and buffer.bits or 32, read_only = sample.slice_alias == true,
      save_as = function(_, path, format)
        if format ~= "wav" and format ~= "flac" then error("mock: save_as wants wav or flac", 2) end
        local dir = path:match("^(.*)[/\\][^/\\]+$")
        if not buffer or (dir and not io.exists(dir)) then
          -- (Renoise shows its own error dialog)
          M.modals[#M.modals + 1] = { "save_as", path }
          M.errors[#M.errors + 1] = "modal: save_as failed"
          return false
        end
        M.write_wav(path, buffer.frames, buffer.rate, buffer.channels)
        M.saved_samples[#M.saved_samples + 1] = path
        return true
      end,
    })
  end
  if getters.is_slice_alias == nil then getters.is_slice_alias = function() return sample.slice_alias == true end end
  if fields.slice_markers == nil then fields.slice_markers = sample.markers or {} end
end

-- The transport

function M.transport_extras(tf, getters, _, d)
  tf.timing_model = tf.timing_model or 2
  tf.edit_pos = tf.edit_pos or { sequence = 1, line = 1 }
  local function beat_of(position)
    local lines = 0
    for s = 1, position.sequence - 1 do lines = lines + d.patterns[d.sequence[s]].lines end
    return (lines + position.line - 1) / tf.lpb
  end
  local function position_of(beat)
    local line = math.floor(beat * tf.lpb + 1e-9)
    for s = 1, #d.sequence do
      local lines = d.patterns[d.sequence[s]].lines
      if line < lines then return { sequence = s, line = line + 1 } end
      line = line - lines
    end
    return { sequence = #d.sequence, line = d.patterns[d.sequence[#d.sequence]].lines }
  end
  getters.song_length_beats = function()
    local lines = 0
    for s = 1, #d.sequence do lines = lines + d.patterns[d.sequence[s]].lines end
    return lines / tf.lpb
  end
  getters.playback_pos_beats = function()
    if tf.playing and d.play_start then return d.play_start.beat + (M.time - d.play_start.time) / 1000 * tf.bpm / 60 end
    return tf.playback_pos_beats
  end
  local start_at, stop = tf.start_at, tf.stop
  tf.start_at = function(self, position)
    start_at(self, position)
    local at = type(position) == "table" and position or { sequence = d.song_fields.selected_sequence_index, line = position }
    d.play_start = { time = M.time, beat = beat_of(at) }
  end
  tf.stop = function(self)
    -- (follow mode drags the edit position to where playback stopped)
    if d.play_start and tf.follow_player then tf.edit_pos = position_of(getters.playback_pos_beats()) end
    stop(self)
    d.play_start = nil
  end
end

-- Each test starts with no render, and with the transport's constants Renoise has.
local reset = M.reset
function M.reset(options)
  reset(options)
  M.renders, M.pending_render, M.render_refuses, M.saved_samples, M.solo_writes, M.copies_from = {}, nil, nil, {}, 0, 0
  local transport = M.impl(renoise.Transport)
  transport.TIMING_MODEL_SPEED, transport.TIMING_MODEL_LPB = 1, 2
  return M
end

-- PatternTrack:copy_from(other): the other's lines and its automation, put on the parameter at the same device and place of this
-- track (measured: a scratch copy's automation lands on its own devices), as one write with its undo step.
local function deep(value)
  if type(value) ~= "table" then return value end
  local out = {}
  for k, v in pairs(value) do out[k] = deep(v) end
  return out
end

local function install_copy_from(song)
  local d = M.data
  local fields = M.impl(song)
  local pattern = fields.pattern
  fields.pattern = function(self, p)
    local object_ = pattern(self, p)
    local pattern_fields = M.impl(object_)
    local track = pattern_fields.track
    pattern_fields.track = function(this, t)
      local pattern_track = track(this, t)
      M.impl(pattern_track).copy_from = function(_, other)
        local here = d.patterns[p]
        local from_t, from_p
        for pp, candidate in ipairs(d.patterns) do
          for tt, data in pairs(candidate.tracks) do
            if M.impl(other).__data == data then from_p, from_t = pp, tt end
          end
        end
        assert(from_t, "mock: copy_from needs a pattern track of this song")
        local source = d.patterns[from_p].tracks[from_t]
        local old = here.tracks[t]
        local copy = { alias = source.alias, lines = deep(source.lines), automation = {} }
        for _, entry in ipairs(source.automation or {}) do
          for di, device in ipairs(d.tracks[from_t].devices) do
            for ki, parameter in ipairs(device.params or {}) do
              local mine = d.tracks[t].devices[di] and d.tracks[t].devices[di].params and d.tracks[t].devices[di].params[ki]
              if parameter == entry.parameter and mine then
                copy.automation[#copy.automation + 1] = { parameter = mine, playmode = entry.playmode, points = deep(entry.points) }
              end
            end
          end
        end
        copy.alias_observable = (old or {}).alias_observable
        here.tracks[t] = copy
        M.copies_from = (M.copies_from or 0) + 1
        M.record(function() here.tracks[t] = old end, function() here.tracks[t] = copy end)
        for _, fn in ipairs({ unpack(here.notifiers) }) do pcall(fn, { pattern = p, track = t, line = 1 }) end
      end
      M.impl(pattern_track).__data = d.patterns[p].tracks[t]
      return pattern_track
    end
    return object_
  end
end

local make_song = M.make_song
function M.make_song(options)
  local song = make_song(options)
  install_render(song)
  install_copy_from(song)
  return song
end

end
