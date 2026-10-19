-- Listening (M5): render jobs (render_start, listen_status, listen_cancel) against the mock Renoise, which renders as Renoise
-- 3.5.4 was measured to (see mock_listening.lua): a solo rewrites every other track's mute state, an un-solo sets every track
-- ACTIVE, the done callback comes even for a render cut short, and cancel_rendering is never to be called.
local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness

local next_id = 9000
local A, OFF, MUTED = 1, 2, 3

local function drain(peer)
  repeat
    local taken = peer:take()
    if #taken > 0 then H.send(peer, { type = "credit", bytes = #taken }) end
  until #taken == 0
end

-- Sends a request and runs the clock until its response comes; events meanwhile are kept in `peer.events`.
local function call(peer, method, params, ticks)
  next_id = next_id + 1
  local id = next_id
  H.send(peer, { type = "request", id = id, method = method, params = params or {} })
  peer.events = peer.events or {}
  for _ = 1, ticks or 400 do
    drain(peer)
    for _, line in ipairs(peer:frames(nil)) do
      local frame = assert(json.decode(line))
      if frame.type == "response" and frame.id == id then return frame end
      if frame.type == "event" then peer.events[#peer.events + 1] = frame end
    end
    mock.advance(10)
  end
end

local function result(response, what)
  ok(response and response.result, (what or "request") .. ": " .. json.encode(response and response.error or "no answer"))
  return response.result
end

local function failure(response, code, pattern)
  ok(response and response.error, "expected an error, got " .. json.encode(response and response.result or "nothing"))
  eq(response.error.code, code, "error code (" .. tostring(response.error.message) .. ")")
  if pattern then ok(response.error.message:find(pattern, 1, true), response.error.message) end
end

-- Kick, Hat, Bass and Pad (the mixer device on each), a master, a send; three patterns of 16 lines at 120 BPM, LPB 4 (a pattern
-- is 4 beats, 2 s).
local function song_options(extra)
  local spec = {
    name = "Listen", file_name = "/s/listen.xrns", bpm = 120, lpb = 4, mixer_device = true,
    tracks = { { name = "Kick" }, { name = "Hat" }, { name = "Bass" }, { name = "Pad" } },
    send_tracks = 1, patterns = 3, pattern_lines = 16, sequence_length = 3, sequence = { 1, 2, 3 }, instruments = 1,
  }
  for k, v in pairs(extra or {}) do spec[k] = v end
  return spec
end

local function setup(options, extra)
  options = options or {}
  options.song = song_options(extra)
  local env = H.setup(options)
  local peer = H.connect(env)
  H.frames(peer)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  env.listening = env.dir .. "/kumi/bridges/renoise/listening"
  return env, peer
end

local function place(peer)
  local focus = result(call(peer, "focus"), "focus")
  return focus.epoch, focus.layout
end

local function track_ref(epoch, i) return epoch .. ":track:" .. i end

local function track(i) return mock.data.tracks[i] end

local function states()
  local out = {}
  for i, t in ipairs(mock.data.tracks) do
    if t.kind ~= "master" then out[#out + 1] = i .. "=" .. t.mute_state .. (t.solo and "*" or "") end
  end
  return table.concat(out, " ")
end

local function start(peer, job, sources, range, more)
  local epoch, layout = place(peer)
  local params = { jobId = job, sources = sources, range = range or { fromBeat = 0, beats = 4 }, preRollBeats = 0, epoch = epoch, layout = layout }
  for k, v in pairs(more or {}) do params[k] = v end
  return call(peer, "render_start", params), epoch, layout
end

local function status(peer, job) return result(call(peer, "listen_status", { jobId = job }), "listen_status") end

local function exists(path)
  local f = io.open(path, "rb")
  if f then f:close() return true end
  return false
end

local function bridge() return KUMI_BRIDGE.bridge end

-- The render chain

test("listening: a render job renders the mix as the producer has it, then each track soloed, and puts every mute and solo back", function()
  local env, peer = setup()
  track(2).mute_state = MUTED
  track(3).mute_state = OFF
  local before = states()
  local epoch = place(peer)
  local answer = result((start(peer, "j1", {
    { id = "bass", what = "track", ref = track_ref(epoch, 3) }, { id = "mix", what = "master" }, { id = "kick", what = "track", ref = track_ref(epoch, 1) },
  })), "render_start")
  eq(answer.jobId, "j1")
  eq(answer.state, "running")
  eq(answer.kind, "render")
  ok(math.abs(answer.seconds - 2) < 1e-9, "16 lines at 120 BPM, LPB 4: " .. answer.seconds)
  eq(#answer.files, 3)
  eq(answer.files[1].file, env.listening .. "/j1-bass.wav")
  eq(answer.files[2].file, env.listening .. "/j1-mix.wav")
  ok(exists(env.listening), "the listening folder is made")
  -- 1: the mix first, as the producer has it (no solo, their mutes)
  eq(#mock.renders, 1)
  local r = mock.renders[1]
  eq(r.path, env.listening .. "/j1-mix.wav")
  eq(r.options.start_pos.sequence, 1)
  eq(r.options.start_pos.line, 1)
  eq(r.options.end_pos.line, 16, "end_pos is inclusive: the last line before beat 4")
  eq(r.options.sample_rate, 48000)
  eq(r.options.bit_depth, 32)
  eq(r.options.priority, "high")
  eq(r.options.interpolation, "precise")
  eq(r.tracks[2].mute, MUTED)
  eq(r.tracks[3].mute, OFF)
  eq(r.tracks[1].solo, false)
  mock.render_progress(0.5)
  local half = status(peer, "j1")
  eq(half.state, "running")
  ok(math.abs(half.progress - 0.5 / 3) < 1e-9, "progress: " .. half.progress)
  mock.finish_render()
  -- 2: Bass soloed (it was OFF: a solo plays it anyway), every other track OFF
  eq(#mock.renders, 2)
  r = mock.renders[2]
  eq(r.path, env.listening .. "/j1-bass.wav")
  eq(r.tracks[3].solo, true)
  eq(r.tracks[3].mute, A)
  eq(r.tracks[1].mute, OFF)
  eq(r.tracks[2].mute, OFF)
  mock.finish_render()
  -- 3: Kick soloed
  r = mock.renders[3]
  eq(r.tracks[1].solo, true)
  eq(r.tracks[3].solo, false)
  eq(r.tracks[3].mute, OFF)
  mock.finish_render()
  local done = status(peer, "j1")
  eq(done.state, "done")
  eq(done.progress, 1)
  eq(#done.files, 3)
  local by = {}
  for _, f in ipairs(done.files) do by[f.id] = f end
  eq(by.mix.stage, "master")
  eq(by.bass.stage, "post-fader", "a Renoise track is heard after its fader")
  eq(by.bass.throughMaster, true)
  eq(by.kick.sampleRate, 48000)
  eq(by.kick.channels, 2)
  ok(math.abs(by.kick.seconds - 2) < 1e-6, "the file's own length: " .. by.kick.seconds)
  eq(by.kick.start, 0)
  ok(exists(by.kick.file) and exists(by.mix.file), "the files are there")
  eq(states(), before, "every mute and solo as the producer had it")
  eq(#mock.data.undo.stack, 0, "no undo step")
  eq(done.notes, nil)
  H.no_dialogs()
end)

test("listening: the producer's soloed track is soloed again after the job, and Kumi's solo and mute writes aren't edits", function()
  local env, peer = setup()
  mock.solo_write(2, true) -- (the producer's solo: every other track OFF)
  track(4).mute_state = A -- (and they turned Pad back on while soloing)
  local before = states()
  local changed = bridge().changed
  local edits = bridge().edits or 0
  local epoch = place(peer)
  result((start(peer, "j2", { { id = "kick", what = "track", ref = track_ref(epoch, 1) } })), "render_start")
  eq(mock.renders[1].tracks[1].solo, true)
  eq(mock.renders[1].tracks[2].solo, false, "solo is exclusive")
  mock.finish_render()
  eq(status(peer, "j2").state, "done")
  eq(states(), before, "Hat soloed again and Pad on, as the producer had them")
  -- (the transport's playing on and off, twice, is all the song's notifiers counted)
  ok(bridge().changed - changed <= 4, "Kumi's writes are quiet: " .. (bridge().changed - changed))
  ok((bridge().edits or 0) - edits <= 4, "and no edits: " .. ((bridge().edits or 0) - edits))
  H.no_dialogs()
end)

test("listening: a track the producer soloed or muted during the render is left as they set it, and said", function()
  local env, peer = setup()
  track(4).mute_state = MUTED
  local epoch = place(peer)
  result((start(peer, "j3", { { id = "kick", what = "track", ref = track_ref(epoch, 1) }, { id = "hat", what = "track", ref = track_ref(epoch, 2) } })))
  mock.finish_render()
  -- (a MIDI-mapped mute button moved while Renoise rendered the Hat)
  mock.ops.mute_track(3, MUTED)
  mock.finish_render()
  local done = status(peer, "j3")
  eq(done.state, "done")
  eq(track(3).mute_state, MUTED, "the producer's mute stays")
  eq(track(4).mute_state, MUTED, "Pad as it was")
  eq(track(1).mute_state, A)
  eq(track(2).solo, false)
  ok(done.notes and done.notes[1]:find("changed during the render and left as it was set", 1, true), json.encode(done.notes))
  H.no_dialogs()
end)

test("listening: two producer solos can't both come back through Renoise's exclusive solo: the mutes do, and it is said", function()
  local env, peer = setup()
  -- (a state the GUI can reach: Hat and Pad soloed, the rest OFF)
  for i, t in ipairs(mock.data.tracks) do
    if t.kind ~= "master" then t.mute_state, t.solo = OFF, false end
    if i == 2 or i == 4 then t.mute_state, t.solo = A, true end
  end
  local before = states()
  local epoch = place(peer)
  result((start(peer, "j4", { { id = "kick", what = "track", ref = track_ref(epoch, 1) } })))
  mock.finish_render()
  local done = status(peer, "j4")
  eq(done.state, "done")
  eq(track(2).solo, true, "the first comes back")
  eq(track(4).mute_state, A, "the second plays as it did")
  eq(track(1).mute_state, OFF)
  ok(done.notes and done.notes[1]:find("one track at a time", 1, true), json.encode(done.notes))
  ok(states() ~= before)
  H.no_dialogs()
end)

test("listening: the range is planned in lines: pre-roll clamped at the start, end_pos inclusive, where the part begins said", function()
  local env, peer = setup()
  local answer = result((start(peer, "j5", { { id = "mix", what = "master" } }, { fromBeat = 1, beats = 2 }, { preRollBeats = 4 })))
  local r = mock.renders[1]
  eq(r.options.start_pos.line, 1, "the pre-roll stops at the song's start")
  eq(r.options.end_pos.line, 12, "beat 3 starts line 13: line 12 is the last")
  ok(math.abs(answer.seconds - 1.5) < 1e-9, "12 lines: " .. answer.seconds)
  mock.finish_render()
  local done = status(peer, "j5")
  ok(math.abs(done.files[1].start - 0.5) < 1e-9, "the part begins a beat in: " .. done.files[1].start)
  -- a part across patterns
  result((start(peer, "j6", { { id = "mix", what = "master" } }, { fromBeat = 3.5, beats = 1 }, { preRollBeats = 1 })))
  r = mock.renders[2]
  eq(r.options.start_pos.sequence, 1)
  eq(r.options.start_pos.line, 11, "beat 2.5 is line 11")
  eq(r.options.end_pos.sequence, 2)
  eq(r.options.end_pos.line, 2, "beat 4.5 starts slot 2's line 3")
  mock.finish_render()
  eq(status(peer, "j6").state, "done")
  H.no_dialogs()
end)

test("listening: ZL and ZT commands in any track move the lines' beats and seconds", function()
  local env, peer = setup()
  -- (ZL08 on line 1 of pattern 1, in Hat's effect column: the pattern's 16 lines are 2 beats; ZT3C (60 BPM) at slot 2's line 1)
  local function command(text)
    local digits = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
    return 256 + (digits:find(text:sub(1, 1), 1, true) - 1) * 36 + (digits:find(text:sub(2, 2), 1, true) - 1)
  end
  mock.ops.set_effect(1, 2, 1, 1, command("ZL"), 8)
  mock.ops.set_effect(2, 4, 1, 1, command("ZT"), 0x3C)
  local answer = result((start(peer, "j7", { { id = "mix", what = "master" } }, { fromBeat = 2, beats = 1 })))
  local r = mock.renders[1]
  eq(r.options.start_pos.sequence, 2, "beat 2 is slot 2 (pattern 1 is 2 beats at LPB 8)")
  eq(r.options.start_pos.line, 1)
  eq(r.options.end_pos.line, 8, "a beat at LPB 8 is 8 lines")
  ok(math.abs(answer.seconds - 1) < 1e-9, "a beat at 60 BPM is a second: " .. answer.seconds)
  mock.finish_render()
  eq(status(peer, "j7").state, "done", "the file has the length the commands give")
  H.no_dialogs()
end)

-- Failures put everything back

test("listening: a render that ends with no callback fails, puts the mutes back and deletes its files", function()
  local env, peer = setup()
  track(2).mute_state = MUTED
  local before = states()
  local epoch = place(peer)
  result((start(peer, "j8", { { id = "mix", what = "master" }, { id = "kick", what = "track", ref = track_ref(epoch, 1) } })))
  mock.finish_render()
  mock.finish_render({ no_callback = true })
  ok(exists(env.listening .. "/j8-mix.wav"))
  H.tick(30)
  local failed = status(peer, "j8")
  eq(failed.state, "failed")
  eq(failed.code, "failed")
  eq(failed.error, "Renoise stopped the render")
  eq(states(), before)
  ok(not exists(env.listening .. "/j8-mix.wav") and not exists(env.listening .. "/j8-kick.wav"), "partial files deleted")
  H.no_dialogs()
end)

test("listening: a render cut short calls back but fails: its file is shorter than the lines asked for", function()
  local env, peer = setup()
  local epoch = place(peer)
  result((start(peer, "j9", { { id = "kick", what = "track", ref = track_ref(epoch, 1) } })))
  mock.finish_render({ fraction = 0.5 })
  local failed = status(peer, "j9")
  eq(failed.state, "failed")
  ok(failed.error:find("Renoise stopped the render after 1.0 of 2.0 s", 1, true), failed.error)
  eq(track(1).solo, false)
  eq(track(2).mute_state, A)
  ok(not exists(env.listening .. "/j9-kick.wav"))
  H.no_dialogs()
end)

test("listening: Renoise refusing a render fails the job at once with its words, nothing left changed", function()
  local env, peer = setup()
  mock.render_refuses = "could not open the file"
  local epoch = place(peer)
  local response = start(peer, "j10", { { id = "kick", what = "track", ref = track_ref(epoch, 1) } })
  failure(response, "failed", "Renoise wouldn't render: could not open the file")
  eq(track(1).solo, false)
  eq(track(2).mute_state, A)
  eq(status(peer, "j10").state, "failed")
  H.no_dialogs()
end)

test("listening: listen_cancel lets the render in flight end (never cancel_rendering), then puts everything back and deletes the files", function()
  local env, peer = setup()
  track(3).mute_state = OFF
  local before = states()
  local epoch = place(peer)
  result((start(peer, "j11", { { id = "mix", what = "master" }, { id = "kick", what = "track", ref = track_ref(epoch, 1) } })))
  mock.finish_render()
  local cancel = result(call(peer, "listen_cancel", { jobId = "j11" }), "listen_cancel")
  eq(cancel.state, "running")
  eq(cancel.stopping, true)
  eq(status(peer, "j11").stopping, true)
  mock.finish_render()
  eq(#mock.renders, 2, "nothing more was rendered")
  local cancelled = status(peer, "j11")
  eq(cancelled.state, "cancelled")
  ok(cancelled.notes and cancelled.notes[1]:find("Renoise finished the render before it could stop", 1, true), json.encode(cancelled.notes))
  eq(states(), before)
  ok(not exists(env.listening .. "/j11-mix.wav") and not exists(env.listening .. "/j11-kick.wav"))
  -- (the mock records cancel_rendering as a crash, which fails any test that calls it)
  H.no_dialogs()
end)

test("listening: listen_cancel of a job that ended answers its end; of an unknown job is bad-params", function()
  local env, peer = setup()
  result((start(peer, "j12", { { id = "mix", what = "master" } })))
  mock.finish_render()
  eq(result(call(peer, "listen_cancel", { jobId = "j12" })).state, "done")
  failure(call(peer, "listen_cancel", { jobId = "j99" }), "bad-params")
  failure(call(peer, "listen_status", { jobId = "j99" }), "bad-params")
  H.no_dialogs()
end)

-- Busy, stale, the demo

test("listening: while Kumi renders, only ping, status, focus and the job's own requests are answered; the rest are busy", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  result((start(peer, "j13", { { id = "mix", what = "master" } })))
  local preview = call(peer, "change_preview", { kind = "track-volume", input = { ref = track_ref(epoch, 1), volumeDb = -6 }, epoch = epoch, layout = layout })
  failure(preview, "busy", "Renoise is rendering for Kumi")
  failure(call(peer, "read", { kind = "track" }), "busy")
  failure(call(peer, "render_start", { jobId = "j14", sources = { { id = "mix", what = "master" } }, range = { fromBeat = 0, beats = 1 }, epoch = epoch }), "busy")
  eq(result(call(peer, "ping")).daw, nil)
  local s = result(call(peer, "status"))
  eq(s.busy, "rendering")
  eq(s.busyReason, "Renoise is rendering for Kumi")
  eq(result(call(peer, "focus")).transport.playing, false, "Renoise reads as playing while it renders; Kumi's render isn't playback")
  mock.finish_render()
  eq(status(peer, "j13").state, "done")
  ok(result(call(peer, "change_preview", { kind = "track-volume", input = { ref = track_ref(epoch, 1), volumeDb = -6 }, epoch = epoch, layout = layout })))
  H.no_dialogs()
end)

test("listening: the transport event of Renoise's own render is held back; a render while playing stops it and says so", function()
  local env, peer = setup({}, { playing = true })
  peer.events = {}
  result((start(peer, "j15", { { id = "mix", what = "master" } })))
  H.tick(3)
  for _ = 1, 3 do call(peer, "ping") end
  mock.finish_render()
  H.tick(3)
  call(peer, "ping")
  local transport = {}
  for _, event in ipairs(peer.events) do
    if event.name == "transport" then transport[#transport + 1] = event.data.playing end
  end
  eq(#transport, 1, "one event, after the job: " .. json.encode(transport))
  eq(transport[1], false, "playback stopped")
  local done = status(peer, "j15")
  ok(done.notes and done.notes[1] == "Renoise stopped playback to render.", json.encode(done.notes))
  H.no_dialogs()
end)

test("listening: render_start checks everything before writing: refs of another epoch, bad sources, rates, ranges, the hour", function()
  local env, peer = setup()
  local epoch = place(peer)
  local function bad(sources, range, more, code, pattern)
    local response = start(peer, "j16", sources, range, more)
    failure(response, code or "bad-params", pattern)
    eq(#mock.renders, 0)
    eq(track(1).solo, false)
  end
  bad({ { id = "kick", what = "track", ref = "1a:track:1" } }, nil, nil, "stale")
  bad({ { id = "mix", what = "master", beforeChain = true } }, nil, nil, "bad-params", "Renoise renders the mix after its Master track's devices")
  bad({ { id = "m", what = "track", ref = track_ref(epoch, 5) } }, nil, nil, "bad-params", "the Master track is the mix")
  bad({ { id = "a", what = "master" }, { id = "a", what = "master" } }, nil, nil, "bad-params", "two sources are named a")
  bad({ { id = "bad id!", what = "master" } })
  local many = {}
  for i = 1, 19 do many[i] = { id = "s" .. i, what = "master" } end
  bad(many)
  bad({ { id = "mix", what = "master" } }, nil, { sampleRate = 22050 }, "bad-params", "sampleRate")
  bad({ { id = "mix", what = "master" } }, { fromBeat = 0, beats = 0 })
  bad({ { id = "mix", what = "master" } }, { fromBeat = -1, beats = 1 })
  bad({ { id = "mix", what = "master" } }, { fromBeat = 12, beats = 1 }, nil, "bad-params", "the song ends at beat 12")
  failure(call(peer, "render_start", { jobId = "x1", sources = { { id = "mix", what = "master" } }, range = { fromBeat = 0, beats = 1 }, epoch = epoch }), "bad-params", "jobId")
  -- the hour: a song of 300 patterns of 512 lines at 32 BPM is far over it
  H.no_dialogs()
end)

test("listening: more than an hour of music is refused", function()
  -- (4 patterns of 512 lines at 32 BPM and LPB 1: 2,048 beats, 64 minutes)
  local env, peer = setup({}, { bpm = 32, lpb = 1, patterns = 4, pattern_lines = 512, sequence_length = 4, sequence = { 1, 2, 3, 4 } })
  local response = start(peer, "j17", { { id = "mix", what = "master" } }, { fromBeat = 0, beats = 2048 })
  failure(response, "bad-params", "Kumi renders at most 60 at a time")
  eq(#mock.renders, 0)
  H.no_dialogs()
end)

test("listening: the demo can't render and says why; status says render is off", function()
  local env, peer = setup({ version = "3.5.4 Demo" })
  local s = result(call(peer, "status"))
  eq(s.listening.render, false)
  eq(s.listening.audition, false)
  eq(s.listening.version, 1)
  local response = start(peer, "j18", { { id = "mix", what = "master" } })
  failure(response, "failed", "The Renoise demo can't render to a file, so Kumi can't hear the song: a licensed Renoise can")
  eq(#mock.renders, 0)
  H.no_dialogs()
end)

test("listening: status gives the listening flags", function()
  local env, peer = setup()
  local s = result(call(peer, "status"))
  eq(s.listening.render, true)
  eq(s.listening.capture, false)
  eq(s.listening.audition, true)
  eq(s.listening.grid, false)
  eq(s.listening.scratch, true)
  eq(s.listening.version, 1)
  H.no_dialogs()
end)

test("listening: a song closed during the render fails the job stale, deletes its files and touches nothing of the new song", function()
  local env, peer = setup()
  local epoch = place(peer)
  result((start(peer, "j19", { { id = "mix", what = "master" }, { id = "kick", what = "track", ref = track_ref(epoch, 1) } })))
  mock.finish_render()
  local old = mock.pending_render
  mock.load_song(song_options({ name = "Other" }))
  mock.pending_render = old
  mock.impl(mock.song).rendering = true
  H.tick()
  mock.finish_render()
  local failed = status(peer, "j19")
  eq(failed.state, "failed")
  eq(failed.code, "stale")
  ok(not exists(env.listening .. "/j19-mix.wav") and not exists(env.listening .. "/j19-kick.wav"))
  eq(states(), "1=1 2=1 3=1 4=1 6=1", "the new song untouched")
  H.no_dialogs()
end)

test("listening: the tool stopped during a job puts the mutes back now and answers the job's end after a reload", function()
  local env, peer = setup()
  track(2).mute_state = MUTED
  local before = states()
  local epoch = place(peer)
  result((start(peer, "j20", { { id = "kick", what = "track", ref = track_ref(epoch, 1) } })))
  eq(track(1).solo, true)
  -- (Renoise reloads its tools: the payload stops, and starts again)
  KUMI_BRIDGE.payload.stop({ bundle_path = env.bundle, state = KUMI_BRIDGE, tick_ms = 50, close_server = function() end }, true)
  eq(states(), before, "put back at once")
  mock.finish_render()
  ok(not exists(env.listening .. "/j20-kick.wav"), "the late file is deleted")
  H.no_dialogs()
end)
