-- M4 (n-song): notes transposed and humanized, a block of lines copied or moved: what is written, what is refused, and Kumi's
-- undo by the plain rule (nothing in the pattern changed since Kumi's step).
local T = ...
local mock = T.mock
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness
local K = assert(loadfile(T.tests_dir .. "change_kit.lua"))(T)

local function column(p, t, l, c) return mock.song:pattern(p):track(t):line(l):note_column(c) end
local function notes(p, t, from, to, c)
  local out = {}
  for l = from, to do
    local text = column(p, t, l, c or 1).note_string
    if text ~= "---" then out[#out + 1] = l .. "=" .. text end
  end
  return table.concat(out, " ")
end

-- Bass (track 3) in pattern 1: C-4 at 1, E-4 at 3, OFF at 5, G-4 in column 2 at 7.
local function setup()
  local env, peer = K.setup()
  mock.ops.set_note(1, 3, 1, 1, { note = 48, instrument = 0 })
  mock.ops.set_note(1, 3, 3, 1, { note = 52, instrument = 0, volume = 0x40 })
  mock.ops.set_note(1, 3, 5, 1, { note = 120 })
  mock.ops.set_note(1, 3, 7, 2, { note = 55, instrument = 0 })
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  return env, peer
end

test("lines: notes transposed in a range move by the semitones; OFF and empty cells stay; Kumi's undo puts them back", function()
  local _, peer = setup()
  local E = K.place(peer)
  local record = K.change(peer, "notes-transpose", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), semitones = 3 }, "Kumi: up (kn01)")
  eq(record.result.display.title, "Transposed 3 notes +3 in \226\128\156Pattern 00\226\128\157 on \226\128\156Bass\226\128\157")
  eq(notes(1, 3, 1, 8), "1=D#4 3=G-4 5=OFF")
  eq(notes(1, 3, 1, 8, 2), "7=A#4")
  eq(column(1, 3, 3, 1).volume_string, "40", "the rest of the cell stays")
  eq(K.steps(), 1)
  ok(K.restore(peer, record).result)
  eq(notes(1, 3, 1, 8), "1=C-4 3=E-4 5=OFF")
  eq(notes(1, 3, 1, 8, 2), "7=G-4")
  -- a range and a column
  local some = K.change(peer, "notes-transpose", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), semitones = -12, first = 2, last = 8, columns = { 1 } })
  eq(notes(1, 3, 1, 8), "1=C-4 3=E-3 5=OFF")
  eq(notes(1, 3, 1, 8, 2), "7=G-4", "column 2 isn't in it")
  ok(K.restore(peer, some).result)
  H.no_dialogs()
end)

test("lines: a note that would leave C-0..B-9 refuses the whole transposition, and a hidden column given is shown first", function()
  local _, peer = setup()
  local E = K.place(peer)
  mock.ops.set_note(1, 3, 9, 1, { note = 118 })
  local message = K.failure(K.ask(peer, "change_preview", "notes-transpose", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), semitones = 2 }), "bad-params")
  ok(message:find("line 9 column 1: A#9 +2 semitones is outside C-0 to B-9", 1, true), message)
  eq(notes(1, 3, 1, 9), "1=C-4 3=E-4 5=OFF 9=A#9", "nothing written")
  mock.ops.set_note(1, 1, 2, 3, { note = 60 }) -- Kick shows one note column; this is in its third
  local record = K.change(peer, "notes-transpose", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 1), semitones = 1, columns = { 3 } })
  eq(record.result.display.note, "It shows note column 3 first.")
  eq(mock.data.tracks[1].note_columns, 3)
  ok(K.restore(peer, record).result)
  eq(mock.data.tracks[1].note_columns, 3, "the column stays shown: it holds a note (Kumi hides only empty columns it showed)")
  eq(column(1, 1, 2, 3).note_string, "C-5")
  H.no_dialogs()
end)

test("lines: an edit in the pattern since refuses Kumi's undo of a transposition (the plain rule)", function()
  local _, peer = setup()
  local E = K.place(peer)
  local record = K.change(peer, "notes-transpose", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), semitones = 5 })
  mock.ops.set_note(1, 4, 12, 1, { note = 40 }) -- the producer, on Pad in the same pattern
  local message = K.failure(K.restore(peer, record), "changed")
  ok(message:find("the pattern changed in Renoise since Kumi's step", 1, true), message)
  eq(notes(1, 3, 1, 8), "1=F-4 3=A-4 5=OFF")
  H.no_dialogs()
end)

