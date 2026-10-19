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
    local path = env.bundle .. "payload/changes_base.lua"
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
    local path = env.bundle .. "payload/changes_base.lua"
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

-- Send tracks and groups (the places Renoise gives a track made again)

local function order()
  local out = {}
  for i, t in ipairs(mock.data.tracks) do out[i] = t.name .. (t.kind == "group" and "[G]" or "") .. (mock.song:track(i).group_parent and "(in)" or "") end
  return table.concat(out, ",")
end

local function delete_and_restore(peer, index)
  local epoch = place(peer)
  local previewed, _, layout = preview(peer, "track-delete", { ref = track_ref(epoch, index) })
  local done = apply(peer, "track-delete", { ref = track_ref(epoch, index) }, "Kumi: d (kg01)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  local undone = restore(peer, "track-delete", { target = previewed.target, prior = previewed.prior, applied = done.result.applied, material = done.result.material },
    "Kumi: undo d (kg02)", epoch, done.result.layout)
  ok(undone.result, json.encode(undone.error or {}))
  return done.result.material, undone.result
end

test("changes: a send track goes back after the master, where it was", function()
  local env, peer = setup(nil, { send_tracks = 2 })
  local before = order()
  eq(before, "Kick,Hat,Bass,Pad,Master,Send 1,Send 2")
  local material, restored = delete_and_restore(peer, 6)
  eq(material.kind, "send")
  eq(order(), before)
  eq(mock.data.tracks[6].kind, "send")
  H.no_dialogs()
end)

test("changes: a group's first member goes back first, its last member back last, and both are members again", function()
  local env, peer = setup()
  local song = mock.song
  song:insert_group_at(5)
  song:add_track_to_group(3, 5) -- Bass joins
  song:add_track_to_group(3, 5) -- Pad joins
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local before = order()
  eq(before, "Kick,Hat,Bass(in),Pad(in),Group[G],Master,Send 1")
  local first = delete_and_restore(peer, 3)
  eq(first.group.name, "Group")
  eq(order(), before, "the first member is the first member again")
  local last = delete_and_restore(peer, 4)
  eq(last.group.index, 5)
  eq(order(), before, "and the last is the last")
  eq(mock.data.tracks[5].members, 2)
  -- a track that is not in a group is not put in one
  local plain = delete_and_restore(peer, 2)
  eq(plain.group, nil)
  eq(order(), before)
  H.no_dialogs()
end)

