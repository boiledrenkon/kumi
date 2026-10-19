-- M4 (n-song): Kumi's changes to a device parameter's automation in one pattern: points added, changed and removed, a range
-- cleared, the play mode, the envelope deleted; Kumi's undo puts the envelope back whole while it is as Kumi left it.
local T = ...
local mock = T.mock
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness
local K = assert(loadfile(T.tests_dir .. "change_kit.lua"))(T)
local json = T.load_payload_module("json")

-- Bass (track 3) has a Gainer after its mixer device: Gain 0..4, Panning 0..1.
local function setup()
  local gainer = {
    name = "Gainer", path = "Audio/Effects/Native/Gainer",
    params = { { name = "Gain", value = 1, min = 0, max = 4 }, { name = "Panning", value = 0.5, min = 0, max = 1 } },
  }
  local env, peer = K.setup(nil, { tracks = {
    { name = "Kick" }, { name = "Hat" }, { name = "Bass", note_columns = 2, devices = { gainer } }, { name = "Pad" },
  } })
  return env, peer
end

local function envelope(p, t, d, k)
  local identity = mock.data.tracks[t].devices[d].params[k]
  for _, automation in ipairs((mock.data.patterns[p].tracks[t] or {}).automation or {}) do
    if automation.parameter == identity then return automation end
  end
  return nil
end