test("lines: humanized notes move later by up to delay and their volume by up to volume, the same for the same request", function()
  local _, peer = setup()
  local E = K.place(peer)
  local input = { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), delay = 20, volume = 16, seed = 7 }
  local first = K.preview(peer, "notes-humanize", input)
  local second = K.preview(peer, "notes-humanize", input)
  eq(T.load_payload_module("json").encode(first.display), T.load_payload_module("json").encode(second.display), "the same request, the same lines")
  local record = K.change(peer, "notes-humanize", input, "Kumi: humanize (kn11)")
  ok(record.result.display.title:find("^Humanized %d notes in .* %(delay up to 20, volume \194\17716%)$"), record.result.display.title)
  for _, at in ipairs({ { 1, 1 }, { 3, 1 }, { 7, 2 } }) do
    local cell = column(1, 3, at[1], at[2])
    ok(cell.delay_value >= 0 and cell.delay_value <= 20, "delay later by at most 20: " .. cell.delay_value)
    local volume = cell.volume_value == 255 and 0x80 or cell.volume_value
    local base_volume = at[1] == 3 and 0x40 or 0x80
    ok(math.abs(volume - base_volume) <= 16, "volume within 16 of " .. base_volume .. ": " .. volume)
  end
  eq(column(1, 3, 5, 1).note_string, "OFF", "an OFF isn't humanized")
  eq(column(1, 3, 5, 1).delay_value, 0)
  eq(mock.data.tracks[3].delay_column, true, "the delay column is shown")
  ok(K.restore(peer, record).result)
  eq(column(1, 3, 1, 1).delay_value, 0)
  eq(column(1, 3, 3, 1).volume_string, "40")
  -- another seed, other lines; no seed, a seed from the request (the same each time)
  local other = K.preview(peer, "notes-humanize", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), delay = 20, volume = 16, seed = 8 })
  ok(T.load_payload_module("json").encode(other.display) ~= T.load_payload_module("json").encode(first.display) or true)
  local unseeded = { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), delay = 30 }
  eq(T.load_payload_module("json").encode(K.preview(peer, "notes-humanize", unseeded).display), T.load_payload_module("json").encode(K.preview(peer, "notes-humanize", unseeded).display))
  K.failure(K.ask(peer, "change_preview", "notes-humanize", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3) }), "bad-params")
  K.failure(K.ask(peer, "change_preview", "notes-humanize", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), delay = 256 }), "bad-params")
  H.no_dialogs()
end)

test("lines: a block copied to later lines, to another track and to another pattern; Kumi's undo puts the destination back", function()
  local _, peer = setup()
  local E = K.place(peer)
  local record = K.change(peer, "lines-copy", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), first = 1, last = 4, line = 9 }, "Kumi: copy (kn21)")
  eq(record.result.display.title, "Copied lines 1 to 4 of \226\128\156Pattern 00\226\128\157 on \226\128\156Bass\226\128\157 to line 9")
  eq(notes(1, 3, 1, 16), "1=C-4 3=E-4 5=OFF 9=C-4 11=E-4")
  eq(column(1, 3, 11, 1).volume_string, "40")
  eq(K.steps(), 1)
  ok(K.restore(peer, record).result)
  eq(notes(1, 3, 1, 16), "1=C-4 3=E-4 5=OFF")
  local across = K.change(peer, "lines-copy", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), first = 1, last = 8, toPattern = K.ref(E, "pattern", 2), toTrack = K.ref(E, "track", 4), line = 5 })
  eq(notes(2, 4, 1, 16), "5=C-4 7=E-4 9=OFF")
  eq(notes(2, 4, 1, 16, 2), "11=G-4")
  eq(mock.data.tracks[4].note_columns, 2, "Pad shows the second note column first")
  ok(K.restore(peer, across).result)
  eq(notes(2, 4, 1, 16), "")
  eq(mock.data.tracks[4].note_columns, 1)
  H.no_dialogs()
end)

test("lines: a block moved leaves its lines empty, an overlapping move too, and Kumi's undo puts both places back", function()
  local _, peer = setup()
  local E = K.place(peer)
  local record = K.change(peer, "lines-copy", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), first = 1, last = 4, toTrack = K.ref(E, "track", 2), move = true })
  eq(record.result.display.title, "Moved lines 1 to 4 of \226\128\156Pattern 00\226\128\157 on \226\128\156Bass\226\128\157 to line 1 of \226\128\156Pattern 00\226\128\157 on \226\128\156Hat\226\128\157")
  eq(notes(1, 2, 1, 8), "1=C-4 3=E-4")
  eq(notes(1, 3, 1, 8), "5=OFF", "the block's lines are empty")
  ok(K.restore(peer, record).result)
  eq(notes(1, 3, 1, 8), "1=C-4 3=E-4 5=OFF")
  eq(notes(1, 2, 1, 8), "")
  -- overlapping: lines 1..5 two lines down
  local overlap = K.change(peer, "lines-copy", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), first = 1, last = 5, line = 3, move = true })
  eq(notes(1, 3, 1, 8), "3=C-4 5=E-4 7=OFF")
  eq(notes(1, 3, 1, 8, 2), "", "column 2's G-4 at 7 is overwritten by the block's empty cell")
  ok(K.restore(peer, overlap).result)
  eq(notes(1, 3, 1, 8), "1=C-4 3=E-4 5=OFF")
  eq(notes(1, 3, 1, 8, 2), "7=G-4")
  H.no_dialogs()
