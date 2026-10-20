-- M4 (n-song): Kumi's changes to a track's settings (collapse, width, output routing and delay, the columns it shows, a note
-- column's name and mute): preview, apply, restore, the producer's change in between, the places a record keeps.
local T = ...
local mock = T.mock
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness
local K = assert(loadfile(T.tests_dir .. "change_kit.lua"))(T)

local function track(i) return mock.data.tracks[i] end
local MINUS = "\226\136\146"

-- Kick, Hat in the group Bus, Bass, Pad, then the master and a send.
local function grouped()
  return { tracks = {
    { name = "Kick" }, { name = "Hat" }, { name = "Bus", kind = "group", members = 1 }, { name = "Bass", note_columns = 2 }, { name = "Pad" },
  } }
end

test("track settings: collapse, width and delay change as one step each and go back as new ones", function()
  local _, peer = K.setup()
  local E = K.place(peer)
  local cases = {
    { "track-collapse", { ref = K.ref(E, "track", 1), collapsed = true }, "Kick collapsed", function() return track(1).collapsed == true end },
    { "track-width", { ref = K.ref(E, "track", 2), width = 50 }, "Hat width 50%", function() return math.abs((track(2).width or 0) - 63.5) < 1e-9 end },
    { "track-delay", { ref = K.ref(E, "track", 3), delayMs = -3.25 }, "Bass delay " .. MINUS .. "3.25 ms", function() return track(3).output_delay == -3.25 end },
    { "track-output", { ref = K.ref(E, "track", 4), routing = "built-in output" }, "Pad routed to \226\128\156Built-in Output\226\128\157", function() return track(4).output_routing == "Built-in Output" end },
  }
  for n, case in ipairs(cases) do
    local record = K.change(peer, case[1], case[2], "Kumi: " .. case[1] .. " (kt0" .. n .. ")")
    eq(record.result.display.title, case[3])
    eq(record.result.undo.entry, true, case[1] .. " is a Renoise undo step")
    eq(K.newest(), "Kumi: " .. case[1] .. " (kt0" .. n .. ")")
    ok(case[4](), case[1] .. " written")
    local undone = K.restore(peer, record)
    ok(undone.result, case[1] .. " restored")
    ok(not case[4](), case[1] .. " put back")
  end
  eq(K.steps(), 8)
  H.no_dialogs()
end)

test("track settings: the columns a track shows, and what hiding them hides", function()
  local _, peer = K.setup()
  local E = K.place(peer)
  local record = K.change(peer, "track-columns", { ref = K.ref(E, "track", 3), noteColumns = 4, volume = true, delay = true })
  eq(record.result.display.title, "Bass 4 note columns, the volume column, the delay column")
  eq(track(3).note_columns, 4)
  eq(track(3).volume_column, true)
  eq(track(3).delay_column, true)
  K.restore(peer, record)
  eq(track(3).note_columns, 2)
  eq(track(3).volume_column, false)
  -- a note in column 2 of Bass: hiding it says so
  mock.ops.set_note(1, 3, 5, 2, { note = 50, instrument = 0 })
  local previewed = K.preview(peer, "track-columns", { ref = K.ref(E, "track", 3), noteColumns = 1 })
  eq(previewed.display.note, "What it hides holds notes in 1 pattern: Renoise keeps them, hidden (and plays them).")
  local quiet = K.preview(peer, "track-columns", { ref = K.ref(E, "track", 1), effectColumns = 0 })
  eq(quiet.display.note, nil, "nothing hidden: no note")
  H.no_dialogs()
end)

test("track settings: a note column's name and mute change and go back, the column kept in the record", function()
  local _, peer = K.setup()
  local E = K.place(peer)
  local record = K.change(peer, "track-column", { ref = K.ref(E, "track", 3), col = 2, name = "Lead", mute = true })
  eq(record.result.display.title, "Bass column 2 named \226\128\156Lead\226\128\157, muted")
  eq(record.result.undo.entry, true, "a column's mute is an undo step, unlike the track's")
  eq(track(3).column_names[2], "Lead")
  eq(track(3).muted_columns[2], true)
  eq(record.prior.col, 2)
  -- a track made before Bass: the record's track is carried, its column kept
  mock.ops.insert_track(1, { name = "Intro" })
  local undone = K.restore(peer, record)
  ok(undone.result, "restored")
  eq(track(4).name, "Bass")
  eq(track(4).column_names[2], nil)
  eq(track(4).muted_columns[2], nil)
  H.no_dialogs()
end)

test("track settings: what a track can't have is bad-params", function()
  local _, peer = K.setup(nil, grouped())
  local E = K.place(peer)
  local function bad(kind, input)
    K.failure(K.ask(peer, "change_preview", kind, input), "bad-params")
  end
  bad("track-collapse", { ref = K.ref(E, "track", 1), groupCollapsed = true }) -- (not a group)
  bad("track-collapse", { ref = K.ref(E, "track", 1) })
  bad("track-width", { ref = K.ref(E, "track", 1), width = 101 })
  bad("track-delay", { ref = K.ref(E, "track", 3), delayMs = 3 }) -- a group
  bad("track-delay", { ref = K.ref(E, "track", 7), delayMs = 3 }) -- a send
  bad("track-delay", { ref = K.ref(E, "track", 6), delayMs = 3 }) -- the master
  bad("track-delay", { ref = K.ref(E, "track", 1), delayMs = 150 })
  bad("track-output", { ref = K.ref(E, "track", 1), routing = "Bus" }) -- (Kick isn't in the group)
  bad("track-output", { ref = K.ref(E, "track", 1), routing = "" })
  bad("track-columns", { ref = K.ref(E, "track", 1), noteColumns = 13 })
  bad("track-columns", { ref = K.ref(E, "track", 1), noteColumns = 0 })
  bad("track-columns", { ref = K.ref(E, "track", 3), noteColumns = 2 }) -- a group has no note columns
  bad("track-columns", { ref = K.ref(E, "track", 3), effectColumns = 0 })
  bad("track-columns", { ref = K.ref(E, "track", 6), volume = true })
  bad("track-column", { ref = K.ref(E, "track", 6), col = 1, name = "x" })
  bad("track-column", { ref = K.ref(E, "track", 1), col = 13, mute = true })
  bad("track-column", { ref = K.ref(E, "track", 1), col = 1, name = "two\nlines" })
  local message = K.failure(K.ask(peer, "change_preview", "track-output", { ref = K.ref(E, "track", 1), routing = "Nowhere" }), "bad-params")
  ok(message:find("Master, Built-in Output", 1, true), "the routings it can take: " .. message)
  eq(K.steps(), 0)
  H.no_dialogs()
end)

test("track settings: a group collapses its tracks, and the producer's change in between refuses the undo", function()
  local _, peer = K.setup(nil, grouped())
  local E = K.place(peer)
  local record = K.change(peer, "track-collapse", { ref = K.ref(E, "track", 3), groupCollapsed = true })
  eq(record.result.display.title, "Bus with its tracks folded")
  eq(track(3).group_collapsed, true)
  ok(K.restore(peer, record).result)
  eq(track(3).group_collapsed, false)
  local delay = K.change(peer, "track-delay", { ref = K.ref(E, "track", 4), delayMs = 10 })
  track(4).output_delay = 20
  K.failure(K.restore(peer, delay), "changed")
  eq(track(4).output_delay, 20, "the producer's stays")
  ok(K.restore(peer, delay, nil, { discard = true }).result)
  eq(track(4).output_delay, 0)
  H.no_dialogs()
end)

test("track settings: a routing to a group is kept as the group (its name moves with its place), not as its name", function()
  local _, peer = K.setup(nil, { tracks = {
    { name = "Kick" }, { name = "Hat" }, { name = "Group 03", kind = "group", members = 1 }, { name = "Pad" },
  } })
  local E = K.place(peer)
  -- Hat (in the group) goes to the master; the group it went to before is a track, followed through what is made before it
  local record = K.change(peer, "track-output", { ref = K.ref(E, "track", 2), routing = "Master" })
  eq(record.prior.routing, "Group 03")
  ok(record.prior.at, "the group as a ref")
  eq(track(2).output_routing, "Master")
  mock.ops.insert_track(1, { name = "Intro" })
  track(4).name = "Group 04" -- (Renoise renames a group named by its place)
  local undone = K.restore(peer, record)
  ok(undone.result, "restored")
  eq(track(3).name, "Hat")
  eq(track(3).output_routing, "Group 04", "the group it was routed to, by its name now")
  H.no_dialogs()
end)

test("track settings: an apply whose routing is gone since the preview writes nothing", function()
  local _, peer = K.setup(nil, { tracks = {
    { name = "Kick" }, { name = "Hat" }, { name = "Group 03", kind = "group", members = 1 }, { name = "Pad" },
  } })
  local E = K.place(peer)
  track(2).output_routing = "Master"
  local previewed, epoch, layout = K.preview(peer, "track-output", { ref = K.ref(E, "track", 2), routing = "Group 03" })
  track(3).name = "Drums" -- the group is another name now: "Group 03" is no group
  local refused = K.apply(peer, "track-output", { ref = K.ref(E, "track", 2), routing = "Group 03" }, "Kumi: o (ko01)", previewed.fence, epoch, layout)
  ok(refused.error, "refused")
  eq(track(2).output_routing, "Master", "nothing written")
  eq(K.steps(), 0)
  H.no_dialogs()
end)

test("track settings: Renoise's undo of a track setting is told", function()
  local _, peer = K.setup()
  local E = K.place(peer)
  K.change(peer, "track-collapse", { ref = K.ref(E, "track", 1), collapsed = true }, "Kumi: collapse (kc01)")
  K.change(peer, "track-columns", { ref = K.ref(E, "track", 2), panning = true }, "Kumi: columns (kc02)")
  K.change(peer, "track-width", { ref = K.ref(E, "track", 3), width = 25 }, "Kumi: width (kc03)")
  K.told(peer)
  mock.song:undo()
  eq(K.told(peer), "undo:Kumi: width (kc03)")
  mock.song:undo()
  eq(K.told(peer), "undo:Kumi: columns (kc02)")
  mock.song:undo()
  eq(K.told(peer), "undo:Kumi: collapse (kc01)")
  H.no_dialogs()
end)

test("track settings: a track made or deleted before the track is followed, one deleted is gone, a swap is stale", function()
  local _, peer = K.setup()
  local E = K.place(peer)
  local width = K.change(peer, "track-width", { ref = K.ref(E, "track", 2), width = 10 })
  local column = K.change(peer, "track-column", { ref = K.ref(E, "track", 3), col = 1, name = "Root" })
  local delay = K.change(peer, "track-delay", { ref = K.ref(E, "track", 4), delayMs = 4 })
  mock.ops.insert_track(1, { name = "Intro" })
  mock.ops.remove_track(1)
  mock.ops.remove_track(1) -- Kick: Hat is first now
  ok(K.restore(peer, width).result, "Hat found where it went")
  eq(track(1).name, "Hat")
  eq(track(1).width, 0)
  mock.ops.remove_track(2) -- Bass
  K.failure(K.restore(peer, column), "gone")
  eq(track(2).name, "Pad")
  -- a swap moves the epoch: a record from before it is stale, and nothing is written
  mock.ops.swap_tracks(1, 2)
  K.failure(K.restore(peer, delay), "stale")
  eq(track(1).output_delay, 4)
  H.no_dialogs()
end)

test("track settings: the track rows show the settings", function()
  local _, peer = K.setup(nil, grouped())
  track(4).output_delay = 2.5
  track(2).width = 31.75
  local rows = K.call(peer, "read", { kind = "track" }).result.items
  eq(rows[1].collapsed, false)
  eq(rows[2].width, 25)
  eq(rows[2].outputRouting, "Bus")
  eq(rows[3].groupCollapsed, false)
  eq(rows[3].outputDelay, nil, "a group has no output delay")
  eq(rows[4].outputDelay, 2.5)
  eq(rows[4].subColumns.volume, false)
  eq(rows[3].subColumns, nil, "a group has no note columns")
  eq(rows[6].outputRouting, "Built-in Output")
  H.no_dialogs()
end)
