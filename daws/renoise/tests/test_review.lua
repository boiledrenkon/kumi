-- The M3 review's findings on the Renoise tool (M-1 .. M-7, S-2 .. S-7), each as a test that fails on the code it was found in.
local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness

local next_id = 70000

local function drain(peer)
  repeat
    local taken = peer:take()
    if #taken > 0 then H.send(peer, { type = "credit", bytes = #taken }) end
  until #taken == 0
end

-- Looks for the response to `id` in what has come; `step` is how much time passes between looks.
local function wait(peer, id, ticks, step)
  peer.events = peer.events or {}
  for _ = 1, ticks or 400 do
    drain(peer)
    for _, line in ipairs(peer:frames(nil)) do
      local frame = assert(json.decode(line))
      if frame.type == "response" and frame.id == id then return frame end
      if frame.type == "event" then peer.events[#peer.events + 1] = frame end
    end
    mock.advance(step or 10)
  end
end

local function send(peer, method, params)
  next_id = next_id + 1
  H.send(peer, { type = "request", id = next_id, method = method, params = params or {} })
  return next_id
end

local function call(peer, method, params, ticks)
  return wait(peer, send(peer, method, params), ticks)
end

local function song_options(extra)
  local spec = {
    name = "Lab", file_name = "/s/lab.xrns", bpm = 120, lpb = 4, mixer_device = true,
    tracks = { { name = "Kick", color = { 255, 0, 0 } }, { name = "Hat" }, { name = "Bass", note_columns = 2 }, { name = "Pad" } },
    send_tracks = 1, patterns = 3, pattern_lines = 32, sequence_length = 4, sequence = { 1, 2, 1, 3 }, instruments = 1,
  }
  for k, v in pairs(extra or {}) do spec[k] = v end
  return spec
end

-- A patch that sets constants in the temporary copy of changes_base.lua.
local function constants(values)
  return function(env)
    local path = env.bundle .. "payload/changes_base.lua"
    local f = assert(io.open(path, "rb"))
    local text = f:read("*a")
    f:close()
    for name, value in pairs(values) do
      local lowered, n = text:gsub("M%." .. name .. " = [%d%* ]+", "M." .. name .. " = " .. value, 1)
      assert(n == 1, name .. " was not found to set")
      text = lowered
    end
    f = assert(io.open(path, "wb"))
    f:write(text)
    f:close()
  end
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
local function seq_ref(epoch, s) return epoch .. ":seq:" .. s end
local function pattern_ref(epoch, p) return epoch .. ":pattern:" .. p end
local function steps() return #mock.data.undo.stack end

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

local function apply(peer, kind, input, desc, fence, epoch, layout)
  return call(peer, "change_apply", { kind = kind, input = input, fence = fence, desc = desc, epoch = epoch, layout = layout })
end

local function restore_params(kind, record, desc, epoch, layout)
  return {
    kind = kind, target = record.target, prior = record.prior, applied = record.applied, created = record.created,
    material = record.material, desc = desc, epoch = epoch, layout = layout,
  }
end

local function restore(peer, kind, record, desc, epoch, layout)
  return call(peer, "change_restore", restore_params(kind, record, desc, epoch, layout), 800)
end

local function failure(response, code)
  ok(response and response.error, "expected an error, got " .. json.encode(response and response.result or "nothing"))
  eq(response.error.code, code, "error code (" .. tostring(response.error.message) .. ")")
end

local function names()
  local out = {}
  for i, t in ipairs(mock.data.tracks) do out[i] = t.name end
  return table.concat(out, ",")
end

local function order()
  local out = {}
  for i, t in ipairs(mock.data.tracks) do out[i] = t.name .. (t.kind == "group" and "[G]" or "") .. (mock.song:track(i).group_parent and "(in)" or "") end
  return table.concat(out, ",")
end

-- Lines in the first `pats` patterns of a track (the writes of one callback each when SLICE_WRITES is small).
local function fill(t, pats, lines)
  for p = 1, pats or 3 do
    for k = 1, lines or 10 do
      local column = mock.song:pattern(p):track(t):line(k):note_column(1)
      column.note_value, column.instrument_value, column.volume_value = 40 + k, 1, 64
    end
  end
end

-- Deletes track `t` the way Kumi does; returns the preview and the apply's answer.
local function delete_track(peer, t, desc)
  local epoch, layout = place(peer)
  local previewed = preview(peer, "track-delete", { ref = track_ref(epoch, t) })
  local done = apply(peer, "track-delete", { ref = track_ref(epoch, t) }, desc or "Kumi: d (kr01)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  return previewed, done.result
end

local function record_of(previewed, done)
  return { target = previewed.target, prior = previewed.prior, applied = done.applied, material = done.material }
end

-- Starts a sliced restore and runs the clock until its first step is written (the others wait 400 ms each).
local function start_restore(peer, previewed, done, desc)
  local id = send(peer, "change_restore", restore_params("track-delete", record_of(previewed, done), desc, done.epoch, done.layout))
  local before = steps()
  for _ = 1, 50 do
    mock.advance(10)
    if steps() > before then break end
  end
  eq(steps(), before + 1, "step 1 is written")
  return id
end

-- M-1: a stopped sliced restore cleans up only the track it made

test("review M-1: the producer takes step 1 back between the steps: the clean-up deletes nothing, and says uncertain", function()
  local env, peer = setup({ patch = constants({ SLICE_WRITES = 40 }) })
  fill(2)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local previewed, done = delete_track(peer, 2)
  local id = start_restore(peer, previewed, done, "Kumi: undo d (kr02)")
  eq(names(), "Kick,Hat,Bass,Pad,Master,Send 1")
  mock.song:undo() -- the producer's Cmd-Z: the made Hat is gone again
  eq(names(), "Kick,Bass,Pad,Master,Send 1")
  local response = wait(peer, id, 300, 20)
  failure(response, "uncertain")
  eq(names(), "Kick,Bass,Pad,Master,Send 1", "Bass is the producer's, and stays")
  H.no_dialogs()
end)

test("review M-1: the producer makes a track before the half-made one: the clean-up takes the half-made one, not the track now in its place", function()
  local env, peer = setup({ patch = constants({ SLICE_WRITES = 40 }) })
  fill(2)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local previewed, done = delete_track(peer, 2)
  -- (a line that Renoise refuses stops the restore at its second step)
  done.material.patterns[2].lines[1].notes[1].col = 99
  local id = start_restore(peer, previewed, done, "Kumi: undo d (kr03)")
  mock.ops.insert_track(1, { name = "Producer's" })
  eq(names(), "Producer's,Kick,Hat,Bass,Pad,Master,Send 1")
  local response = wait(peer, id, 300, 20)
  ok(response and response.error, "the restore stops")
  eq(names(), "Producer's,Kick,Bass,Pad,Master,Send 1", "the half-made Hat went, Kick stayed")
  H.no_dialogs()
end)

-- M-2: a sliced restore of a track with a device, or a group's member, ends well and answers the epoch it moved to

test("review M-2: a sliced restore of a track with a device finishes and answers the epoch the device moved", function()
  local env, peer = setup({ patch = constants({ SLICE_WRITES = 40 }) })
  mock.song:track(2):insert_device_at("Audio/Effects/Native/EQ 5", 2)
  fill(2)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local previewed, done = delete_track(peer, 2)
  local id = send(peer, "change_restore", restore_params("track-delete", record_of(previewed, done), "Kumi: undo d (kr04)", done.epoch, done.layout))
  local undone = wait(peer, id, 300, 20)
  ok(undone and undone.result, json.encode(undone and undone.error or "no answer"))
  eq(undone.result.undo.steps, 3)
  eq(names(), "Kick,Hat,Bass,Pad,Master,Send 1")
  eq(mock.data.tracks[2].devices[2].path, "Audio/Effects/Native/EQ 5")
  ok(undone.result.epoch ~= done.epoch, "the device moved the epoch")
  ok(undone.result.kept and undone.result.kept.from == done.epoch, "and the answer says what was kept: " .. json.encode(undone.result.kept))
  eq(undone.result.restored.ref, track_ref(undone.result.epoch, 2))
  H.no_dialogs()
end)

test("review M-2: a sliced restore of a group's first member finishes, and it is a member again", function()
  local env, peer = setup({ patch = constants({ SLICE_WRITES = 40 }) })
  mock.song:insert_group_at(5)
  mock.song:add_track_to_group(3, 5)
  mock.song:add_track_to_group(3, 5)
  fill(3)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local before = order()
  eq(before, "Kick,Hat,Bass(in),Pad(in),Group[G],Master,Send 1")
  local previewed, done = delete_track(peer, 3)
  local id = send(peer, "change_restore", restore_params("track-delete", record_of(previewed, done), "Kumi: undo d (kr05)", done.epoch, done.layout))
  local undone = wait(peer, id, 300, 20)
  ok(undone and undone.result, json.encode(undone and undone.error or "no answer"))
  eq(order(), before)
  H.no_dialogs()
end)

-- M-7: a dialog, a render

test("review M-7: a dialog is open when an apply or a preview arrives: busy, nothing written", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local input = { ref = track_ref(epoch, 1), volumeDb = -6 }
  local previewed = preview(peer, "track-volume", input)
  mock.advance(100) -- (the timers run)
  mock.stall_timers = true -- the producer opened a dialog: Renoise's timers stop, its sockets still answer
  mock.advance(1000)
  failure(apply(peer, "track-volume", input, "Kumi: v (kr06)", previewed.fence, epoch, layout), "busy")
  failure(call(peer, "change_preview", { kind = "track-volume", input = input, epoch = epoch, layout = layout }), "busy")
  mock.stall_timers = false
  eq(mock.data.tracks[1].volume, 1, "nothing was written")
  eq(steps(), 0)
  -- the dialog closed: the same apply goes through
  mock.advance(200)
  local again = apply(peer, "track-volume", input, "Kumi: v (kr07)", previewed.fence, epoch, layout)
  ok(again.result, json.encode(again.error or {}))
  H.no_dialogs()
end)

test("review M-7: a restore asked behind a dialog is busy too, and writes nothing", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local input = { ref = track_ref(epoch, 1), volumeDb = -6 }
  local previewed = preview(peer, "track-volume", input)
  local done = apply(peer, "track-volume", input, "Kumi: v (kr08)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  mock.advance(100)
  mock.stall_timers = true
  mock.advance(1000)
  local before = steps()
  failure(restore(peer, "track-volume", { target = previewed.target, prior = previewed.prior, applied = done.result.applied }, "Kumi: undo v (kr09)", epoch, layout), "busy")
  mock.stall_timers = false
  eq(steps(), before)
  ok(mock.data.tracks[1].volume < 1, "still at -6 dB")
  H.no_dialogs()
end)

test("review M-7: a render starts after step 1 of a sliced restore: no later step is written until it ends", function()
  local env, peer = setup({ patch = constants({ SLICE_WRITES = 40 }) })
  fill(2)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local previewed, done = delete_track(peer, 2)
  local id = start_restore(peer, previewed, done, "Kumi: undo d (kr10)")
  local written = steps()
  mock.data.song_fields.rendering = true
  for _ = 1, 40 do
    ok(not wait(peer, id, 1, 50), "no answer while it waits")
  end
  eq(steps(), written, "nothing is written behind the render")
  ok(mock.song:pattern(3):track(2).is_empty, "the last step's lines are not there")
  mock.data.song_fields.rendering = false
  local response = wait(peer, id, 300, 20)
  ok(response and response.result, json.encode(response and response.error or "no answer"))
  eq(response.result.undo.steps, 3)
  ok(not mock.song:pattern(3):track(2).is_empty, "the render ended and the restore finished")
  H.no_dialogs()
end)

test("review M-7: a render that outlasts the wait: uncertain, nothing written behind it, the half-made track left", function()
  local env, peer = setup({ patch = constants({ SLICE_WRITES = 40, BUSY_WAIT_MS = 600 }) })
  fill(2)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local previewed, done = delete_track(peer, 2)
  local id = start_restore(peer, previewed, done, "Kumi: undo d (kr11)")
  local written = steps()
  mock.data.song_fields.rendering = true
  local response = wait(peer, id, 100, 50)
  mock.data.song_fields.rendering = false
  failure(response, "uncertain")
  eq(steps(), written, "nothing was written behind the render, the clean-up included")
  eq(names(), "Kick,Hat,Bass,Pad,Master,Send 1", "the half-made track is left for the producer")
  H.no_dialogs()
end)

-- M-3 (the tool's half): what the preview located is what the apply writes

test("review M-3: an apply that locates another track than the preview did is refused `changed`, nothing written", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local input = { ref = track_ref(epoch, 3), volumeDb = -6 } -- Bass, in Kumi's book
  mock.ops.insert_track(1, { name = "New" })
  H.tick(2)
  local previewed = call(peer, "change_preview", { kind = "track-volume", input = input, epoch = epoch, layout = layout }).result
  ok(previewed, "a preview in the older layout carries the ref")
  ok(previewed.target.ref:match(":track:4$"), previewed.target.ref)
  -- the host's old pairing: Kumi's old ref with the preview's (newer) layout, so the ref is taken as it is: Hat
  local wrong = call(peer, "change_apply", { kind = "track-volume", input = input, fence = previewed.fence, desc = "Kumi: v (kr12)",
    epoch = previewed.epoch, layout = previewed.layout })
  failure(wrong, "changed")
  eq(mock.data.tracks[3].volume, 1, "Hat was not changed")
  eq(mock.data.tracks[4].volume, 1)
  eq(steps(), 0)
  -- paired as Kumi asked (its own layout), the ref is carried to Bass
  local right = call(peer, "change_apply", { kind = "track-volume", input = input, fence = previewed.fence, desc = "Kumi: v (kr13)",
    epoch = epoch, layout = layout })
  ok(right.result, json.encode(right.error or {}))
  ok(math.abs(mock.data.tracks[4].volume - 10 ^ (-6 / 20)) < 1e-6, "Bass")
  eq(mock.data.tracks[3].volume, 1)
  eq(right.result.target.ref, track_ref(right.result.epoch, 4), "the answer names the track it located, in its own layout")
  H.no_dialogs()
end)

test("review M-3: notes written under the wrong pairing go to no other track", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local input = {
    pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 3),
    lines = { { line = 1, notes = { { col = 1, note = "C-4", instrument = "00" } } } },
  }
  mock.ops.insert_track(1, { name = "New" })
  H.tick(2)
  local previewed = call(peer, "change_preview", { kind = "notes-write", input = input, epoch = epoch, layout = layout }).result
  ok(previewed, "previewed")
  local wrong = call(peer, "change_apply", { kind = "notes-write", input = input, fence = previewed.fence, desc = "Kumi: n (kr14)",
    epoch = previewed.epoch, layout = previewed.layout })
  failure(wrong, "changed")
  ok(mock.song:pattern(1):track(3):line(1).is_empty, "Hat's line is empty")
  ok(mock.song:pattern(1):track(4):line(1).is_empty, "and so is Bass's: nothing was written")
  eq(steps(), 0)
  local right = call(peer, "change_apply", { kind = "notes-write", input = input, fence = previewed.fence, desc = "Kumi: n (kr15)",
    epoch = epoch, layout = layout })
  ok(right.result, json.encode(right.error or {}))
  ok(not mock.song:pattern(1):track(4):line(1).is_empty, "Bass got the note")
  ok(right.result.target.ref:match(":ptrack:1%.4$"), json.encode(right.result.target))
  H.no_dialogs()
end)

test("review M-3: the apply answer of a move names what it located", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local move = { ref = track_ref(epoch, 1), index = 3 }
  local moved = apply(peer, "track-move", move, "Kumi: m (kr16)", preview(peer, "track-move", move).fence, epoch, layout)
  ok(moved.result, json.encode(moved.error or {}))
  ok(moved.result.target, "a target")
  eq(moved.result.target.ref, track_ref(moved.result.epoch, 3))
  H.no_dialogs()
end)

-- M-5: the mixer device's automation

local function automation_points(t)
  local points = 0
  for _, pattern in ipairs(mock.data.patterns) do
    local pt = pattern.tracks[t]
    for _, entry in ipairs(pt and pt.automation or {}) do points = points + #entry.points end
  end
  return points
end

test("review M-5: a fade on a track's own volume and panning comes back with the track, also in a pattern with no lines", function()
  local env, peer = setup()
  mock.ops.automate(1, 3, 1, 1, { { time = 1, value = 0, scaling = 0 }, { time = 17, value = 1, scaling = 0 } })
  mock.ops.automate(3, 3, 1, 2, { { time = 1, value = 0.2, scaling = 0 }, { time = 9, value = 0.8, scaling = 0 }, { time = 20, value = 0.5, scaling = 0 } })
  fill(3, 1, 4) -- lines in pattern 1 only: the fade in pattern 3 is in a pattern with none
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  eq(automation_points(3), 5)
  local previewed, done = delete_track(peer, 3)
  eq(#done.material.automation, 2, "both fades are kept")
  eq(automation_points(3), 0)
  local undone = restore(peer, "track-delete", record_of(previewed, done), "Kumi: undo d (kr17)", done.epoch, done.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(automation_points(3), 5, "the volume and panning fades are back")
  H.no_dialogs()
end)

-- M-6: slot records hold epoch-categorised pattern refs, not bare numbers

local function seq_text() return table.concat(mock.data.sequence, ",") end

-- What Renoise's "delete unused patterns" does to the pool: the pattern goes, those after it move down one, and the sequence's
-- numbers follow.
local function delete_unused(p)
  for s, value in ipairs(mock.data.sequence) do
    if value > p then mock.data.sequence[s] = value - 1 end
  end
  mock.ops.remove_pattern(p)
  H.tick(2)
end

test("review M-6: a deleted slot, the pool renumbered by the producer, Kumi's undo: stale, not another pattern", function()
  local env, peer = setup()
  mock.data.patterns[2].name, mock.data.patterns[3].name = "Verse", "Drop"
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local epoch, layout = place(peer)
  local previewed = preview(peer, "slot-delete", { ref = seq_ref(epoch, 2) })
  local done = apply(peer, "slot-delete", { ref = seq_ref(epoch, 2) }, "Kumi: delete slot 2 (kr18)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  delete_unused(2)
  local before = steps()
  local now, now_layout = place(peer)
  local undone = restore(peer, "slot-delete", record_of(previewed, done.result), "Kumi: undo delete slot 2 (kr19)", now, now_layout)
  failure(undone, "stale")
  eq(steps(), before, "nothing was written")
  H.no_dialogs()
end)

test("review M-6: a slot's pattern changed, the pool renumbered, Kumi's undo: stale", function()
  local env, peer = setup(nil, { patterns = 4, sequence = { 1, 3, 3, 4 }, sequence_length = 4 })
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local epoch, layout = place(peer)
  local input = { ref = seq_ref(epoch, 2), pattern = pattern_ref(epoch, 1) }
  local previewed = preview(peer, "slot-pattern", input)
  local done = apply(peer, "slot-pattern", input, "Kumi: slot 2 plays 00 (kr20)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(seq_text(), "1,1,3,4")
  delete_unused(2)
  local before = steps()
  local now, now_layout = place(peer)
  failure(restore(peer, "slot-pattern", record_of(previewed, done.result), "Kumi: undo (kr21)", now, now_layout), "stale")
  eq(steps(), before)
  H.no_dialogs()
end)

-- S-2: the delay column

local function write_notes(peer, cells, desc)
  local epoch, layout = place(peer)
  local input = { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 3), lines = { { line = 1, notes = cells } } }
  local previewed = call(peer, "change_preview", { kind = "notes-write", input = input, epoch = epoch, layout = layout })
  if not previewed.result then return previewed end
  return apply(peer, "notes-write", input, desc, previewed.result.fence, epoch, layout)
end

test("review S-2: delay 00 is no delay and FF is 255", function()
  local env, peer = setup()
  local column = function(c) return mock.song:pattern(1):track(3):line(1):note_column(c) end
  local none = write_notes(peer, { { col = 1, note = "C-4", instrument = "00", delay = "00" } }, "Kumi: n (kr22)")
  ok(none.result, json.encode(none.error or {}))
  eq(column(1).delay_value, 0)
  eq(column(1).delay_string, "..")
  local late = write_notes(peer, { { col = 2, note = "D-4", instrument = "00", delay = "FF" } }, "Kumi: n (kr23)")
  ok(late.result, json.encode(late.error or {}))
  eq(column(2).delay_value, 255)
  local number = write_notes(peer, { { col = 1, note = "E-4", instrument = "00", delay = 128 } }, "Kumi: n (kr24)")
  ok(number.result, json.encode(number.error or {}))
  eq(column(1).delay_value, 128)
  H.no_dialogs()
end)

-- S-3 / S-4: places that follow the tracks made since

test("review S-3: a move put back after the producer made a track before both places", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local move = { ref = track_ref(epoch, 1), index = 3 }
  local previewed = preview(peer, "track-move", move)
  local done = apply(peer, "track-move", move, "Kumi: m (kr25)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(names(), "Hat,Bass,Kick,Pad,Master,Send 1")
  mock.ops.insert_track(1, { name = "New" })
  H.tick(2)
  local undone = restore(peer, "track-move", record_of(previewed, done.result), "Kumi: undo m (kr26)", done.result.epoch, done.result.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(names(), "New,Kick,Hat,Bass,Pad,Master,Send 1", "Kick is back among the tracks it was moved among")
  H.no_dialogs()
end)

test("review S-4: a deleted group member goes back into its group after a track was made before the group", function()
  local env, peer = setup()
  mock.song:insert_group_at(5)
  mock.song:add_track_to_group(3, 5)
  mock.song:add_track_to_group(3, 5)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local before = order()
  local previewed, done = delete_track(peer, 3)
  mock.ops.insert_track(1, { name = "New" })
  H.tick(2)
  local undone = restore(peer, "track-delete", record_of(previewed, done), "Kumi: undo d (kr27)", done.epoch, done.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(order(), "New," .. before)
  H.no_dialogs()
end)

test("review S-4: when the group is gone the track comes back outside it, and the answer says so", function()
  local env, peer = setup()
  mock.song:insert_group_at(5)
  mock.song:add_track_to_group(3, 5)
  mock.song:add_track_to_group(3, 5)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local previewed, done = delete_track(peer, 3)
  mock.ops.rename_track(4, "Renamed group") -- Pad is track 3 now, the group is track 4
  H.tick(2)
  local undone = restore(peer, "track-delete", record_of(previewed, done), "Kumi: undo d (kr28)", done.epoch, done.layout)
  ok(undone.result, json.encode(undone.error or {}))
  ok(undone.result.display.note and undone.result.display.note:find("isn't there any more", 1, true), json.encode(undone.result.display))
  H.no_dialogs()
end)

test("review S-3: a track put in a group is taken out again after the producer made a track before both places", function()
  local env, peer = setup()
  mock.song:insert_group_at(4)
  mock.song:track(4).name = "Bus"
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local epoch, layout = place(peer)
  local input = { ref = track_ref(epoch, 3), group = track_ref(epoch, 4) }
  local previewed = preview(peer, "track-group", input)
  local done = apply(peer, "track-group", input, "Kumi: g (kr30)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  eq(order(), "Kick,Hat,Bass(in),Bus[G],Pad,Master,Send 1")
  mock.ops.insert_track(1, { name = "New" })
  H.tick(2)
  local undone = restore(peer, "track-group", record_of(previewed, done.result), "Kumi: undo g (kr31)", done.result.epoch, done.result.layout)
  ok(undone.result, json.encode(undone.error or {}))
  eq(order(), "New,Kick,Hat,Bass,Bus[G],Pad,Master,Send 1")
  H.no_dialogs()
end)

-- S-6: the loop range names slots

test("review S-6: a loop range put back after the sequence changed is refused: its slots are not those slots any more", function()
  local env, peer = setup()
  local epoch, layout = place(peer)
  local input = { first = 2, last = 3 }
  local previewed = preview(peer, "loop-range", input)
  local done = apply(peer, "loop-range", input, "Kumi: loop (kr32)", previewed.fence, epoch, layout)
  ok(done.result, json.encode(done.error or {}))
  mock.ops.insert_sequence_slot(1, 1) -- the producer makes a slot before them
  H.tick(2)
  mock.song.transport.loop_sequence_range = { 3, 4 } -- (Renoise moved the range along with the slots)
  local now, now_layout = place(peer)
  failure(restore(peer, "loop-range", record_of(previewed, done.result), "Kumi: undo (kr33)", now, now_layout), "stale")
  eq(mock.song.transport.loop_sequence_range[1], 3, "the loop is as the producer has it")
  H.no_dialogs()
end)

-- S-7: budgets

test("review S-7: a track with more than can be put back in time is refused where it is read, and stays", function()
  local env, peer = setup({ patch = constants({ SLICE_WRITES = 40, MAX_RESTORE_STEPS = 4 }) })
  fill(2, 3, 10) -- 30 cells: well over the 160 writes 4 steps of 40 make
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  local epoch, layout = place(peer)
  local previewed = preview(peer, "track-delete", { ref = track_ref(epoch, 2) })
  failure(apply(peer, "track-delete", { ref = track_ref(epoch, 2) }, "Kumi: d (kr29)", previewed.fence, epoch, layout), "failed")
  eq(names(), "Kick,Hat,Bass,Pad,Master,Send 1", "nothing was deleted")
  eq(steps(), 0)
  H.no_dialogs()
end)

test("review S-7: one notes-write is no more than a callback's budget", function()
  local env, peer = setup(nil, { pattern_lines = 512 })
  mock.song:track(3).visible_note_columns = 12
  local epoch, layout = place(peer)
  local function lines(first, last)
    local out = {}
    for l = first, last do
      local notes = {}
      for c = 1, 12 do notes[c] = { col = c, note = "C-4", instrument = "00" } end
      out[#out + 1] = { line = l, notes = notes }
    end
    return out
  end
  local function write(first, last)
    return call(peer, "change_preview", {
      kind = "notes-write", input = { pattern = pattern_ref(epoch, 1), track = track_ref(epoch, 3), lines = lines(first, last) },
      epoch = epoch, layout = layout,
    })
  end
  local fine = write(1, 100)
  ok(fine.result, json.encode(fine.error or {}))
  local refused = write(1, 400) -- 4,800 cells
  ok(refused and refused.error, "400 lines of twelve columns are not")
  eq(refused.error.code, "bad-params")
  ok(refused.error.message:find("split", 1, true), refused.error.message)
  H.no_dialogs()
end)