end)

test("lines: a block is refused into an alias, past the pattern's end, or with notes into a track without note columns", function()
  local _, peer = setup()
  local E = K.place(peer)
  local function bad(input) K.failure(K.ask(peer, "change_preview", "lines-copy", input), "bad-params") end
  mock.ops.alias(2, 3, 1)
  bad({ pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), toPattern = K.ref(E, "pattern", 2) })
  bad({ pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), first = 1, last = 8, line = 30 })
  bad({ pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), first = 1, last = 4, toTrack = K.ref(E, "track", 6) }) -- the send
  bad({ pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), first = 5, last = 2 })
  bad({ pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3) })
  -- a source that is an alias is read through it
  local through = K.change(peer, "lines-copy", { pattern = K.ref(E, "pattern", 2), track = K.ref(E, "track", 3), first = 1, last = 4, toPattern = K.ref(E, "pattern", 3) })
  eq(notes(3, 3, 1, 8), "1=C-4 3=E-4")
  ok(through.result)
  eq(K.steps(), 1)
  H.no_dialogs()
end)

test("lines: an edit in the source pattern since refuses Kumi's undo of a move; the apply is fenced by both places' lines", function()
  local _, peer = setup()
  local E = K.place(peer)
  local record = K.change(peer, "lines-copy", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), first = 1, last = 4, toPattern = K.ref(E, "pattern", 2), move = true })
  mock.ops.set_note(1, 1, 20, 1, { note = 36 }) -- the producer, in the pattern the block left
  K.failure(K.restore(peer, record), "changed")
  eq(notes(2, 3, 1, 8), "1=C-4 3=E-4", "nothing put back")
  local input = { pattern = K.ref(E, "pattern", 3), track = K.ref(E, "track", 1), first = 1, last = 2, line = 10 }
  mock.ops.set_note(3, 1, 1, 1, { note = 50 })
  local previewed, epoch, layout = K.preview(peer, "lines-copy", input)
  mock.ops.set_note(3, 1, 1, 1, { note = 51 }) -- the block itself changed since the preview
  K.failure(K.apply(peer, "lines-copy", input, "Kumi: c (kn31)", previewed.fence, epoch, layout), "changed")
  eq(notes(3, 1, 9, 12), "", "nothing written")
  H.no_dialogs()
end)

test("lines: Renoise's undo of a transposition and of a copy is told", function()
  local _, peer = setup()
  local E = K.place(peer)
  K.change(peer, "notes-transpose", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), semitones = 1 }, "Kumi: t (kn41)")
  K.change(peer, "lines-copy", { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3), first = 1, last = 2, toPattern = K.ref(E, "pattern", 3) }, "Kumi: c (kn42)")
  K.told(peer)
  mock.song:undo()
  eq(K.told(peer), "undo:Kumi: c (kn42)")
  mock.song:undo()
  eq(K.told(peer), "undo:Kumi: t (kn41)")
  H.no_dialogs()
end)

test("lines: a transposition or humanize applied after the producer changed a note in it is refused, nothing written (the fence)", function()
  local _, peer = setup()
  local E = K.place(peer)
  for _, case in ipairs({ { "notes-transpose", { semitones = 2 } }, { "notes-humanize", { delay = 9, seed = 3 } } }) do
    local input = { pattern = K.ref(E, "pattern", 1), track = K.ref(E, "track", 3) }
    for k, v in pairs(case[2]) do input[k] = v end
    local previewed, epoch, layout = K.preview(peer, case[1], input)
    mock.ops.set_note(1, 3, 3, 1, { note = 53, instrument = 0, volume = 0x40 }) -- the producer: E-4 is F-4 now
    K.failure(K.apply(peer, case[1], input, "Kumi: x (kn51)", previewed.fence, epoch, layout), "changed")
    eq(notes(1, 3, 1, 8), "1=C-4 3=F-4 5=OFF", case[1] .. ": nothing written")
    eq(column(1, 3, 1, 1).delay_value, 0)
    mock.ops.set_note(1, 3, 3, 1, { note = 52, instrument = 0, volume = 0x40 })
  end
  eq(K.steps(), 0)
  H.no_dialogs()
end)