local function points_text(automation)
  if not automation then return "none" end
  local out = {}
  for _, point in ipairs(automation.points) do out[#out + 1] = string.format("%g=%g", point.time, point.value) end
  return table.concat(out, " ") .. " " .. ({ "points", "linear", "curves" })[automation.playmode]
end

test("automation: an envelope made with points, as one step; Kumi's undo takes it away again", function()
  local _, peer = setup()
  local E = K.place(peer)
  local input = { ref = K.ref(E, "device", 3, 2), param = "gain", pattern = K.ref(E, "seq", 1),
    points = { { line = 1, value = 0.25 }, { line = 9, tick = 3, paramValue = 2 }, { time = 17, value = 1, scaling = 0.5 } }, playmode = "curves" }
  local previewed = K.preview(peer, "automation-write", input)
  eq(previewed.display.title, "Automation of Gainer Gain on \226\128\156Bass\226\128\157 in \226\128\156Pattern 00\226\128\157: 3 points, curves")
  eq(previewed.prior.exists, false)
  eq(envelope(1, 3, 2, 1), nil, "a preview writes nothing")
  local record = K.change(peer, "automation-write", input, "Kumi: automate (ku01)")
  eq(record.target.ref, K.ref(record.epoch, "param", 3, 2, 1))
  eq(points_text(envelope(1, 3, 2, 1)), "1=0.25 9.5=0.5 17=1 curves", "a tick is 1/TPL of a line (TPL 6: tick 3 is half)")
  eq(K.steps(), 1)
  eq(mock.data.tracks[3].devices[2].params[1].automated, true)
  ok(K.restore(peer, record).result)
  eq(envelope(1, 3, 2, 1), nil, "the envelope Kumi made is gone")
  eq(K.steps(), 2)
  H.no_dialogs()
end)

test("automation: points changed, removed and a range cleared in an envelope, and Kumi's undo puts its points back exactly", function()
  local _, peer = setup()
  local E = K.place(peer)
  mock.ops.automate(1, 3, 2, 2, { { time = 1, value = 0.1, scaling = 0 }, { time = 5, value = 0.2, scaling = 0 }, { time = 9, value = 0.3, scaling = 0 }, { time = 20, value = 0.4, scaling = 0 } }, 2)
  local record = K.change(peer, "automation-write", { ref = K.ref(E, "param", 3, 2, 2), pattern = K.ref(E, "pattern", 1),
    clear = { from = 4, to = 10 }, remove = { 20 }, points = { { time = 1, value = 0.9 }, { time = 2.5, value = 0.6 } }, playmode = "points" })
  eq(points_text(envelope(1, 3, 2, 2)), "1=0.9 2.5=0.6 points")
  eq(record.result.display.title, "Automation of Gainer Panning on \226\128\156Bass\226\128\157 in \226\128\156Pattern 00\226\128\157: 2 points, points")
  ok(K.restore(peer, record).result)
  eq(points_text(envelope(1, 3, 2, 2)), "1=0.1 5=0.2 9=0.3 20=0.4 linear")
  H.no_dialogs()
end)

test("automation: a deleted envelope is made again by Kumi's undo, the mixer's volume too", function()
  local _, peer = setup()
  local E = K.place(peer)
  mock.ops.automate(2, 1, 1, 2, { { time = 1, value = 0.5, scaling = 0 }, { time = 16, value = 0.7, scaling = 0.2 } }, 3)
  local record = K.change(peer, "automation-write", { ref = K.ref(E, "param", 1, 1, 2), pattern = K.ref(E, "pattern", 2), delete = true })
  eq(record.result.display.title, "Automation of Track Vol/Pan Volume on \226\128\156Kick\226\128\157 in \226\128\156Pattern 01\226\128\157 deleted")
  eq(envelope(2, 1, 1, 2), nil)
  ok(K.restore(peer, record).result)
  eq(points_text(envelope(2, 1, 1, 2)), "1=0.5 16=0.7 curves")
  eq(envelope(2, 1, 1, 2).points[2].scaling, 0.2)
  H.no_dialogs()
end)

test("automation: the producer's points in between refuse Kumi's undo; discard puts the envelope back anyway", function()
  local _, peer = setup()
  local E = K.place(peer)
  local record = K.change(peer, "automation-write", { ref = K.ref(E, "param", 3, 2, 1), pattern = K.ref(E, "pattern", 3), points = { { time = 1, value = 0.5 } } })
  local automation = envelope(3, 3, 2, 1)
  automation.points = { { time = 1, value = 0.5, scaling = 0 }, { time = 8, value = 0.8, scaling = 0 } } -- the producer adds one
  local message = K.failure(K.restore(peer, record), "changed")
  ok(message:find("automation changed in Renoise since Kumi set it", 1, true), message)
  eq(#envelope(3, 3, 2, 1).points, 2)
  ok(K.restore(peer, record, nil, { discard = true }).result)
  eq(envelope(3, 3, 2, 1), nil)
  H.no_dialogs()
end)

test("automation: an apply after the producer changed the envelope is refused before anything is written (the fence)", function()
  local _, peer = setup()
  local E = K.place(peer)
  mock.ops.automate(1, 3, 2, 1, { { time = 1, value = 0.1, scaling = 0 } }, 2)
  local input = { ref = K.ref(E, "param", 3, 2, 1), pattern = K.ref(E, "pattern", 1), points = { { time = 4, value = 0.4 } } }
  local previewed, epoch, layout = K.preview(peer, "automation-write", input)
  envelope(1, 3, 2, 1).points = { { time = 1, value = 0.2, scaling = 0 } }
  K.failure(K.apply(peer, "automation-write", input, "Kumi: a (ku09)", previewed.fence, epoch, layout), "changed")
  eq(points_text(envelope(1, 3, 2, 1)), "1=0.2 linear", "nothing written")
  eq(K.steps(), 0)
  H.no_dialogs()
end)

test("automation: bad points and parameters are bad-params", function()
  local _, peer = setup()
  local E = K.place(peer)
  local function bad(input)
    input.ref = input.ref or K.ref(E, "param", 3, 2, 1)
    input.pattern = input.pattern or K.ref(E, "pattern", 1)
    K.failure(K.ask(peer, "change_preview", "automation-write", input), "bad-params")
  end
  bad({ points = { { time = 0.5, value = 0.1 } } })
  bad({ points = { { time = 33, value = 0.1 } } }) -- (32 lines)
  bad({ points = { { time = 2, value = 1.5 } } })
  bad({ points = { { time = 2, paramValue = 5 } } })
  bad({ points = { { time = 2, value = 0.1, scaling = 2 } } })
  bad({ points = { { time = 2, value = 0.1 }, { time = 2, value = 0.2 } } })
  bad({ points = { { line = 2, tick = 6, value = 0.1 } } })
  bad({ remove = { 3 } })
  bad({ clear = { from = 5, to = 2 } })
  bad({ playmode = "smooth" })
  bad({ delete = true, playmode = "points" })
  bad({})
  bad({ ref = K.ref(E, "device", 3, 2), param = "Cutoff", points = { { time = 1, value = 0 } } })
  mock.data.tracks[3].devices[2].params[2].automatable = false
  bad({ ref = K.ref(E, "param", 3, 2, 2), points = { { time = 1, value = 0 } } })
  mock.ops.alias(2, 3, 1)
  bad({ pattern = K.ref(E, "pattern", 2), points = { { time = 1, value = 0 } } })
  eq(K.steps(), 0)
  H.no_dialogs()
end)

test("automation: Renoise's undo of Kumi's points is told", function()
  local _, peer = setup()
  local E = K.place(peer)
  K.change(peer, "automation-write", { ref = K.ref(E, "param", 3, 2, 1), pattern = K.ref(E, "pattern", 1), points = { { time = 1, value = 0.5 } } }, "Kumi: make (ku21)")
  K.change(peer, "automation-write", { ref = K.ref(E, "param", 3, 2, 1), pattern = K.ref(E, "pattern", 1), points = { { time = 9, value = 0.7 } } }, "Kumi: add (ku22)")
  K.told(peer)
  mock.song:undo()
  eq(K.told(peer), "undo:Kumi: add (ku22)")
  mock.song:undo()
  eq(K.told(peer), "undo:Kumi: make (ku21)")
  H.no_dialogs()
end)

test("automation: a track made before follows the record; a device made on any track or a pattern made makes it stale", function()
  local _, peer = setup()
  local E = K.place(peer)
  local first = K.change(peer, "automation-write", { ref = K.ref(E, "param", 3, 2, 1), pattern = K.ref(E, "pattern", 1), points = { { time = 1, value = 0.5 } } })
  local second = K.change(peer, "automation-write", { ref = K.ref(E, "param", 3, 2, 2), pattern = K.ref(E, "pattern", 2), points = { { time = 2, value = 0.5 } } })
  local third = K.change(peer, "automation-write", { ref = K.ref(E, "param", 3, 2, 1), pattern = K.ref(E, "pattern", 3), points = { { time = 3, value = 0.5 } } })
  mock.ops.insert_track(1, { name = "Intro" })
  ok(K.restore(peer, first).result, "followed")
  eq(envelope(1, 4, 2, 1), nil)
  mock.ops.insert_pattern()
  K.failure(K.restore(peer, second), "stale")
  ok(envelope(2, 4, 2, 2), "nothing written")
  mock.ops.insert_device(2, { name = "Gate", params = {} })
  K.failure(K.restore(peer, third), "stale")
  H.no_dialogs()
end)

test("automation: the read lists a pattern track's envelopes, and the pattern body shows its lanes", function()
  local _, peer = setup()
  local E = K.place(peer)
  mock.ops.automate(1, 3, 2, 1, { { time = 1, value = 0.25, scaling = 0 }, { time = 30, value = 1, scaling = 0 } }, 3)
  local rows = K.call(peer, "read", { kind = "automation", parent = K.ref(E, "pattern", 1), track = 3 }).result.items
  eq(#rows, 1)
  eq(rows[1].ref, K.ref(E, "param", 3, 2, 1))
  eq(rows[1].device, "Gainer")
  eq(rows[1].parameter, "Gain")
  eq(rows[1].playmode, "curves")
  eq(#rows[1].points, 2)
  eq(rows[1].valueMax, 4)
  local body = K.call(peer, "focus_body", { kind = "pattern", ref = K.ref(E, "pattern", 1), window = { from = 1, to = 16 } }).result
  local lanes = body.tracks[3].automation
  eq(#lanes, 1)
  eq(lanes[1].name, "Gainer Gain")
  eq(#lanes[1].points, 1, "only the points in the window")
  eq(body.tracks[1].automation, nil)
  H.no_dialogs()
end)
