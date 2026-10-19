-- Kumi's changes to a Renoise song (M3): preview, apply, restore, the producer's undo noticed, the emergency stop. Against the
-- mock Renoise, which keeps an undo step for every callback that writes (and none for mute, solo, loop or selection).
local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness

local next_id = 5000

local function drain(peer)
  repeat
    local taken = peer:take()
    if #taken > 0 then H.send(peer, { type = "credit", bytes = #taken }) end
  until #taken == 0
end

-- Sends a request and runs the clock until its response comes; the events that came meanwhile are kept in `peer.events`.
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

-- A song of five tracks (Kick, Hat, Bass, Pad, a group) with the mixer device every Renoise track has, a master and a send.
local function song_options(extra)
  local spec = {
    name = "Lab", file_name = "/s/lab.xrns", bpm = 120, lpb = 4, mixer_device = true,
    tracks = {
      { name = "Kick", color = { 255, 0, 0 } }, { name = "Hat" }, { name = "Bass", note_columns = 2 }, { name = "Pad" },
    },
    send_tracks = 1, patterns = 3, pattern_lines = 32, sequence_length = 4, sequence = { 1, 2, 1, 3 }, instruments = 1,
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
  return env, peer
end

local function place(peer)
  local focus = call(peer, "focus").result
  return focus.epoch, focus.layout
end

local function track_ref(epoch, i) return epoch .. ":track:" .. i end

local function ask(peer, method, kind, input, more)
  local epoch, layout = place(peer)
  local params = { kind = kind, input = input, epoch = epoch, layout = layout }
  for k, v in pairs(more or {}) do params[k] = v end
  return call(peer, method, params), epoch, layout
end

local function preview(peer, kind, input)
  local response, epoch, layout = ask(peer, "change_preview", kind, input)
  ok(response and response.result, kind .. " preview: " .. json.encode(response and response.error or {}))
  return response.result, epoch, layout
end

-- The step count of the song's undo, and the newest step's name.
local function steps() return #mock.data.undo.stack end
local function newest() return mock.data.undo.stack[#mock.data.undo.stack] and mock.data.undo.stack[#mock.data.undo.stack].desc end

local function apply(peer, kind, input, desc, fence, epoch, layout)
  return call(peer, "change_apply", { kind = kind, input = input, fence = fence, desc = desc, epoch = epoch, layout = layout })
end

local function restore(peer, kind, record, desc, epoch, layout, more)
  local params = {
    kind = kind, target = record.target, prior = record.prior, applied = record.applied, created = record.created,
    material = record.material, desc = desc, epoch = epoch, layout = layout,
  }
  for k, v in pairs(more or {}) do params[k] = v end
  return call(peer, "change_restore", params)
end

local function failure(response, code)
  ok(response and response.error, "expected an error")
  eq(response.error.code, code, "error code (" .. tostring(response.error.message) .. ")")
end

local function track_data(i) return mock.data.tracks[i] end

-- Preview, apply

test("changes: a preview writes nothing and says what a change would do, with true minus signs", function()
  local env, peer = setup()
  local before = steps()
  local result, epoch, layout = preview(peer, "track-volume", { ref = track_ref(place(peer), 1), volumeDb = -6 })
  eq(result.epoch, epoch)
  eq(result.layout, layout)
  eq(result.target.ref, track_ref(epoch, 1))
  eq(result.target.kind, "track")
  eq(result.prior.volume, 1)
  eq(result.prior.stage, "pre")
  eq(result.fence.volume, 1)
  eq(result.display.title, "Kick volume \226\136\1466 dB")
  eq(result.display.unchanged, nil)
  eq(steps(), before, "no undo step")
  eq(track_data(1).volume, 1, "nothing written")
  local same = preview(peer, "track-volume", { ref = track_ref(epoch, 1), volumeDb = 0 })
  eq(same.display.unchanged, true)
  eq(same.display.title, "Kick volume 0 dB")
  H.no_dialogs()
end)

test("changes: an apply makes one undo step named by the host's description and reads back what it wrote", function()
  local env, peer = setup()
  local epoch = place(peer)
  local ref = track_ref(epoch, 2)
  local previewed, _, layout = preview(peer, "track-volume", { ref = ref, volumeDb = -6 })
  local done = apply(peer, "track-volume", { ref = ref, volumeDb = -6 }, "Kumi: Hat volume \226\136\1466 dB (k7f3)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(done.result.epoch, epoch)
  eq(done.result.layout, layout)
  ok(math.abs(done.result.applied.volume - 10 ^ (-6 / 20)) < 1e-6)
  eq(done.result.undo.desc, "Kumi: Hat volume \226\136\1466 dB (k7f3)")
  eq(done.result.undo.entry, true)
  eq(done.result.display.title, "Hat volume \226\136\1466 dB")
  eq(steps(), 1, "one step for the whole change")
  eq(newest(), "Kumi: Hat volume \226\136\1466 dB (k7f3)")
  ok(math.abs(track_data(2).volume - 10 ^ (-6 / 20)) < 1e-6, "the track has it")
  H.no_dialogs()
end)

test("changes: an apply is refused before anything is written when the fence fails, the epoch is old or Renoise is busy", function()
  local env, peer = setup()
  local epoch = place(peer)
  local ref = track_ref(epoch, 1)
  local previewed, _, layout = preview(peer, "track-volume", { ref = ref, volumeDb = -6 })
  -- the producer moves the fader first
  mock.ops.rename_track(1, "Kick") -- (a bystander)
  track_data(1).volume = 0.5
  local changed = apply(peer, "track-volume", { ref = ref, volumeDb = -6 }, "Kumi: x (k1)", previewed.fence, epoch, layout)
  failure(changed, "changed")
  eq(track_data(1).volume, 0.5, "left as the producer had it")
  eq(steps(), 0)
  track_data(1).volume = 1
  -- an old epoch
  local stale = apply(peer, "track-volume", { ref = ref, volumeDb = -6 }, "Kumi: x (k1)", previewed.fence, "0", layout)
  failure(stale, "stale")
  -- busy
  mock.data.song_fields.rendering = true
  local busy = apply(peer, "track-volume", { ref = ref, volumeDb = -6 }, "Kumi: x (k1)", previewed.fence, epoch, layout)
  failure(busy, "busy")
  mock.data.song_fields.rendering = false
  eq(steps(), 0)
  eq(track_data(1).volume, 1)
  H.no_dialogs()
end)

test("changes: bad inputs are bad-params, the master has no name or solo, and an unknown kind is refused", function()
  local env, peer = setup()
  local epoch = place(peer)
  local function bad(kind, input)
    local response = ask(peer, "change_preview", kind, input)
    failure(response, "bad-params")
  end
  bad("track-volume", { ref = track_ref(epoch, 1), volumeDb = "loud" })
  bad("track-volume", { ref = track_ref(epoch, 1), volumeDb = 40 })
  bad("track-pan", { ref = track_ref(epoch, 1), pan = 2 })
  bad("track-mute", { ref = track_ref(epoch, 1), mute = "yes" })
  bad("track-color", { ref = track_ref(epoch, 1), color = "red" })
  bad("track-rename", { ref = track_ref(epoch, 1), name = "two\nlines" })
  bad("track-rename", { ref = track_ref(epoch, 5), name = "Master" }) -- (the master is the fifth: four tracks, then it)
  bad("track-solo", { ref = track_ref(epoch, 5), solo = true })
  bad("track-delete", { ref = track_ref(epoch, 5) })
  bad("track-fly", { ref = track_ref(epoch, 1) })
  bad("track-volume", { ref = "not a ref", volumeDb = -3 })
  failure(call(peer, "change_preview", { kind = "track-volume", input = { ref = track_ref(epoch, 1), volumeDb = -3 } }), "bad-params")
  H.no_dialogs()
end)

test("changes: pan, rename and colour change and put back, each as one step", function()
  local env, peer = setup()
  local epoch = place(peer)
  local function round_trip(kind, input, read, expected, original)
    local ref = input.ref
    local previewed, _, layout = preview(peer, kind, input)
    local before = steps()
    local done = apply(peer, kind, input, "Kumi: " .. kind .. " (k" .. kind:len() .. ")", previewed.fence, epoch, layout)
    ok(done.result, kind .. ": " .. json.encode(done.error or {}))
    eq(steps(), before + 1, kind .. ": one step")
    eq(read(), expected, kind .. " written")
    local undone = restore(peer, kind, {
      target = previewed.target, prior = previewed.prior, applied = done.result.applied,
    }, "Kumi: undo " .. kind .. " (k9)", epoch, layout)
    ok(undone.result, kind .. " restore: " .. json.encode(undone.error or {}))
    eq(read(), original, kind .. " put back")
    eq(steps(), before + 2, kind .. ": the restore is a new step")
    eq(newest(), "Kumi: undo " .. kind .. " (k9)")
    return previewed, done.result
  end
  local _, pan = round_trip("track-pan", { ref = track_ref(epoch, 3), pan = -0.5 }, function() return track_data(3).panning end, 0.25, 0.5)
  eq(pan.display.title, "Bass pan 50% left")
  local _, rename = round_trip("track-rename", { ref = track_ref(epoch, 4), name = "Pad 2" }, function() return track_data(4).name end, "Pad 2", "Pad")
  eq(rename.display.title, "Renamed track Pad to Pad 2")
  local _, colour = round_trip("track-color", { ref = track_ref(epoch, 2), color = "#336699" }, function()
    local c = track_data(2).color
    return c and string.format("#%02x%02x%02x", c[1], c[2], c[3])
  end, "#336699", "#29a699")
  eq(colour.display.title, "Hat colour changed")
  H.no_dialogs()
end)

test("changes: mute and solo are not Renoise undo steps, say so, and are put back by Kumi", function()
  local env, peer = setup()
  local epoch = place(peer)
  local ref = track_ref(epoch, 1)
  local previewed, _, layout = preview(peer, "track-mute", { ref = ref, mute = true })
  ok(previewed.display.note:find("Renoise's own undo won't take this back", 1, true), previewed.display.note)
  local done = apply(peer, "track-mute", { ref = ref, mute = true }, "Kumi: Kick muted (k2)", previewed.fence, epoch, layout)
  eq(done.result.undo.entry, false)
  ok(done.result.display.note)
  eq(steps(), 0, "no step in Renoise")
  eq(track_data(1).mute_state, 3)
  local undone = restore(peer, "track-mute", { target = previewed.target, prior = previewed.prior, applied = done.result.applied }, "Kumi: undo (k3)", epoch, layout)
  eq(undone.result.undo.entry, false)
  eq(track_data(1).mute_state, 1)
  local solo = preview(peer, "track-solo", { ref = ref, solo = true })
  local soloed = apply(peer, "track-solo", { ref = ref, solo = true }, "Kumi: Kick soloed (k4)", solo.fence, epoch, layout)
  eq(soloed.result.undo.entry, false)
  eq(track_data(1).solo, true)
  eq(steps(), 0)
  H.no_dialogs()
end)

test("changes: a restore refuses `changed` when the producer moved the value, accepts discard, and a value already back is not written", function()
  local env, peer = setup()
  local epoch = place(peer)
  local ref = track_ref(epoch, 1)
  local previewed, _, layout = preview(peer, "track-rename", { ref = ref, name = "Boom" })
  local done = apply(peer, "track-rename", { ref = ref, name = "Boom" }, "Kumi: rename (k5)", previewed.fence, epoch, layout)
  local record = { target = previewed.target, prior = previewed.prior, applied = done.result.applied }
  mock.ops.rename_track(1, "Producer's")
  local before = steps()
  failure(restore(peer, "track-rename", record, "Kumi: undo (k6)", epoch, layout), "changed")
  eq(track_data(1).name, "Producer's")
  eq(steps(), before)
  local forced = restore(peer, "track-rename", record, "Kumi: undo (k6)", epoch, layout, { discard = true })
  ok(forced.result, json.encode(forced.error or {}))
  eq(track_data(1).name, "Kick")
  -- a retry of a restore that went through writes nothing
  local after = steps()
  local again = restore(peer, "track-rename", record, "Kumi: undo (k6)", epoch, layout)
  ok(again.result)
  eq(steps(), after, "nothing written, no step")
  -- a track that is gone
  mock.ops.remove_track(1)
  failure(restore(peer, "track-rename", record, "Kumi: undo (k7)", epoch, layout), "gone")
  H.no_dialogs()
end)

test("changes: refs made in an earlier layout are carried to where the tracks went", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local ref = track_ref(epoch, 3) -- Bass
  mock.ops.insert_track(1, { name = "New" })
  local response = call(peer, "change_preview", { kind = "track-rename", input = { ref = ref, name = "Bass 2" }, epoch = epoch, layout = layout })
  ok(response.result, json.encode(response.error or {}))
  eq(response.result.target.ref, track_ref(epoch, 4), "a request that names the old layout is carried through the new track: Bass is the fourth")
  eq(response.result.layout, layout + 1)
  eq(response.result.prior.name, "Bass")
  H.no_dialogs()
end)

-- The producer's undo

test("changes: the producer's undo and redo of a Kumi change are told, once, by the description", function()
  local env, peer = setup()
  local epoch = place(peer)
  local ref = track_ref(epoch, 1)
  local previewed, _, layout = preview(peer, "track-volume", { ref = ref, volumeDb = -6 })
  local desc = "Kumi: Kick volume \226\136\1466 dB (k7f3)"
  apply(peer, "track-volume", { ref = ref, volumeDb = -6 }, desc, previewed.fence, epoch, layout)
  H.tick(2)
  peer.events = {}
  mock.song:undo()
  H.tick(3)
  drain(peer)
  local told = H.events(peer, "undone_in_daw")
  eq(#told, 1)
  eq(told[1].data.how, "undo")
  eq(told[1].data.descs[1], desc)
  eq(told[1].data.epoch, epoch)
  eq(track_data(1).volume, 1)
  mock.song:redo()
  H.tick(3)
  drain(peer)
  told = H.events(peer, "undone_in_daw")
  eq(#told, 1)
  eq(told[1].data.how, "redo")
  eq(told[1].data.descs[1], desc)
  -- told once: nothing moves, nothing more is told
  H.tick(5)
  eq(#H.events(peer, "undone_in_daw"), 0)
  H.no_dialogs()
end)

test("changes: the producer's own edit is not told, and neither is Kumi's own restore", function()
  local env, peer = setup()
  local epoch = place(peer)
  local ref = track_ref(epoch, 1)
  local previewed, _, layout = preview(peer, "track-volume", { ref = ref, volumeDb = -6 })
  local done = apply(peer, "track-volume", { ref = ref, volumeDb = -6 }, "Kumi: v (k8)", previewed.fence, epoch, layout)
  H.tick(2)
  H.events(peer, "undone_in_daw")
  -- the producer sets the same value by hand: no undo, no word
  mock.ops.rename_track(1, "Kick again")
  H.tick(3)
  eq(#H.events(peer, "undone_in_daw"), 0)
  -- Kumi's restore is an ordinary write
  restore(peer, "track-volume", { target = previewed.target, prior = previewed.prior, applied = done.result.applied }, "Kumi: undo v (k9)", epoch, layout)
  H.tick(3)
  eq(#H.events(peer, "undone_in_daw"), 0)
  -- the producer's undo of that restore brings the change back: told as a redo
  mock.song:undo()
  H.tick(3)
  drain(peer)
  local told = H.events(peer, "undone_in_daw")
  eq(#told, 1)
  eq(told[1].data.how, "redo")
  eq(told[1].data.descs[1], "Kumi: v (k8)")
  H.no_dialogs()
end)

-- Deleting a track

local function dense_song()
  local env, peer = setup()
  mock.device_catalog = { ["Audio/Effects/Native/EQ 5"] = { { name = "Gain", value = 0.5, min = 0, max = 1 } } }
  local song = mock.song
  -- Bass (track 3) gets two devices, lines in two patterns, a slot mute, a column name and an automation
  local bass = song:track(3)
  local eq5 = bass:insert_device_at("Audio/Effects/Native/EQ 5", 2)
  local gain = bass:insert_device_at("Audio/Effects/Native/Gainer", 3)
  mock.data.tracks[3].devices[2].params = { { name = "Gain", value = 0.5, min = 0, max = 1 } }
  mock.data.tracks[3].devices[2].preset_data = "<preset eq5/>"
  mock.data.tracks[3].devices[3].display_name = "Louder"
  mock.data.tracks[3].devices[3].active = false
  for _, spec in ipairs({ { 1, 1, 1, 48 }, { 1, 5, 2, 51 }, { 3, 9, 1, 40 } }) do
    local pattern_track = song:pattern(spec[1]):track(3)
    local column = pattern_track:line(spec[2]):note_column(spec[3])
    column.note_value, column.instrument_value, column.volume_value = spec[4], 1, 64
  end
  local effect = song:pattern(2):track(3):line(2):effect_column(1)
  effect.number_value, effect.amount_value = 10, 32
  song:track(3).visible_note_columns = 3
  song:track(3):set_column_name(2, "Octave")
  song.sequencer:set_track_sequence_slot_is_muted(3, 2, true)
  song:track(3).prefx_volume.value = 0.8
  song:track(3).postfx_panning.value = 0.3
  mock.ops.automate(1, 3, 2, 1, { { time = 1, value = 0.2, scaling = 0 }, { time = 9, value = 0.9, scaling = 0 } })
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  return env, peer
end

local function snapshot_of_track(index)
  local t = mock.data.tracks[index]
  -- (what Renoise fills in by itself is its default whether it is set or not)
  local copy = { name = t.name, volume = t.volume, panning = t.panning, post_panning = t.post_panning or 0.5, post_volume = t.post_volume or 1,
    note_columns = t.note_columns, muted_columns = t.muted_columns or {}, column_names = t.column_names or {}, mute_state = t.mute_state,
    devices = {}, patterns = {}, color = t.color or { 41, 166, 153 }, color_blend = t.color_blend or 0, solo = t.solo == true,
    effect_columns = t.effect_columns }
  for i, device in ipairs(t.devices) do
    copy.devices[i] = { path = device.path, name = device.name, active = device.active ~= false, display_name = device.display_name, preset_data = device.preset_data }
  end
  for p, pattern in ipairs(mock.data.patterns) do
    local pt = pattern.tracks[index]
    local lines = {}
    for k, l in pairs(pt and pt.lines or {}) do lines[k] = json.encode(l) end
    copy.patterns[p] = lines
  end
  return json.encode(copy)
end

test("changes: deleting a track keeps its material, and the restore makes the same track again", function()
  local env, peer = dense_song()
  local epoch = place(peer)
  local ref = track_ref(epoch, 3)
  local before = snapshot_of_track(3)
  local previewed, _, layout = preview(peer, "track-delete", { ref = ref })
  eq(previewed.display.title, "Deleted track Bass")
  ok(previewed.display.note:find("2 devices", 1, true), previewed.display.note)
  eq(previewed.prior.name, "Bass")
  local done = apply(peer, "track-delete", { ref = ref }, "Kumi: Deleted track Bass (kd01)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(done.result.applied.deleted, true)
  eq(done.result.layout, layout + 1, "the removal moved the layout")
  eq(done.result.undo.entry, true)
  eq(steps(), 1)
  eq(#mock.data.tracks, 5, "the track is gone")
  local material = done.result.material
  eq(material.name, "Bass")
  eq(material.index, 3)
  eq(#material.devices, 2)
  eq(material.devices[1].path, "Audio/Effects/Native/EQ 5")
  eq(material.devices[1].presetData, "<preset eq5/>")
  eq(material.devices[2].active, false)
  eq(material.devices[2].display, "Louder")
  eq(#material.patterns, 3, "the lines of three patterns (two with notes, one with an effect)")
  eq(material.slotMutes[1], 2)
  eq(material.columns.names["2"], "Octave")
  eq(#material.automation, 1)
  eq(material.automation[1].points[2].value, 0.9)
  ok(math.abs(material.volume.pre - 0.8) < 1e-9)
  -- restore, with the layout the apply answered
  local undone = restore(peer, "track-delete", { target = previewed.target, prior = previewed.prior, applied = done.result.applied, material = material },
    "Kumi: undo Deleted track Bass (kd02)", epoch, done.result.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(undone.result.restored.ref, track_ref(undone.result.epoch, 3), "(its devices moved the epoch as they were made)")
  eq(undone.result.restored.name, "Bass")
  eq(undone.result.display.title, "Track Bass put back")
  eq(undone.result.undo.entry, true)
  eq(steps(), 2, "the restore is one step")
  eq(newest(), "Kumi: undo Deleted track Bass (kd02)")
  eq(snapshot_of_track(3), before, "the track is what it was")
  H.no_dialogs()
end)

test("changes: a track delete's restore finds its place after tracks were made and deleted since", function()
  local env, peer = dense_song()
  local epoch = place(peer)
  local previewed, _, layout = preview(peer, "track-delete", { ref = track_ref(epoch, 3) })
  local done = apply(peer, "track-delete", { ref = track_ref(epoch, 3) }, "Kumi: d (kd03)", previewed.fence, epoch, layout)
  -- the producer makes a track in front of it
  mock.ops.insert_track(1, { name = "Intro" })
  local undone = restore(peer, "track-delete", { target = previewed.target, prior = previewed.prior, applied = done.result.applied, material = done.result.material },
    "Kumi: undo d (kd04)", epoch, done.result.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(undone.result.restored.ref, track_ref(undone.result.epoch, 4), "Bass goes back after Intro, Kick and Hat")
  eq(mock.data.tracks[4].name, "Bass")
  eq(mock.data.tracks[1].name, "Intro")
  H.no_dialogs()
end)

test("changes: a track delete is refused `changed` when the track changed since the preview, and a group or the master is refused", function()
  local env, peer = dense_song()
  local epoch = place(peer)
  local previewed, _, layout = preview(peer, "track-delete", { ref = track_ref(epoch, 3) })
  mock.song:track(3).visible_note_columns = 5
  failure(apply(peer, "track-delete", { ref = track_ref(epoch, 3) }, "Kumi: d (kd05)", previewed.fence, epoch, layout), "changed")
  eq(#mock.data.tracks, 6, "nothing deleted")
  failure(ask(peer, "change_preview", "track-delete", { ref = track_ref(epoch, 5) }), "bad-params")
  H.no_dialogs()
end)

test("changes: a restore finds the track there already and says changed, unless it is told to discard", function()
  local env, peer = dense_song()
  local epoch = place(peer)
  local previewed, _, layout = preview(peer, "track-delete", { ref = track_ref(epoch, 3) })
  local done = apply(peer, "track-delete", { ref = track_ref(epoch, 3) }, "Kumi: d (kd06)", previewed.fence, epoch, layout)
  local record = { target = previewed.target, prior = previewed.prior, applied = done.result.applied, material = done.result.material }
  mock.ops.insert_track(3, { name = "Bass" })
  failure(restore(peer, "track-delete", record, "Kumi: undo d (kd07)", epoch, done.result.layout), "changed")
  H.no_dialogs()
end)

test("changes: a restore too big for one callback is numbered steps, each its own, and ends with the whole track", function()
  local env, peer = setup({ patch = function(env)
    local path = env.bundle .. "payload/changes.lua"
    local f = assert(io.open(path, "rb"))
    local text = f:read("*a")
    f:close()
    local lowered = text:gsub("M%.SLICE_WRITES = %d+", "M.SLICE_WRITES = 40")
    assert(lowered ~= text, "the slice size was not found to lower")
    f = assert(io.open(path, "wb"))
    f:write(lowered)
    f:close()
  end })
  local song = mock.song
  for p = 1, 3 do
    for k = 1, 10 do
      local column = song:pattern(p):track(2):line(k):note_column(1)
      column.note_value, column.instrument_value, column.volume_value = 40 + k, 1, 64
    end
  end
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local epoch = place(peer)
  local previewed, _, layout = preview(peer, "track-delete", { ref = track_ref(epoch, 2) })
  local before = snapshot_of_track(2)
  local done = apply(peer, "track-delete", { ref = track_ref(epoch, 2) }, "Kumi: d (ks01)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(steps(), 1)
  local undone = restore(peer, "track-delete", { target = previewed.target, prior = previewed.prior, applied = done.result.applied, material = done.result.material },
    "Kumi: undo d (ks02)", epoch, done.result.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(undone.result.undo.steps, 3, "three numbered steps")
  eq(steps(), 4, "the delete, then three")
  eq(mock.data.undo.stack[2].desc, "Kumi: undo d (ks02) (1/3)")
  eq(mock.data.undo.stack[3].desc, "Kumi: undo d (ks02) (2/3)")
  eq(mock.data.undo.stack[4].desc, "Kumi: undo d (ks02) (3/3)")
  eq(snapshot_of_track(2), before, "all of it is back")
  -- each step undoes exactly its own part: the last one takes back one pattern's lines and nothing else
  mock.song:undo()
  ok(mock.song:pattern(3):track(2).is_empty, "the last step's lines are gone")
  eq(mock.data.tracks[2].name, "Hat", "the track itself stays")
  ok(not mock.song:pattern(1):track(2).is_empty, "the first step's lines stay")
  ok(not mock.song:pattern(2):track(2).is_empty, "so do the second's")
  H.no_dialogs()
end)

test("changes: a sliced restore that is stopped by a busy Renoise takes back the track it made", function()
  local env, peer = setup({ patch = function(env)
    local path = env.bundle .. "payload/changes.lua"
    local f = assert(io.open(path, "rb"))
    local text = f:read("*a")
    f:close()
    f = assert(io.open(path, "wb"))
    f:write((text:gsub("M%.SLICE_WRITES = %d+", "M.SLICE_WRITES = 40")))
    f:close()
  end })
  local song = mock.song
  for p = 1, 3 do
    for k = 1, 10 do
      local column = song:pattern(p):track(2):line(k):note_column(1)
      column.note_value, column.instrument_value, column.volume_value = 40 + k, 1, 64
    end
  end
  local epoch = place(peer)
  local previewed, _, layout = preview(peer, "track-delete", { ref = track_ref(epoch, 2) })
  local done = apply(peer, "track-delete", { ref = track_ref(epoch, 2) }, "Kumi: d (ks03)", previewed.fence, epoch, layout)
  next_id = next_id + 1
  local id = next_id
  H.send(peer, { type = "request", id = id, method = "change_restore", params = {
    kind = "track-delete", target = previewed.target, prior = previewed.prior, applied = done.result.applied,
    material = done.result.material, desc = "Kumi: undo d (ks04)", epoch = epoch, layout = done.result.layout } })
  mock.advance(10) -- the first step runs
  eq(mock.data.tracks[2].name, "Hat", "the track is made")
  ok(mock.song:pattern(3):track(2).is_empty, "and only its first step has run")
  mock.data.song_fields.rendering = true
  local response
  for _ = 1, 100 do
    mock.advance(20)
    drain(peer)
    for _, line in ipairs(peer:frames(nil)) do
      local frame = assert(json.decode(line))
      if frame.type == "response" and frame.id == id then response = frame end
    end
    if response then break end
  end
  mock.data.song_fields.rendering = false
  ok(response and response.error, "the restore did not finish")
  eq(response.error.code, "busy")
  eq(#mock.data.tracks, 5, "the track the restore made is gone again")
  H.no_dialogs()
end)

-- Transport and steps

test("changes: the emergency stop stops what plays and says whether it was recording", function()
  local env, peer = setup()
  local idle = call(peer, "transport_stop").result
  eq(idle.stopped, false)
  eq(idle.wasRecording, false)
  mock.ops.transport("playing", true)
  local playing = call(peer, "transport_stop").result
  eq(playing.stopped, true)
  eq(playing.wasRecording, false)
  H.no_dialogs()
end)

test("changes: a plan's step in Renoise is not merged: every change stays its own step", function()
  local env, peer = setup()
  local begun = call(peer, "undo_step_begin", { label = "Kumi: a plan (kp01)" }).result
  ok(begun.stepId)
  eq(begun.merged, false)
  failure(call(peer, "undo_step_end", { stepId = "nope" }), "failed")
  local ended = call(peer, "undo_step_end", { stepId = begun.stepId }).result
  eq(ended.desc, "Kumi: a plan (kp01)")
  eq(ended.entry, false)
  eq(ended.merged, false)
  failure(call(peer, "undo_step_begin", {}), "bad-params")
  H.no_dialogs()
end)

test("changes: the producer's undo of a Kumi track delete brings the track back and is told, and a redo is told too", function()
  local env, peer = dense_song()
  local epoch = place(peer)
  local previewed, _, layout = preview(peer, "track-delete", { ref = track_ref(epoch, 3) })
  local desc = "Kumi: Deleted track Bass (kd08)"
  local done = apply(peer, "track-delete", { ref = track_ref(epoch, 3) }, desc, previewed.fence, epoch, layout)
  ok(done.result)
  H.tick(2)
  H.events(peer, "undone_in_daw")
  mock.song:undo()
  H.tick(3)
  drain(peer)
  local told = H.events(peer, "undone_in_daw")
  eq(#told, 1, "told once")
  eq(told[1].data.how, "undo")
  eq(told[1].data.descs[1], desc)
  eq(mock.data.tracks[3].name, "Bass", "Renoise brought it back")
  mock.song:redo()
  H.tick(3)
  drain(peer)
  told = H.events(peer, "undone_in_daw")
  eq(#told, 1)
  eq(told[1].data.how, "redo")
  eq(#mock.data.tracks, 5)
  -- Kumi puts it back itself, then the producer takes that step back: the delete is done again, which is told as a redo
  local undone = restore(peer, "track-delete", { target = previewed.target, prior = previewed.prior, applied = done.result.applied, material = done.result.material },
    "Kumi: undo d (kd09)", epoch, done.result.layout)
  ok(undone.result, json.encode(undone.error or {}))
  H.tick(3)
  eq(#H.events(peer, "undone_in_daw"), 0, "Kumi's own restore is not the producer's undo")
  mock.song:undo()
  H.tick(3)
  drain(peer)
  told = H.events(peer, "undone_in_daw")
  eq(#told, 1)
  eq(told[1].data.how, "redo")
  eq(told[1].data.descs[1], desc)
  H.no_dialogs()
end)

test("changes: a producer's insert of a track moves the places the journal knows, so a later undo is matched to the right track", function()
  local env, peer = setup()
  local epoch = place(peer)
  local ref = track_ref(epoch, 3)
  local previewed, _, layout = preview(peer, "track-rename", { ref = ref, name = "Bass 2" })
  apply(peer, "track-rename", { ref = ref, name = "Bass 2" }, "Kumi: rename (kr01)", previewed.fence, epoch, layout)
  mock.ops.insert_track(1, { name = "Intro" }) -- Bass is the fourth now
  H.tick(2)
  H.events(peer, "undone_in_daw")
  -- the producer's own (unrecorded) edit made the insert; Kumi's rename is the newest step
  mock.song:undo()
  H.tick(3)
  drain(peer)
  local told = H.events(peer, "undone_in_daw")
  eq(#told, 1)
  eq(told[1].data.descs[1], "Kumi: rename (kr01)")
  eq(mock.data.tracks[4].name, "Bass")
  H.no_dialogs()
end)
