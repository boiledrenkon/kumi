-- M4 (n-song): the actions Kumi asks of Renoise that change nothing of the song: auditioning a note (ended after its time),
-- selecting or showing things, saving the song to its own file.
local T = ...
local mock = T.mock
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness
local K = assert(loadfile(T.tests_dir .. "change_kit.lua"))(T)

local function triggers()
  local out = {}
  for _, t in ipairs(mock.triggers) do out[#out + 1] = table.concat({ t[1], t[2], t[3], t[4] }, " ") end
  return table.concat(out, " | ")
end

test("actions: a note auditioned plays out loud and is ended after its time; a chord, a sample, and stop", function()
  local _, peer = K.setup(nil, { instruments = 2, instrument_specs = { { name = "Piano" }, { name = "Kit", samples = 2 } } })
  local E = K.place(peer)
  local answer = K.call(peer, "audition_note", { instrument = K.ref(E, "instrument", 1), track = K.ref(E, "track", 3), note = "E-4", durationMs = 300, epoch = E })
  ok(answer.result, T.load_payload_module("json").encode(answer.error or {}))
  eq(answer.result.display.title, "Auditioning E-4 on \226\128\156Piano\226\128\157 (\226\128\156Bass\226\128\157, 300 ms)")
  eq(answer.result.auditioning.notes[1], "E-4")
  eq(triggers(), "on 1 3 52")
  eq(K.steps(), 0, "no undo step")
  mock.advance(400)
  H.tick(2)
  eq(triggers(), "on 1 3 52 | off 1 3 52", "ended after its time")
  local chord = K.call(peer, "audition_note", { instrument = K.ref(E, "instrument", 1), notes = { "C-4", 52, "G-4" }, volume = 0.5 }).result
  eq(chord.auditioning.trackRef, K.ref(E, "track", 1), "the selected track by default")
  eq(mock.triggers[#mock.triggers][4], "48,52,55")
  -- a new audition on the same instrument and track ends the one playing first
  K.call(peer, "audition_note", { instrument = K.ref(E, "instrument", 1), note = 60 })
  eq(mock.triggers[#mock.triggers - 1][1], "off")
  local sample = K.call(peer, "audition_note", { instrument = K.ref(E, "instrument", 2), sample = K.ref(E, "sample", 2, 2), note = "C-5" }).result
  eq(sample.auditioning.sampleRef, K.ref(E, "sample", 2, 2))
  eq(mock.triggers[#mock.triggers][1], "sample on")
  local stopped = K.call(peer, "audition_note", { stop = true }).result
  eq(stopped.stopped, 2)
  eq(mock.triggers[#mock.triggers][1], "sample off")
  H.no_dialogs()
end)

test("actions: an audition's note-off reaches the instrument where it is now, and everything stops when that can't be told (#361 N-S14)", function()
  local _, peer = K.setup(nil, { instruments = 2, instrument_specs = { { name = "Piano" }, { name = "Pad" } } })
  local E = K.place(peer)
  ok(K.call(peer, "audition_note", { instrument = K.ref(E, "instrument", 2), track = K.ref(E, "track", 3), note = "C-4", durationMs = 300 }).result)
  mock.ops.insert_instrument_at(1, { name = "Organ" }) -- made while Pad plays: Pad is instrument 3 now
  H.tick(2)
  mock.advance(400)
  H.tick(2)
  eq(mock.triggers[#mock.triggers][1] .. " " .. mock.triggers[#mock.triggers][2], "off 3", "the note-off goes to Pad, where it is now")
  -- an audition whose instrument can't be followed: every note stops
  ok(K.call(peer, "audition_note", { instrument = K.ref(K.place(peer), "instrument", 3), note = "C-4", durationMs = 300 }).result)
  mock.fire(mock.tool.app_new_document_observable) -- the tool watches the song anew: what moved can't be told
  mock.advance(400)
  H.tick(2)
  eq(mock.triggers[#mock.triggers][1], "panic", "Renoise's panic: no voice hangs on")
  H.no_dialogs()
end)

test("actions: an audition asks nothing Renoise can't play, and waits for nothing while Renoise is busy", function()
  local _, peer = K.setup()
  local E = K.place(peer)
  local function refused(params, code) K.failure(K.call(peer, "audition_note", params), code) end
  refused({ instrument = K.ref(E, "instrument", 1), note = "H-4" }, "bad-params")
  refused({ instrument = K.ref(E, "instrument", 1), note = 120 }, "bad-params")
  refused({ instrument = K.ref(E, "instrument", 1), durationMs = 20000 }, "bad-params")
  refused({ instrument = K.ref(E, "instrument", 9) }, "gone")
  refused({ instrument = K.ref(E, "track", 1) }, "bad-params")
  refused({ instrument = K.ref(E, "instrument", 1), sample = K.ref(E, "sample", 1, 1) }, "gone")
  refused({ instrument = K.ref("0", "instrument", 1), epoch = "0" }, "stale")
  mock.data.song_fields.rendering = true
  refused({ instrument = K.ref(E, "instrument", 1) }, "busy")
  mock.data.song_fields.rendering = false
  eq(#mock.triggers, 0, "nothing played")
  H.no_dialogs()
end)

test("actions: select shows a track, its device, an instrument, a slot that plays a pattern, a line and the frames", function()
  local _, peer = K.setup(nil, { instruments = 2, instrument_specs = { {}, { name = "Kit", samples = 2 } } })
  local E = K.place(peer)
  local answer = K.call(peer, "select", { device = K.ref(E, "device", 3, 1), sample = K.ref(E, "sample", 2, 2), pattern = K.ref(E, "pattern", 3),
    line = 8, frame = "mixer", lower = "dsp", matrix = true })
  ok(answer.result, T.load_payload_module("json").encode(answer.error or {}))
  local fields = mock.data.song_fields
  eq(fields.selected_track_index, 3)
  eq(fields.selected_track_device_index, 1)
  eq(fields.selected_instrument_index, 2)
  eq(fields.selected_sample_index, 2)
  eq(fields.selected_sequence_index, 4, "the first slot that plays pattern 3")
  eq(fields.selected_pattern_index, 1, "the pattern itself isn't selected (Renoise would make the slot play it)")
  eq(fields.selected_line_index, 9, "Renoise's line 08 is its ninth")
  eq(mock.window.active_middle_frame, 2)
  eq(mock.window.lower_frame_is_visible, true)
  eq(mock.window.pattern_matrix_is_visible, true)
  eq(answer.result.display.title, "Selected \226\128\156Bass\226\128\157, the instrument \226\128\156Kit\226\128\157, slot 4, line 08, the mixer")
  eq(answer.result.selected.sequenceRef, K.ref(E, "seq", 4))
  eq(mock.data.sequence[1], 1, "no slot's pattern changed")
  eq(K.steps(), 0)
  local function refused(params, code) K.failure(K.call(peer, "select", params), code) end
  refused({}, "bad-params")
  refused({ frame = "arranger" }, "bad-params")
  refused({ pattern = K.ref(E, "pattern", 1), sequence = K.ref(E, "seq", 1) }, "bad-params")
  refused({ track = K.ref(E, "track", 1), device = K.ref(E, "device", 3, 1) }, "bad-params")
  refused({ sequence = K.ref(E, "seq", 2), line = 32 }, "bad-params")
  refused({ line = -1 }, "bad-params")
  refused({ track = K.ref(E, "track", 9) }, "gone")
  H.no_dialogs()
end)

test("actions: saving into a folder named outside ASCII leaves no probe file there (#361 N-N9)", function()
  local env, peer = K.setup()
  local dir = T.temp_dir("save") .. "/S\195\182ngs"
  os.execute("mkdir -p '" .. dir .. "'")
  mock.data.song_fields.file_name = dir .. "/mine.xrns"
  local saved = K.call(peer, "project_save", {}).result
  ok(saved, "saved")
  eq(io.exists(dir .. "/.kumi-save-check"), false, "no probe file written there")
end)

test("actions: project_save refuses a song whose own folder is gone, before Renoise stops at its error (#361 N-S15)", function()
  local env, peer = K.setup()
  local dir = T.temp_dir("save-gone")
  mock.data.song_fields.file_name = dir .. "/moved/mine.xrns" -- its folder renamed or its drive gone since it was opened
  local saves = #(mock.saves or {})
  local refused = K.call(peer, "project_save", {})
  K.failure(refused, "bad-params")
  ok(refused.error.message:find("give path", 1, true), refused.error.message)
  eq(#(mock.saves or {}), saves, "nothing saved")
end)

test("actions: project_save writes the song's own file, or saves it as a new .xrns; an unsaved song needs the path", function()
  local env, peer = K.setup()
  local dir = T.temp_dir("save")
  mock.data.song_fields.file_name = dir .. "/mine.xrns"
  local saved = K.call(peer, "project_save", {}).result
  eq(saved.saved, dir .. "/mine.xrns")
  eq(saved.dirty, false)
  eq(saved.display.title, "Saved the song")
  eq(mock.saves[#mock.saves], dir .. "/mine.xrns")
  local same = K.call(peer, "project_save", { path = dir .. "/mine.xrns" }).result
  eq(same.display.title, "Saved the song", "its own path is a plain save")
  K.failure(K.call(peer, "project_save", { version = true }), "bad-params")
  -- saved as another file: the song is that file from then on
  local other = K.call(peer, "project_save", { path = dir .. "/other.xrns" }).result
  eq(other.saved, dir .. "/other.xrns")
  eq(other.display.note, "The song is that file from now on (it was " .. dir .. "/mine.xrns).")
  eq(mock.data.song_fields.file_name, dir .. "/other.xrns")
  -- an unsaved song
  mock.data.song_fields.file_name = ""
  K.failure(K.call(peer, "project_save", {}), "bad-params")
  K.failure(K.call(peer, "project_save", { path = "relative.xrns" }), "bad-params")
  K.failure(K.call(peer, "project_save", { path = dir .. "/song.txt" }), "bad-params")
  K.failure(K.call(peer, "project_save", { path = dir .. "/nowhere/song.xrns" }), "bad-params")
  K.failure(K.call(peer, "project_save", { path = dir .. "/mine.xrns" }), "bad-params") -- there already: never written over
  local new = K.call(peer, "project_save", { path = dir .. "/new.xrns" }).result
  eq(new.saved, dir .. "/new.xrns")
  eq(new.dirty, false)
  eq(new.display.title, "Saved the song as \226\128\156new.xrns\226\128\157")
  eq(mock.data.song_fields.file_name, dir .. "/new.xrns")
  -- another song open since the request's epoch: stale, nothing saved
  local saves = #mock.saves
  K.failure(K.call(peer, "project_save", { epoch = "0" }), "stale")
  eq(#mock.saves, saves)
  mock.data.song_fields.rendering = true
  K.failure(K.call(peer, "project_save", {}), "busy")
  mock.data.song_fields.rendering = false
  H.no_dialogs()
end)

test("actions: the bridge says it has the actions", function()
  local env = H.setup({ song = K.song_options() })
  local peer = H.connect(env)
  local frames = H.frames(peer)
  ok(frames ~= nil)
  local status = H.request(peer, "status", {})
  ok(status.ok)
  local methods = table.concat(T.load_payload_module("status").METHODS, ",")
  ok(methods:find("audition_note,select,project_save", 1, true), methods)
  H.no_dialogs()
end)

test("actions: status advertises every method the bridge answers (#361 N-N14)", function()
  local methods = {}
  for _, name in ipairs(T.load_payload_module("status").METHODS) do methods[name] = true end
  local f = assert(io.open(T.tests_dir .. "../com.kumi.Bridge.xrnx/payload/bridge.lua", "rb"))
  local text = f:read("*a")
  f:close()
  local missing = {}
  for name in text:gmatch("\nfunction handlers%.([%w_]+)") do if not methods[name] then missing[#missing + 1] = name end end
  for name in text:gmatch("\nhandlers%.([%w_]+) = ") do if not methods[name] then missing[#missing + 1] = name end end
  eq(table.concat(missing, ","), "", "every handler is in status's methods")
end)

test("actions: refs come as Kumi names them (instrumentRef, trackRef, sequenceRef) and are carried through the layout", function()
  local _, peer = K.setup()
  local E, layout = K.place(peer)
  mock.ops.insert_track(1, { name = "Intro" }) -- Bass is the fourth now
  local answer = K.call(peer, "audition_note", { instrumentRef = K.ref(E, "instrument", 1), trackRef = K.ref(E, "track", 3), note = "C-4", epoch = E, layout = layout })
  ok(answer.result, T.load_payload_module("json").encode(answer.error or {}))
  eq(mock.triggers[#mock.triggers][3], 4, "Bass, where it went")
  local selected = K.call(peer, "select", { trackRef = K.ref(E, "track", 3), sequenceRef = K.ref(E, "seq", 2), epoch = E, layout = layout }).result
  eq(mock.data.song_fields.selected_track_index, 4)
  eq(mock.data.song_fields.selected_sequence_index, 2)
  ok(selected.display.title:find("Bass", 1, true))
  H.no_dialogs()
end)