test("changes: a restore Renoise can't finish takes back the track it began, and says so", function()
  local env, peer = setup()
  local song = mock.song
  local bass = song:track(3)
  bass:insert_device_at("Audio/Effects/Native/EQ 5", 2)
  bass:insert_device_at("Audio/Effects/Native/Gainer", 3)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local epoch = place(peer)
  local previewed, _, layout = preview(peer, "track-delete", { ref = track_ref(epoch, 3) })
  local done = apply(peer, "track-delete", { ref = track_ref(epoch, 3) }, "Kumi: d (kf01)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  done.result.material.devices[2].path = "Audio/Effects/Native/Missing Device" -- a plug-in this Renoise doesn't have
  local before = #mock.data.tracks
  local undone = restore(peer, "track-delete", { target = previewed.target, prior = previewed.prior, applied = done.result.applied, material = done.result.material },
    "Kumi: undo d (kf02)", epoch, done.result.layout)
  failure(undone, "failed")
  ok(undone.error.message:find("wouldn't put the track back", 1, true), undone.error.message)
  eq(#mock.data.tracks, before, "no track was left behind")
  H.no_dialogs()
end)

-- Song settings and devices

local function simple_change(peer, kind, input, desc)
  local previewed, epoch, layout = preview(peer, kind, input)
  local done = apply(peer, kind, input, desc, previewed.fence, epoch, layout)
  ok(done.result, kind .. ": " .. json.encode(done.error or {}))
  return previewed, done.result, epoch, layout
end

test("changes: tempo, lines per beat and ticks per line are undoable steps that put back", function()
  local env, peer = setup()
  for _, case in ipairs({
    { "song-bpm", { bpm = 140 }, "Tempo 140 BPM", function() return mock.data.transport_fields.bpm end, 140, 120 },
    { "song-lpb", { lpb = 8 }, "Lines per beat 8", function() return mock.data.transport_fields.lpb end, 8, 4 },
    { "song-tpl", { tpl = 3 }, "Ticks per line 3", function() return mock.data.transport_fields.tpl end, 3, 6 },
  }) do
    local kind, input, title, read, written, original = case[1], case[2], case[3], case[4], case[5], case[6]
    local before = steps()
    local previewed, done, epoch, layout = simple_change(peer, kind, input, "Kumi: " .. kind .. " (ka01)")
    eq(previewed.display.title, title)
    eq(previewed.target.kind, "song")
    eq(done.undo.entry, true)
    eq(read(), written, kind .. " written")
    eq(steps(), before + 1)
    local undone = restore(peer, kind, { target = previewed.target, prior = previewed.prior, applied = done.applied }, "Kumi: undo " .. kind .. " (ka02)", epoch, layout)
    ok(undone.result, json.encode(undone.error or {}))
    eq(read(), original, kind .. " put back")
  end
  local response = ask(peer, "change_preview", "song-bpm", { bpm = 5 })
  failure(response, "bad-params")
  failure(ask(peer, "change_preview", "song-lpb", { lpb = 1.5 }), "bad-params")
  failure(ask(peer, "change_preview", "song-tpl", { tpl = 17 }), "bad-params")
  H.no_dialogs()
end)

test("changes: the producer's undo of a tempo change is told", function()
  local env, peer = setup()
  local _, done = simple_change(peer, "song-bpm", { bpm = 150 }, "Kumi: tempo (ka03)")
  H.tick(2)
  H.events(peer, "undone_in_daw")
  mock.song:undo()
  H.tick(3)
  drain(peer)
  local told = H.events(peer, "undone_in_daw")
  eq(#told, 1)
  eq(told[1].data.how, "undo")
  eq(told[1].data.descs[1], "Kumi: tempo (ka03)")
  H.no_dialogs()
end)

local function device_song()
  local env, peer = setup(nil, { send_tracks = 2 })
  mock.device_catalog = {
    ["Audio/Effects/Native/EQ 5"] = { { name = "Gain", value = 0.5, min = 0, max = 1 } },
    ["Audio/Effects/Native/#Send"] = { { name = "Receiver", value = 0, min = 0, max = 255 }, { name = "Amount", value = 1, min = 0, max = 1 } },
    ["Audio/Effects/Native/Gainer"] = { { name = "Gain", value = 1, min = 0, max = 4 } },
  }
  local bass = mock.song:track(3)
  bass:insert_device_at("Audio/Effects/Native/EQ 5", 2)
  bass:insert_device_at("Audio/Effects/Native/#Send", 3)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  return env, peer
end

test("changes: a device is bypassed and put back, and the track's mixer device is not touched", function()
  local env, peer = device_song()
  local epoch = place(peer)
  local ref = epoch .. ":device:3.2"
  local previewed, done, e, layout = simple_change(peer, "device-bypass", { ref = ref, bypass = true }, "Kumi: bypass (kb01)")
  eq(previewed.display.title, "EQ 5 on Bass bypassed")
  eq(previewed.target.ref, ref)
  eq(mock.data.tracks[3].devices[2].active, false)
  eq(steps(), 1)
  local undone = restore(peer, "device-bypass", { target = previewed.target, prior = previewed.prior, applied = done.applied }, "Kumi: undo (kb02)", e, layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(mock.data.tracks[3].devices[2].active, true)
  failure(ask(peer, "change_preview", "device-bypass", { ref = epoch .. ":device:3.1", bypass = true }), "bad-params")
  failure(ask(peer, "change_preview", "device-bypass", { ref = epoch .. ":device:3.9", bypass = true }), "gone")
  failure(ask(peer, "change_preview", "device-bypass", { ref = ref, bypass = "yes" }), "bad-params")
  H.no_dialogs()
end)

test("changes: a parameter is set by value or by text, kept in its range, and a Receiver must be a send track", function()
  local env, peer = device_song()
  local epoch = place(peer)
  local ref = epoch .. ":param:3.2.1"
  local previewed, done, e, layout = simple_change(peer, "device-param", { ref = ref, value = 0.25 }, "Kumi: gain (kc01)")
  eq(previewed.display.title, "EQ 5 on Bass Gain 0.25")
  eq(mock.data.tracks[3].devices[2].params[1].value, 0.25)
  local undone = restore(peer, "device-param", { target = previewed.target, prior = previewed.prior, applied = done.applied }, "Kumi: undo (kc02)", e, layout)
  ok(undone.result)
  eq(mock.data.tracks[3].devices[2].params[1].value, 0.5)
  -- by text
  local text, textdone = simple_change(peer, "device-param", { ref = ref, valueString = "0.75" }, "Kumi: gain text (kc03)")
  eq(mock.data.tracks[3].devices[2].params[1].value, 0.75)
  eq(textdone.applied.value, 0.75)
  -- out of range, and neither
  failure(ask(peer, "change_preview", "device-param", { ref = ref, value = 2 }), "bad-params")
  failure(ask(peer, "change_preview", "device-param", { ref = ref }), "bad-params")
  -- a #Send's Receiver is a send track's number (there are two)
  local receiver = epoch .. ":param:3.3.1"
  simple_change(peer, "device-param", { ref = receiver, value = 1 }, "Kumi: send (kc04)")
  eq(mock.data.tracks[3].devices[3].params[1].value, 1)
  local past = ask(peer, "change_preview", "device-param", { ref = receiver, value = 2 })
  failure(past, "bad-params")
  ok(past.error.message:find("send track", 1, true), past.error.message)
  failure(ask(peer, "change_preview", "device-param", { ref = receiver, value = 0.5 }), "bad-params")
  H.no_dialogs()
end)

test("changes: the producer's undo of a parameter change is told, and the device's place follows the devices made before it", function()
  local env, peer = device_song()
  local epoch = place(peer)
  simple_change(peer, "device-param", { ref = epoch .. ":param:3.3.1", value = 1 }, "Kumi: send (kd11)")
  H.tick(2)
  H.events(peer, "undone_in_daw")
  -- the producer puts a device in front of it: the #Send is the fourth device now
  mock.ops.insert_device(3, { name = "Filter", params = {} }, 2)
  H.tick(2)
  mock.data.tracks[3].devices[4].params[1].value = 1
  mock.song:undo() -- the parameter change is the newest step (the device was made by a producer's op, no step)
  H.tick(3)
  drain(peer)
  local told = H.events(peer, "undone_in_daw")
  eq(#told, 1, "told")
  eq(told[1].data.descs[1], "Kumi: send (kd11)")
  H.no_dialogs()
end)

-- Putting a device in, taking it out, moving it

local function devices_of(i) return mock.data.tracks[i].devices end
local function paths_of(i)
  local out = {}
  for k, device in ipairs(devices_of(i)) do out[k] = k == 1 and "Mixer" or device.path:match("([^/]+)$") end
  return table.concat(out, ",")
end

test("changes: a device is put in by its path or its name as one undo step, and taken out again", function()
  local env, peer = device_song()
  local epoch, layout = place(peer)
  local ref = track_ref(epoch, 3)
  eq(paths_of(3), "Mixer,EQ 5,#Send")
  local input = { ref = ref, path = "Audio/Effects/Native/Gainer", index = 3 }
  local previewed = preview(peer, "device-add", input)
  eq(previewed.display.title, "Added Gainer to Bass")
  eq(previewed.display.after.index, 3)
  eq(previewed.target.ref, ref)
  eq(paths_of(3), "Mixer,EQ 5,#Send", "a preview writes nothing")
  eq(steps(), 0)
  -- by name, in any case
  eq(preview(peer, "device-add", { ref = ref, path = "gainer" }).display.after.index, 4)
  local done = apply(peer, "device-add", input, "Kumi: add (ke01)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(paths_of(3), "Mixer,EQ 5,Gainer,#Send")
  eq(steps(), 1)
  eq(newest(), "Kumi: add (ke01)")
  ok(done.result.epoch ~= epoch, "the devices moved the epoch")
  eq(done.result.created.ref, done.result.epoch .. ":device:3.3")
  eq(done.result.created.kind, "device")
  eq(done.result.applied.index, 3)
  eq(done.result.undo.entry, true)
  eq(done.result.material.track, 3)
  -- put back from where the apply left the song
  local undone = restore(peer, "device-add", { target = previewed.target, prior = previewed.prior, applied = done.result.applied,
    created = done.result.created, material = done.result.material }, "Kumi: undo add (ke02)", done.result.epoch, done.result.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(paths_of(3), "Mixer,EQ 5,#Send")
  eq(steps(), 2)
  eq(newest(), "Kumi: undo add (ke02)")
  H.no_dialogs()
end)

test("changes: a device is refused when the song has none like it, the place is out of range or a send's settings are not for a send", function()
  local env, peer = device_song()
  local epoch = place(peer)
  local ref = track_ref(epoch, 3)
  local unknown = ask(peer, "change_preview", "device-add", { ref = ref, path = "Audio/Effects/Native/Gain" })
  failure(unknown, "bad-params")
  ok(unknown.error.message:find("Gainer", 1, true), unknown.error.message)
  failure(ask(peer, "change_preview", "device-add", { ref = ref, path = "Nothing at all" }), "bad-params")
  for _, index in ipairs({ 1, 5, 2.5 }) do
    failure(ask(peer, "change_preview", "device-add", { ref = ref, path = "Gainer", index = index }), "bad-params")
  end
  failure(ask(peer, "change_preview", "device-add", { ref = ref, path = "Gainer", receiver = 0 }), "bad-params")
  failure(ask(peer, "change_preview", "device-add", { ref = ref, path = "#Send", receiver = 2 }), "bad-params")
  failure(ask(peer, "change_preview", "device-add", { ref = ref, path = "#Send", receiver = 0.5 }), "bad-params")
  failure(ask(peer, "change_preview", "device-add", { ref = epoch .. ":track:9", path = "Gainer" }), "gone")
  failure(ask(peer, "change_preview", "device-add", { path = "Gainer" }), "bad-params")
  eq(steps(), 0)
  H.no_dialogs()
end)

test("changes: a #Send is put in with its receiver and amount", function()
  local env, peer = device_song()
  local epoch, layout = place(peer)
  local input = { ref = track_ref(epoch, 1), path = "#Send", receiver = 1, amount = 0.5 }
  local previewed = preview(peer, "device-add", input)
  local done = apply(peer, "device-add", input, "Kumi: send (ke11)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(paths_of(1), "Mixer,#Send")
  eq(devices_of(1)[2].params[1].value, 1, "the receiver")
  eq(devices_of(1)[2].params[2].value, 0.5, "the amount")
  eq(done.result.created.ref, done.result.epoch .. ":device:1.2")
  local out_of_range = { ref = track_ref(done.result.epoch, 1), path = "#Send", amount = 3 }
  local again = call(peer, "change_preview", { kind = "device-add", input = out_of_range, epoch = done.result.epoch, layout = done.result.layout })
  ok(again.result, json.encode(again.error or {}))
  local refused = apply(peer, "device-add", out_of_range, "Kumi: send (ke12)", again.result.fence, done.result.epoch, done.result.layout)
  failure(refused, "bad-params")
  H.no_dialogs()
end)

test("changes: a device the producer changed since is not taken out unless Kumi is told to", function()
  local env, peer = device_song()
  local epoch, layout = place(peer)
  local input = { ref = track_ref(epoch, 3), path = "Gainer", index = 2 }
  local previewed = preview(peer, "device-add", input)
  local done = apply(peer, "device-add", input, "Kumi: add (ke21)", previewed.fence, epoch, layout).result
  local record = { target = previewed.target, prior = previewed.prior, applied = done.applied, created = done.created, material = done.material }
  devices_of(3)[2].params[1].value = 3
  failure(restore(peer, "device-add", record, "Kumi: undo add (ke22)", done.epoch, done.layout), "changed")
  eq(paths_of(3), "Mixer,Gainer,EQ 5,#Send")
  eq(steps(), 1, "nothing was written")
  local forced = restore(peer, "device-add", record, "Kumi: undo add (ke22)", done.epoch, done.layout, { discard = true })
  ok(forced.result, json.encode(forced.error or {}))
  eq(paths_of(3), "Mixer,EQ 5,#Send")
  -- another epoch is stale
  local made = apply(peer, "device-add", input, "Kumi: add (ke23)", previewed.fence, place(peer), 0)
  failure(made, "stale")
  H.no_dialogs()
end)

local function bass_with_automation()
  local env, peer = device_song()
  local d = devices_of(3)
  d[2].preset_data, d[2].display_name, d[2].active = "<preset eq5/>", "Warm", false
  mock.ops.automate(1, 3, 2, 1, { { time = 1, value = 0.2, scaling = 0 }, { time = 9, value = 0.9, scaling = 0 } })
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  return env, peer
end

test("changes: deleting a device keeps its settings and automation, and the restore makes it again where it was", function()
  local env, peer = bass_with_automation()
  local epoch, layout = place(peer)
  local ref = epoch .. ":device:3.2"
  local previewed = preview(peer, "device-delete", { ref = ref })
  eq(previewed.display.title, "Deleted Warm on Bass")
  eq(previewed.prior.deleted, false)
  eq(steps(), 0)
  local done = apply(peer, "device-delete", { ref = ref }, "Kumi: delete device (kf01)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(paths_of(3), "Mixer,#Send")
  eq(steps(), 1)
  eq(done.result.applied.deleted, true)
  eq(done.result.material.track, 3)
  eq(done.result.material.index, 2)
  eq(done.result.material.device.presetData, "<preset eq5/>")
  eq(#done.result.material.automation, 1)
  eq(done.result.material.automation[1].points[2].value, 0.9)
  ok(done.result.epoch ~= epoch)
  local record = { target = previewed.target, prior = previewed.prior, applied = done.result.applied, material = done.result.material }
  local undone = restore(peer, "device-delete", record, "Kumi: undo delete device (kf02)", done.result.epoch, done.result.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(paths_of(3), "Mixer,EQ 5,#Send")
  local back = devices_of(3)[2]
  eq(back.preset_data, "<preset eq5/>")
  eq(back.display_name, "Warm")
  eq(back.active, false)
  eq(steps(), 2)
  eq(newest(), "Kumi: undo delete device (kf02)")
  eq(undone.result.restored.ref, undone.result.epoch .. ":device:3.2")
  local automation = mock.data.patterns[1].tracks[3].automation
  eq(#automation, 1)
  eq(automation[1].points[2].value, 0.9)
  -- asked again: it is back, nothing is written
  local again = restore(peer, "device-delete", record, "Kumi: undo delete device (kf03)", undone.result.epoch, undone.result.layout)
  ok(again.result, json.encode(again.error or {}))
  eq(again.result.undo.entry, false)
  eq(steps(), 2)
  H.no_dialogs()
end)

test("changes: a deleted device goes back to its track after tracks were made before it, and the mixer device is never touched", function()
  local env, peer = bass_with_automation()
  local epoch, layout = place(peer)
  local ref = epoch .. ":device:3.3"
  local previewed = preview(peer, "device-delete", { ref = ref })
  local done = apply(peer, "device-delete", { ref = ref }, "Kumi: delete device (kf11)", previewed.fence, epoch, layout).result
  eq(paths_of(3), "Mixer,EQ 5")
  mock.api.insert_track_at(1)
  H.tick(3)
  local now = place(peer)
  eq(now, done.epoch, "a made track moves the layout, not the epoch")
  local undone = restore(peer, "device-delete", { target = previewed.target, prior = previewed.prior, applied = done.applied, material = done.material },
    "Kumi: undo delete device (kf12)", done.epoch, done.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(paths_of(4), "Mixer,EQ 5,#Send", "Bass is track 4 now")
  eq(undone.result.restored.ref, undone.result.epoch .. ":device:4.3")
  failure(ask(peer, "change_preview", "device-delete", { ref = undone.result.epoch .. ":device:4.1" }), "bad-params")
  failure(ask(peer, "change_preview", "device-move", { ref = undone.result.epoch .. ":device:4.1", index = 2 }), "bad-params")
  failure(ask(peer, "change_preview", "device-delete", { ref = undone.result.epoch .. ":device:4.9" }), "gone")
  H.no_dialogs()
end)

test("changes: a device changed since the preview is not deleted", function()
  local env, peer = bass_with_automation()
  local epoch, layout = place(peer)
  local ref = epoch .. ":device:3.2"
  local previewed = preview(peer, "device-delete", { ref = ref })
  devices_of(3)[2].params[1].value = 0.1
  failure(apply(peer, "device-delete", { ref = ref }, "Kumi: delete device (kf21)", previewed.fence, epoch, layout), "changed")
  eq(paths_of(3), "Mixer,EQ 5,#Send")
  eq(steps(), 0)
  H.no_dialogs()
end)

local function four_devices()
  local env, peer = device_song()
  devices_of(3)[4] = nil
  mock.song:track(3):insert_device_at("Audio/Effects/Native/Gainer", 4)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  return env, peer
end

test("changes: a device is moved by swaps as one undo step, and moved back", function()
  local env, peer = four_devices()
  eq(paths_of(3), "Mixer,EQ 5,#Send,Gainer")
  local epoch, layout = place(peer)
  local ref = epoch .. ":device:3.2"
  local previewed = preview(peer, "device-move", { ref = ref, index = 4 })
  eq(previewed.display.title, "Moved EQ 5 on Bass to place 4")
  eq(previewed.prior.index, 2)
  eq(steps(), 0)
  local done = apply(peer, "device-move", { ref = ref, index = 4 }, "Kumi: move (kg01)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(paths_of(3), "Mixer,#Send,Gainer,EQ 5")
  eq(steps(), 1)
  eq(done.result.applied.index, 4)
  eq(done.result.material.track, 3)
  local record = { target = previewed.target, prior = previewed.prior, applied = done.result.applied, material = done.result.material }
  local undone = restore(peer, "device-move", record, "Kumi: undo move (kg02)", done.result.epoch, done.result.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(paths_of(3), "Mixer,EQ 5,#Send,Gainer")
  eq(steps(), 2)
  -- already back: nothing written
  local again = restore(peer, "device-move", record, "Kumi: undo move (kg03)", undone.result.epoch, undone.result.layout)
  ok(again.result, json.encode(again.error or {}))
  eq(again.result.undo.entry, false)
  eq(steps(), 2)
  -- to where it is: nothing to do
  local same = preview(peer, "device-move", { ref = place(peer) .. ":device:3.2", index = 2 })
  eq(same.display.unchanged, true)
  H.no_dialogs()
end)

test("changes: a device the producer moved since is not moved back", function()
  local env, peer = four_devices()
  local epoch, layout = place(peer)
  local ref = epoch .. ":device:3.2"
  local previewed = preview(peer, "device-move", { ref = ref, index = 3 })
  local done = apply(peer, "device-move", { ref = ref, index = 3 }, "Kumi: move (kg11)", previewed.fence, epoch, layout).result
  eq(paths_of(3), "Mixer,#Send,EQ 5,Gainer")
  mock.song:track(3):swap_devices_at(3, 4)
  H.tick(2)
  local now, layout_now = place(peer)
  local record = { target = previewed.target, prior = previewed.prior, applied = done.applied, material = done.material }
  failure(restore(peer, "device-move", record, "Kumi: undo move (kg12)", now, layout_now), "changed")
  eq(paths_of(3), "Mixer,#Send,Gainer,EQ 5")
  failure(ask(peer, "change_preview", "device-move", { ref = now .. ":device:3.2", index = 9 }), "bad-params")
  H.no_dialogs()
end)

test("changes: the producer's undo and redo of a device made, deleted or moved is told", function()
  local env, peer = bass_with_automation()
  local epoch, layout = place(peer)
  local function told_after(action)
    H.tick(2)
    H.events(peer, "undone_in_daw")
    action()
    H.tick(3)
    drain(peer)
    return H.events(peer, "undone_in_daw")
  end
  -- made
  local added = apply(peer, "device-add", { ref = track_ref(epoch, 3), path = "Gainer", index = 2 }, "Kumi: add (kh01)",
    preview(peer, "device-add", { ref = track_ref(epoch, 3), path = "Gainer", index = 2 }).fence, epoch, layout).result
  local told = told_after(function() mock.song:undo() end)
  eq(#told, 1, "made, undone")
  eq(told[1].data.how, "undo")
  eq(told[1].data.descs[1], "Kumi: add (kh01)")
  eq(paths_of(3), "Mixer,EQ 5,#Send")
  told = told_after(function() mock.song:redo() end)
  eq(#told, 1, "made, redone")
  eq(told[1].data.how, "redo")
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  -- deleted
  local epoch2, layout2 = place(peer)
  local ref = epoch2 .. ":device:3.2"
  local fence = preview(peer, "device-delete", { ref = ref }).fence
  apply(peer, "device-delete", { ref = ref }, "Kumi: delete (kh02)", fence, epoch2, layout2)
  told = told_after(function() mock.song:undo() end)
  eq(#told, 1, "deleted, undone")
  eq(told[1].data.how, "undo")
  eq(told[1].data.descs[1], "Kumi: delete (kh02)")
  told = told_after(function() mock.song:redo() end)
  eq(#told, 1, "deleted, redone")
  eq(told[1].data.how, "redo")
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  H.no_dialogs()
end)

test("changes: the producer's undo of a device move is told", function()
  local env, peer = four_devices()
  local epoch, layout = place(peer)
  local ref = epoch .. ":device:3.2"
  local fence = preview(peer, "device-move", { ref = ref, index = 4 }).fence
  apply(peer, "device-move", { ref = ref, index = 4 }, "Kumi: move (ki01)", fence, epoch, layout)
  H.tick(2)
  H.events(peer, "undone_in_daw")
  mock.song:undo()
  H.tick(3)
  drain(peer)
  local told = H.events(peer, "undone_in_daw")
  eq(#told, 1)
  eq(told[1].data.how, "undo")
  eq(told[1].data.descs[1], "Kumi: move (ki01)")
  eq(paths_of(3), "Mixer,EQ 5,#Send,Gainer")
  mock.song:redo()
  H.tick(3)
  drain(peer)
  told = H.events(peer, "undone_in_daw")
  eq(told[#told].data.how, "redo")
  H.no_dialogs()
end)

-- Notes and effects on lines

local function pattern_ref(epoch, p) return epoch .. ":pattern:" .. p end

local function cell(p, t, l, c)
  local column = mock.song:pattern(p):track(t):line(l):note_column(c)
  return column.note_string .. " " .. column.instrument_string .. " " .. column.volume_string .. " " .. column.panning_string
    .. " " .. column.delay_string .. " " .. column.effect_number_string .. column.effect_amount_string
end

local function effect_cell(p, t, l, c)
  local column = mock.song:pattern(p):track(t):line(l):effect_column(c)
  return column.number_string .. column.amount_string
end

local function write_input(epoch)
  return {
    pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 3),
    lines = {
      { line = 1, notes = { { col = 1, note = "c-2", instrument = 0, volume = "40" }, { col = 3, note = "E-2", instrument = "01", delay = "10", fx = "0a20" } },
        effects = { { col = 2, number = "0A", amount = "20" } } },
      { line = 5, notes = { { col = 1, note = "OFF" } } },
    },
  }
end

test("changes: lines are written as the tracker's own text, the columns they need are shown, and it is one undo step", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local input = write_input(epoch)
  local previewed = preview(peer, "notes-write", input)
  eq(previewed.display.title, "Wrote 2 lines in Pattern 00 \194\183 Bass")
  eq(previewed.target.ref, epoch .. ":ptrack:1.3")
  eq(previewed.target.kind, "pattern-track")
  eq(#previewed.prior.cells, 4)
  eq(previewed.prior.columns.note, 2)
  eq(steps(), 0)
  eq(cell(1, 3, 1, 1), "--- .. .. .. .. 0000", "a preview writes nothing")
  local done = apply(peer, "notes-write", input, "Kumi: notes (kn01)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(steps(), 1)
  eq(newest(), "Kumi: notes (kn01)")
  eq(done.result.undo.entry, true)
  eq(cell(1, 3, 1, 1), "C-2 00 40 .. .. 0000")
  eq(cell(1, 3, 1, 3), "E-2 01 .. .. 10 0A20")
  eq(effect_cell(1, 3, 1, 2), "0A20")
  eq(cell(1, 3, 5, 1), "OFF .. .. .. .. 0000")
  eq(#done.result.applied.cells, 4)
  eq(done.result.applied.cells[1].note, "C-2")
  eq(done.result.applied.columns.note, 3)
  eq(mock.data.tracks[3].note_columns, 3, "the third note column is shown")
  eq(mock.data.tracks[3].effect_columns, 2)
  eq(mock.data.tracks[3].volume_column, true)
  eq(mock.data.tracks[3].delay_column, true)
  eq(mock.data.tracks[3].sample_effects_column, true)
  eq(done.result.epoch, epoch, "lines don't move the epoch")
  -- put back
  local undone = restore(peer, "notes-write", { target = previewed.target, prior = previewed.prior, applied = done.result.applied },
    "Kumi: undo notes (kn02)", epoch, layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(cell(1, 3, 1, 1), "--- .. .. .. .. 0000")
  eq(cell(1, 3, 1, 3), "--- .. .. .. .. 0000")
  eq(effect_cell(1, 3, 1, 2), "0000")
  eq(cell(1, 3, 5, 1), "--- .. .. .. .. 0000")
  eq(mock.data.tracks[3].note_columns, 2, "the columns Kumi showed are hidden again")
  eq(mock.data.tracks[3].effect_columns, 1)
  eq(mock.data.tracks[3].volume_column or false, false)
  eq(steps(), 2)
  eq(newest(), "Kumi: undo notes (kn02)")
  H.no_dialogs()
end)

test("changes: the columns Kumi showed stay shown when the producer put something in them", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local input = write_input(epoch)
  local previewed = preview(peer, "notes-write", input)
  local done = apply(peer, "notes-write", input, "Kumi: notes (kn11)", previewed.fence, epoch, layout).result
  local other = mock.song:pattern(2):track(3):line(7):note_column(3)
  other.note_value, other.instrument_value = 40, 2
  local undone = restore(peer, "notes-write", { target = previewed.target, prior = previewed.prior, applied = done.applied }, "Kumi: undo (kn12)", epoch, layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(mock.data.tracks[3].note_columns, 3, "column 3 holds a note in another pattern")
  eq(mock.data.tracks[3].effect_columns, 1, "nothing is in the second effect column")
  H.no_dialogs()
end)

test("changes: a cell the producer changed since is not overwritten by a restore unless Kumi is told to", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local input = write_input(epoch)
  local previewed = preview(peer, "notes-write", input)
  local done = apply(peer, "notes-write", input, "Kumi: notes (kn21)", previewed.fence, epoch, layout).result
  local mine = mock.song:pattern(1):track(3):line(1):note_column(1)
  mine.note_string = "G-3"
  local before = steps()
  local record = { target = previewed.target, prior = previewed.prior, applied = done.applied }
  failure(restore(peer, "notes-write", record, "Kumi: undo (kn22)", epoch, layout), "changed")
  eq(cell(1, 3, 1, 1), "G-3 00 40 .. .. 0000")
  eq(steps(), before)
  local forced = restore(peer, "notes-write", record, "Kumi: undo (kn22)", epoch, layout, { discard = true })
  ok(forced.result, json.encode(forced.error or {}))
  eq(cell(1, 3, 1, 1), "--- .. .. .. .. 0000")
  -- asked again: it is back, nothing is written
  local after_forced = steps()
  local again = restore(peer, "notes-write", record, "Kumi: undo (kn23)", epoch, layout)
  ok(again.result, json.encode(again.error or {}))
  eq(again.result.undo.entry, false)
  eq(steps(), after_forced)
  H.no_dialogs()
end)

test("changes: lines the producer changed between the preview and the apply are not overwritten", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local input = write_input(epoch)
  local previewed = preview(peer, "notes-write", input)
  mock.song:pattern(1):track(3):line(5):note_column(1).note_string = "C-5"
  local before = steps()
  failure(apply(peer, "notes-write", input, "Kumi: notes (kn31)", previewed.fence, epoch, layout), "changed")
  eq(cell(1, 3, 1, 1), "--- .. .. .. .. 0000")
  eq(steps(), before)
  failure(apply(peer, "notes-write", input, "Kumi: notes (kn32)", previewed.fence, "ffff", layout), "stale")
  H.no_dialogs()
end)

test("changes: merge keeps the fields a cell isn't given, and without it a cell is written whole", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local first = { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 3), lines = { { line = 2, notes = { { col = 1, note = "A-3", instrument = "02", volume = "30" } } } } }
  local p1 = preview(peer, "notes-write", first)
  ok(apply(peer, "notes-write", first, "Kumi: a (kn41)", p1.fence, epoch, layout).result)
  local merged = { pattern = first.pattern, track = first.track, merge = true, lines = { { line = 2, notes = { { col = 1, volume = "50" } } } } }
  local p2 = preview(peer, "notes-write", merged)
  ok(apply(peer, "notes-write", merged, "Kumi: b (kn42)", p2.fence, epoch, layout).result)
  eq(cell(1, 3, 2, 1), "A-3 02 50 .. .. 0000")
  local whole = { pattern = first.pattern, track = first.track, lines = { { line = 2, notes = { { col = 1, volume = "60" } } } } }
  local p3 = preview(peer, "notes-write", whole)
  ok(apply(peer, "notes-write", whole, "Kumi: c (kn43)", p3.fence, epoch, layout).result)
  eq(cell(1, 3, 2, 1), "--- .. 60 .. .. 0000")
  -- a write of what is there is no change
  local same = preview(peer, "notes-write", whole)
  eq(same.display.unchanged, true)
  local done = apply(peer, "notes-write", whole, "Kumi: d (kn44)", same.fence, epoch, layout)
  eq(done.result.undo.entry, false)
  eq(steps(), 3)
  H.no_dialogs()
end)

test("changes: lines are cleared with every cell on them, hidden columns too, and notes-clear clears a range", function()
  local env, peer = setup()
  local song = mock.song
  for _, spec in ipairs({ { 1, 1, 1, 48 }, { 1, 4, 5, 50 }, { 1, 10, 10, 52 } }) do
    local column = song:pattern(1):track(3):line(spec[2]):note_column(spec[3])
    column.note_value, column.instrument_value = spec[4], 1
  end
  local effect = song:pattern(1):track(3):line(4):effect_column(2)
  effect.number_value, effect.amount_value = 10, 32
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local epoch, layout = place(peer)
  local input = { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 3), lines = { { line = 4, clear = true } } }
  local previewed = preview(peer, "notes-write", input)
  eq(previewed.display.title, "Wrote 1 line in Pattern 00 \194\183 Bass")
  eq(#previewed.prior.cells, 2, "the note in column 5 and the effect in column 2")
  local done = apply(peer, "notes-write", input, "Kumi: clear (kn51)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  ok(song:pattern(1):track(3):line(4).is_empty, "the line is empty")
  local undone = restore(peer, "notes-write", { target = previewed.target, prior = previewed.prior, applied = done.result.applied }, "Kumi: undo (kn52)", epoch, layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(cell(1, 3, 4, 5), "D-4 01 .. .. .. 0000")
  eq(effect_cell(1, 3, 4, 2), "0A20")
  -- a range
  local range = { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 3), first = 1, last = 5 }
  local p2 = preview(peer, "notes-clear", range)
  eq(p2.display.title, "Cleared 2 lines in Pattern 00 \194\183 Bass")
  eq(p2.display.before.notes, 2)
  eq(p2.display.before.effects, 1)
  local d2 = apply(peer, "notes-clear", range, "Kumi: clear (kn53)", p2.fence, epoch, layout)
  ok(d2.result, json.encode(d2.error or {}))
  ok(not song:pattern(1):track(3):line(10).is_empty, "line 10 is outside the range")
  ok(song:pattern(1):track(3):line(1).is_empty and song:pattern(1):track(3):line(4).is_empty)
  local u2 = restore(peer, "notes-clear", { target = p2.target, prior = p2.prior, applied = d2.result.applied }, "Kumi: undo (kn54)", epoch, layout)
  ok(u2.result, json.encode(u2.error or {}))
  eq(cell(1, 3, 1, 1), "C-4 01 .. .. .. 0000")
  eq(effect_cell(1, 3, 4, 2), "0A20")
  -- nothing to clear
  local empty = preview(peer, "notes-clear", { pattern = pattern_ref(epoch, 3), track = track_ref(epoch, 3) })
  eq(empty.display.unchanged, true)
  H.no_dialogs()
end)

test("changes: lines are refused when what they name isn't there or isn't text Renoise reads", function()
  local env, peer = setup()
  local epoch = place(peer)
  local function with(lines, more)
    local input = { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 3), lines = lines }
    for k, v in pairs(more or {}) do input[k] = v end
    return ask(peer, "change_preview", "notes-write", input)
  end
  failure(with({ { line = 0, notes = { { col = 1, note = "C-4" } } } }), "bad-params")
  failure(with({ { line = 33, notes = { { col = 1, note = "C-4" } } } }), "bad-params")
  failure(with({ { line = 1, notes = { { col = 13, note = "C-4" } } } }), "bad-params")
  failure(with({ { line = 1, effects = { { col = 9, number = "0A" } } } }), "bad-params")
  failure(with({ { line = 1, notes = { { col = 1, note = "H-4" } } } }), "bad-params")
  failure(with({ { line = 1, notes = { { col = 1, note = "E#4" } } } }), "bad-params")
  failure(with({ { line = 1, notes = { { col = 1, instrument = "ZZ" } } } }), "bad-params")
  failure(with({ { line = 1, notes = { { col = 1, volume = "400" } } } }), "bad-params")
  failure(with({ { line = 1, notes = { { col = 1, fx = "0A" } } } }), "bad-params")
  failure(with({ { line = 1, notes = { { col = 1 } } } }), "bad-params")
  failure(with({ { line = 1 } }), "bad-params")
  failure(with({ { line = 1, notes = { { col = 1, note = "C-4" }, { col = 1, note = "D-4" } } } }), "bad-params")
  failure(with({ { line = 1, notes = { { col = 1, note = "C-4" } } }, { line = 1, notes = { { col = 2, note = "C-4" } } } }), "bad-params")
  failure(with({}), "bad-params")
  failure(with(nil), "bad-params")
  failure(ask(peer, "change_preview", "notes-write", { pattern = pattern_ref(epoch, 9), track = track_ref(epoch, 3), lines = { { line = 1, clear = true } } }), "gone")
  failure(ask(peer, "change_preview", "notes-write", { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 9), lines = { { line = 1, clear = true } } }), "gone")
  -- the master has no note columns, but takes effects
  failure(ask(peer, "change_preview", "notes-write", { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 5), lines = { { line = 1, notes = { { col = 1, note = "C-4" } } } } }), "bad-params")
  local master = ask(peer, "change_preview", "notes-write", { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 5), lines = { { line = 1, effects = { { col = 1, number = "0A", amount = "10" } } } } })
  ok(master.result, json.encode(master.error or {}))
  -- an alias writes nowhere
  mock.song:pattern(2):track(3).alias_pattern_index = 1
  local before = steps()
  local alias = ask(peer, "change_preview", "notes-write", { pattern = pattern_ref(epoch, 2), track = track_ref(epoch, 3), lines = { { line = 1, notes = { { col = 1, note = "C-4" } } } } })
  failure(alias, "bad-params")
  ok(alias.error.message:find("alias of pattern 1", 1, true), alias.error.message)
  eq(steps(), before)
  H.no_dialogs()
end)

test("changes: a sequence slot names its pattern, and Renoise refusing the text takes back what was written", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  -- slot 4 is pattern 3
  local input = { pattern = epoch .. ":seq:4", track = track_ref(epoch, 1), lines = { { line = 2, notes = { { col = 1, note = "C-5" } } } } }
  local previewed = preview(peer, "notes-write", input)
  eq(previewed.target.ref, epoch .. ":ptrack:3.1")
  local done = apply(peer, "notes-write", input, "Kumi: slot (kn61)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(cell(3, 1, 2, 1), "C-5 .. .. .. .. 0000")
  eq(cell(1, 1, 2, 1), "--- .. .. .. .. 0000")
  -- a text that Renoise itself refuses (a volume the tracker has no meaning for) leaves the pattern as it was
  local column = mock.song:pattern(1):track(2):line(3):note_column(1)
  local refused = { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 2), lines = { { line = 3, notes = { { col = 1, note = "C-4", volume = "ZZ" } } } } }
  local p2 = preview(peer, "notes-write", refused)
  local before = steps()
  failure(apply(peer, "notes-write", refused, "Kumi: bad (kn62)", p2.fence, epoch, layout), "failed")
  eq(column.note_string, "---", "the note that was written before the refusal is taken back")
  H.no_dialogs()
end)

test("changes: a big restore is numbered steps, and the producer's undo of lines is told", function()
  local env, peer = setup({ patch = function(env)
    local path = env.bundle .. "payload/changes_base.lua"
    local f = assert(io.open(path, "rb"))
    local text = f:read("*a")
    f:close()
    f = assert(io.open(path, "wb"))
    f:write((text:gsub("M%.SLICE_WRITES = %d+", "M.SLICE_WRITES = 30")))
    f:close()
  end })
  local epoch, layout = place(peer)
  local lines = {}
  for l = 1, 12 do lines[l] = { line = l, notes = { { col = 1, note = "C-3", instrument = "01" } } } end
  local input = { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 2), lines = lines }
  local previewed = preview(peer, "notes-write", input)
  local done = apply(peer, "notes-write", input, "Kumi: many (kn71)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(steps(), 1)
  -- the producer's undo of it is told
  H.tick(2)
  H.events(peer, "undone_in_daw")
  mock.song:undo()
  H.tick(3)
  drain(peer)
  local told = H.events(peer, "undone_in_daw")
  eq(#told, 1)
  eq(told[1].data.how, "undo")
  eq(told[1].data.descs[1], "Kumi: many (kn71)")
  mock.song:redo()
  H.tick(3)
  drain(peer)
  told = H.events(peer, "undone_in_daw")
  eq(told[#told].data.how, "redo")
  -- restored in numbered steps (84 writes in slices of 30)
  local undone = restore(peer, "notes-write", { target = previewed.target, prior = previewed.prior, applied = done.result.applied }, "Kumi: undo many (kn72)", epoch, layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(undone.result.undo.steps, 3)
  eq(mock.data.undo.stack[#mock.data.undo.stack].desc, "Kumi: undo many (kn72) (3/3)")
  eq(cell(1, 2, 12, 1), "--- .. .. .. .. 0000")
  eq(cell(1, 2, 1, 1), "--- .. .. .. .. 0000")
  H.no_dialogs()
end)

test("changes: a track made before the pattern's track carries the restore to where the lines are now", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local input = { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 2), lines = { { line = 1, notes = { { col = 1, note = "C-3" } } } } }
  local previewed = preview(peer, "notes-write", input)
  local done = apply(peer, "notes-write", input, "Kumi: shift (kn81)", previewed.fence, epoch, layout).result
  mock.api.insert_track_at(1)
  H.tick(3)
  local undone = restore(peer, "notes-write", { target = previewed.target, prior = previewed.prior, applied = done.applied }, "Kumi: undo shift (kn82)", epoch, layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(cell(1, 3, 1, 1), "--- .. .. .. .. 0000", "Hat is track 3 now")
  H.no_dialogs()
end)
