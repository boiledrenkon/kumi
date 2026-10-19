-- M4 (n-song): track-solo keeps every track's mute and solo. Renoise solos by turning the other tracks Off and ends a solo by
-- turning every track Active, so the record is a snapshot of them all: the producer's mutes come back with Kumi's undo, a
-- change in between refuses it, several tracks are heard alone with `with`, and the snapshot's tracks are found where they went.
local T = ...
local mock = T.mock
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness
local K = assert(loadfile(T.tests_dir .. "change_kit.lua"))(T)

local ACTIVE, MUTED = 1, 3

-- Kick, Hat, Bass, Pad (muted by the producer), the master and a send; `group` puts Kick and Hat in the group Drums.
local function song(group)
  local tracks = { { name = "Kick" }, { name = "Hat" }, { name = "Bass" }, { name = "Pad", mute_state = MUTED } }
  if group then table.insert(tracks, 3, { name = "Drums", kind = "group", members = 2 }) end
  return { tracks = tracks }
end

-- Every non-master track as name=A|O|M (active, off, muted), * when it reads soloed.
local function states()
  local out = {}
  for _, t in ipairs(mock.data.tracks) do
    if t.kind ~= "master" then out[#out + 1] = t.name .. "=" .. ({ "A", "O", "M" })[t.mute_state] .. (t.solo and "*" or "") end
  end
  return table.concat(out, " ")
end

local function solo(peer, E, t, on, more)
  local input = { ref = K.ref(E, "track", t), solo = on }
  for k, v in pairs(more or {}) do input[k] = v end
  return K.change(peer, "track-solo", input, "Kumi: solo (ks" .. t .. ")")
end

local function note_has(display, text)
  ok(display.note and display.note:find(text, 1, true), "note has " .. text .. ": " .. tostring(display.note))
end

test("track solo: the producer's muted track, turned Off by Renoise's solo, is muted again after Kumi's undo", function()
  local _, peer = K.setup(nil, song())
  local E = K.place(peer)
  eq(states(), "Kick=A Hat=A Bass=A Pad=M Send 1=A")
  local previewed = K.preview(peer, "track-solo", { ref = K.ref(E, "track", 1), solo = true })
  note_has(previewed.display, "turning every other track Off")
  note_has(previewed.display, "Renoise's own undo won't take this back")
  eq(#previewed.prior.tracks, 5, "every track but the master")
  eq(states(), "Kick=A Hat=A Bass=A Pad=M Send 1=A", "a preview writes nothing")
  local record = solo(peer, E, 1, true)
  eq(record.result.display.title, "Kick soloed")
  eq(states(), "Kick=A* Hat=O Bass=O Pad=O Send 1=O")
  note_has(record.result.display, "Renoise turned 4 other tracks Off.")
  note_has(record.result.display, "Kumi's undo puts each track back as it was.")
  eq(record.result.undo.entry, false)
  eq(K.steps(), 0, "no undo step in Renoise")
  local undone = K.restore(peer, record, "Kumi: undo solo (ks9)")
  ok(undone.result, "restored")
  eq(states(), "Kick=A Hat=A Bass=A Pad=M Send 1=A")
  eq(undone.result.display.title, "Kick unsoloed (put back)")
  note_has(undone.result.display, "\226\128\156Pad\226\128\157 muted again")
  -- a retry writes nothing
  local again = K.restore(peer, record)
  eq(again.result.display.note, "It was back already: nothing was written.")
  eq(K.steps(), 0)
  H.no_dialogs()
end)

test("track solo: a mute or solo the producer changed after Kumi's solo refuses its undo; discard puts every track back", function()
  local _, peer = K.setup(nil, song())
  local E = K.place(peer)
  local record = solo(peer, E, 1, true)
  mock.ops.mute_track(2, ACTIVE) -- the producer hears Hat along with Kick
  local message = K.failure(K.restore(peer, record), "changed")
  ok(message:find("\226\128\156Hat\226\128\157's mute or solo changed in Renoise since Kumi's solo", 1, true), message)
  eq(states(), "Kick=A* Hat=A Bass=O Pad=O Send 1=O", "nothing written")
  local forced = K.restore(peer, record, nil, { discard = true })
  ok(forced.result, "discard goes through")
  eq(states(), "Kick=A Hat=A Bass=A Pad=M Send 1=A")
  -- the producer's own solo since Kumi's: refused too
  local again = solo(peer, E, 1, true)
  mock.ops.solo(3, true)
  K.failure(K.restore(peer, again), "changed")
  eq(states(), "Kick=O Hat=O Bass=A* Pad=O Send 1=O")
  H.no_dialogs()
end)

test("track solo: a mute the producer changed between the preview and the apply refuses the apply, with nothing soloed", function()
  local _, peer = K.setup(nil, song())
  local E = K.place(peer)
  local input = { ref = K.ref(E, "track", 1), solo = true }
  local previewed, epoch, layout = K.preview(peer, "track-solo", input)
  mock.ops.mute_track(3, MUTED) -- the producer mutes Bass
  local message = K.failure(K.apply(peer, "track-solo", input, "Kumi: solo (ks1)", previewed.fence, epoch, layout), "changed")
  ok(message:find("mute or solo changed since the preview", 1, true), message)
  eq(states(), "Kick=A Hat=A Bass=M Pad=M Send 1=A")
  H.no_dialogs()
end)

test("track solo: Kumi's solo ends the producer's (Renoise's solo is exclusive); the undo brings it back with what they turned on", function()
  local _, peer = K.setup(nil, song())
  local E = K.place(peer)
  mock.ops.solo(3, true)
  mock.ops.mute_track(2, ACTIVE) -- Hat heard along with Bass
  eq(states(), "Kick=O Hat=A Bass=A* Pad=O Send 1=O")
  local previewed = K.preview(peer, "track-solo", { ref = K.ref(E, "track", 1), solo = true })
  note_has(previewed.display, "Renoise's solo is exclusive: \226\128\156Bass\226\128\157 stops being soloed.")
  local record = solo(peer, E, 1, true)
  eq(states(), "Kick=A* Hat=O Bass=O Pad=O Send 1=O")
  note_has(record.result.display, "\226\128\156Bass\226\128\157 isn't soloed any more (a solo is exclusive).")
  ok(K.restore(peer, record).result, "restored")
  eq(states(), "Kick=O Hat=A Bass=A* Pad=O Send 1=O")
  H.no_dialogs()
end)

test("track solo: a group's track brings its group; un-soloing the group ends the whole solo; undo finds whose solo it was", function()
  local _, peer = K.setup(nil, song(true))
  local E = K.place(peer)
  local record = solo(peer, E, 1, true)
  eq(states(), "Kick=A* Hat=O Drums=A* Bass=O Pad=O Send 1=O")
  -- Drums reads soloed along with Kick: Renoise's setter would solo it on its own, so the un-solo goes on until no solo is left
  local unsolo = solo(peer, E, 3, false)
  eq(unsolo.result.display.title, "Drums unsoloed")
  eq(states(), "Kick=A Hat=A Drums=A Bass=A Pad=A Send 1=A")
  note_has(unsolo.result.display, "Renoise turned 4 tracks back on.")
  ok(K.restore(peer, unsolo).result, "the un-solo undone")
  eq(states(), "Kick=A* Hat=O Drums=A* Bass=O Pad=O Send 1=O", "Kick's own solo, not the group's")
  ok(K.restore(peer, record).result, "the solo undone")
  eq(states(), "Kick=A Hat=A Drums=A Bass=A Pad=M Send 1=A")
  -- the group soloed while it reads soloed along with its track: its own solo
  mock.ops.solo(1, true)
  local own = solo(peer, E, 3, true)
  eq(states(), "Kick=A* Hat=A* Drums=A* Bass=O Pad=O Send 1=O")
  ok(K.restore(peer, own).result)
  eq(states(), "Kick=A* Hat=O Drums=A* Bass=O Pad=O Send 1=O")
  -- a track soloed on its own already: the tracks the producer turned on since stay
  mock.ops.mute_track(4, ACTIVE)
  local same = solo(peer, E, 1, true)
  eq(states(), "Kick=A* Hat=O Drums=A* Bass=A Pad=O Send 1=O")
  note_has(same.result.display, "It was soloed on its own already: nothing changed.")
  H.no_dialogs()
end)

test("track solo: with, the other tracks heard alone with it, are turned on with what they play through", function()
  local _, peer = K.setup(nil, song(true))
  local E = K.place(peer)
  local record = solo(peer, E, 4, true, { with = { K.ref(E, "track", 1), K.ref(E, "track", 5), K.ref(E, "track", 1) } })
  eq(record.result.display.title, "Bass soloed, with \226\128\156Kick\226\128\157 and \226\128\156Pad\226\128\157")
  eq(states(), "Kick=A Hat=O Drums=A Bass=A* Pad=A Send 1=O")
  note_has(record.result.display, "A script solos one track at a time: \226\128\156Kick\226\128\157 and \226\128\156Pad\226\128\157 are on instead.")
  ok(K.restore(peer, record).result)
  eq(states(), "Kick=A Hat=A Drums=A Bass=A Pad=M Send 1=A")
  K.failure((K.ask(peer, "change_preview", "track-solo", { ref = K.ref(E, "track", 4), solo = false, with = { K.ref(E, "track", 1) } })), "bad-params")
  K.failure((K.ask(peer, "change_preview", "track-solo", { ref = K.ref(E, "track", 4), solo = true, with = { K.ref(E, "track", 6) } })), "bad-params")
  K.failure((K.ask(peer, "change_preview", "track-solo", { ref = K.ref(E, "track", 6), solo = true })), "bad-params")
  K.failure((K.ask(peer, "change_preview", "track-solo", { ref = K.ref(E, "track", 4), solo = "yes" })), "bad-params")
  H.no_dialogs()
end)

test("track solo: a track's solo brings the sends it feeds, and a send's the tracks feeding it", function()
  local tracks = song().tracks
  tracks[1].devices = { { path = "Audio/Effects/Native/#Send", name = "#Send", params = {
    { name = "Amount", value = 1, min = 0, max = 1 }, { name = "Panning", value = 0.5, min = 0, max = 1 }, { name = "Receiver", value = 0, min = -1, max = 8 },
  } } }
  local _, peer = K.setup(nil, { tracks = tracks })
  local E = K.place(peer)
  local record = solo(peer, E, 1, true)
  eq(states(), "Kick=A* Hat=O Bass=O Pad=O Send 1=A*")
  ok(K.restore(peer, record).result)
  eq(states(), "Kick=A Hat=A Bass=A Pad=M Send 1=A")
  local send = solo(peer, E, 6, true)
  eq(states(), "Kick=A* Hat=O Bass=O Pad=O Send 1=A*")
  ok(K.restore(peer, send).result)
  eq(states(), "Kick=A Hat=A Bass=A Pad=M Send 1=A")
  H.no_dialogs()
end)

test("track solo: tracks made, deleted or renamed between the solo and its undo; a swap is stale; the soloed track deleted is gone", function()
  local _, peer = K.setup(nil, song())
  local E = K.place(peer)
  local record = solo(peer, E, 3, true)
  eq(states(), "Kick=O Hat=O Bass=A* Pad=O Send 1=O")
  mock.ops.insert_track(1, { name = "New" }) -- Off: made during a solo
  mock.ops.remove_track(3) -- Hat
  eq(states(), "New=O Kick=O Bass=A* Pad=O Send 1=O")
  local undone = K.restore(peer, record)
  ok(undone.result, "restored")
  eq(states(), "New=A Kick=A Bass=A Pad=M Send 1=A")
  note_has(undone.result.display, "A track deleted since isn't in it.")
  -- a track renamed since isn't known for the one it was: refused, and discard leaves it as Renoise does
  local again = solo(peer, E, 4, true)
  mock.ops.rename_track(1, "Newer")
  mock.ops.mute_track(1, MUTED)
  local message = K.failure(K.restore(peer, again), "changed")
  ok(message:find("isn't the track it was", 1, true), message)
  local forced = K.restore(peer, again, nil, { discard = true })
  ok(forced.result, "discard")
  eq(states(), "Newer=M Kick=A Bass=A Pad=M Send 1=A", "the track that isn't the one it was keeps the producer's mute")
  note_has(forced.result.display, "A track that isn't the track it was is left as Renoise left it.")
  -- tracks swapped since: Renoise's track refs don't survive a swap, so nothing is written
  local swapped = solo(peer, E, 2, true)
  mock.ops.swap_tracks(3, 4)
  eq(states(), "Newer=O Kick=A* Pad=O Bass=O Send 1=O")
  K.failure(K.restore(peer, swapped), "stale")
  eq(states(), "Newer=O Kick=A* Pad=O Bass=O Send 1=O")
  -- the soloed track deleted (Renoise ends the solo): gone
  local E2 = K.place(peer)
  mock.ops.solo(2, false)
  local last = solo(peer, E2, 2, true)
  mock.ops.remove_track(2)
  eq(states(), "Newer=A Pad=A Bass=A Send 1=A")
  K.failure(K.restore(peer, last), "gone")
  H.no_dialogs()
end)

test("track solo: a solo of two tracks made in Renoise comes back as one solo, the other track playing, and the answer says so", function()
  local _, peer = K.setup(nil, song())
  local E = K.place(peer)
  mock.ops.solo(1, true)
  mock.ops.add_solo(3)
  eq(states(), "Kick=A* Hat=O Bass=A* Pad=O Send 1=O")
  local record = solo(peer, E, 4, true)
  eq(states(), "Kick=O Hat=O Bass=O Pad=A* Send 1=O")
  local undone = K.restore(peer, record)
  ok(undone.result, "restored")
  eq(states(), "Kick=A Hat=O Bass=A* Pad=O Send 1=O")
  note_has(undone.result.display, "Renoise lets a script solo one track at a time: \226\128\156Kick\226\128\157 plays but isn't shown soloed.")
  H.no_dialogs()
end)
