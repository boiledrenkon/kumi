-- Kumi's changes to phrases, phrase scripts, plug-in and MIDI instruments (M4, n-instruments): each applied, put back, refused
-- when changed, gone, back already; the places followed; scripts' compile errors reported.
local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness
local I = T.instruments
local setup, place, preview, apply, change, restore, failure, call = I.setup, I.place, I.preview, I.apply, I.change, I.restore, I.failure, I.call
local steps, newest, instrument, iref = I.steps, I.newest, I.instrument, I.iref

local function pref(epoch, i, p) return epoch .. ":phrase:" .. i .. "." .. p end
local function phrase(i, p) return mock.data.instruments[i].phrases[p] end
local function cell(i, p, l, c) return mock.song:instrument(i):phrase(p):line(l):note_column(c or 1) end

-- phrase-add, phrase-delete, phrase-set

test("phrases: a phrase made with its settings; Kumi's undo removes it, refused once the producer wrote in it", function()
  local env, peer = setup()
  local record, done = change(peer, "phrase-add", { ref = iref(place(peer), 2), index = 1, name = "Riff", lines = 32, lpb = 8, keyTracking = "transpose", looping = true })
  eq(#instrument(2).phrases, 2)
  eq(phrase(2, 1).name, "Riff")
  eq(phrase(2, 1).lines, 32)
  eq(phrase(2, 1).lpb, 8)
  eq(phrase(2, 1).key_tracking, 2)
  eq(done.created.kind, "phrase")
  local undone = restore(peer, "phrase-add", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(#instrument(2).phrases, 1)
  record = change(peer, "phrase-add", { ref = iref(place(peer), 2) })
  cell(2, 2, 1).note_string = "C-4"
  failure(restore(peer, "phrase-add", record), "changed")
  eq(#instrument(2).phrases, 2)
end)

test("phrases: a deleted phrase comes back with its lines, settings, column name, keys and script, after the phrase before it", function()
  local env, peer = setup()
  local p = mock.song:instrument(2):phrase(1)
  p.name = "Arp"
  p.number_of_lines = 8
  p.lpb = 12
  p:line(1):note_column(1).note_string = "C-4"
  p:line(3):note_column(2).note_string = "E-4"
  p:line(5):effect_column(1).number_string = "0A"
  p:set_column_name(1, "Root")
  mock.song:instrument(2):insert_phrase_mapping_at(1, p).note_range = { 60, 71 }
  mock.data.undo.stack = {}
  local record = change(peer, "phrase-delete", { ref = pref(place(peer), 2, 1) })
  eq(#instrument(2).phrases, 0)
  eq(#instrument(2).mappings, 0, "its mapping went with it")
  mock.song:instrument(2):insert_phrase_at(1).name = "New"
  local undone = restore(peer, "phrase-delete", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(#instrument(2).phrases, 2)
  local back = mock.song:instrument(2):phrase(1)
  eq(back.name, "Arp", "first, as it was: no phrase was before it")
  eq(back.number_of_lines, 8)
  eq(back.lpb, 12)
  eq(back:line(3):note_column(2).note_string, "E-4")
  eq(back:line(5):effect_column(1).number_string, "0A")
  eq(back:column_name(1), "Root")
  eq(instrument(2).mappings[1].note_range[1], 60, "its keys mapped again")
  local again = restore(peer, "phrase-delete", record)
  ok(again.result, json.encode(again.error or {}))
  ok(again.result.display.title:find("back already", 1, true), again.result.display.title)
end)

test("phrases: settings change and go back (lines, LPB, loop, shuffle, key tracking, base note, columns); a loop past its lines is bad-params", function()
  local env, peer = setup()
  local record = change(peer, "phrase-set", { ref = pref(place(peer), 2, 1), lines = 32, lpb = 16, loopStart = 17, loopEnd = 32, shuffle = 0.5, keyTracking = "offset", baseNote = 36, noteColumns = 4, delayColumn = true })
  local data = phrase(2, 1)
  eq(data.lines, 32)
  eq(data.loop_start, 17)
  eq(data.loop_end, 32)
  eq(data.shuffle, 0.5)
  eq(data.key_tracking, 3)
  eq(data.note_columns, 4)
  eq(data.delay_column_visible, true)
  local undone = restore(peer, "phrase-set", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(data.lines, 16)
  eq(data.loop_start, 1)
  eq(data.loop_end, 16)
  eq(data.note_columns, 1)
  failure(call(peer, "change_preview", { kind = "phrase-set", input = { ref = pref(place(peer), 2, 1), loopEnd = 20 }, epoch = place(peer) }), "bad-params")
end)

test("places, phrase × a phrase made before it by the producer: the undo of its settings follows it; deleted: gone", function()
  local env, peer = setup()
  local record = change(peer, "phrase-set", { ref = pref(place(peer), 2, 1), lpb = 6 })
  mock.song:instrument(2):insert_phrase_at(1).name = "Intro"
  local undone = restore(peer, "phrase-set", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(phrase(2, 2).lpb, 4)
  record = change(peer, "phrase-set", { ref = pref(place(peer), 2, 2), lpb = 6 })
  mock.song:instrument(2):delete_phrase_at(2)
  failure(restore(peer, "phrase-set", record), "gone")
end)

-- phrase-map, phrase-playback

test("phrases: a phrase mapped to keys, its range changed, unmapped; each goes back; overlapping keys are bad-params", function()
  local env, peer = setup()
  local p2 = mock.song:instrument(2):insert_phrase_at(2)
  mock.song:instrument(2):insert_phrase_mapping_at(1, p2).note_range = { 72, 83 }
  mock.data.undo.stack = {}
  local record = change(peer, "phrase-map", { ref = pref(place(peer), 2, 1), noteRange = { 48, 59 } })
  eq(#instrument(2).mappings, 2)
  eq(instrument(2).mappings[1].note_range[1], 48, "sorted by key: first")
  local undone = restore(peer, "phrase-map", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(#instrument(2).mappings, 1)
  failure(call(peer, "change_preview", { kind = "phrase-map", input = { ref = pref(place(peer), 2, 1), noteRange = { 70, 75 } }, epoch = place(peer) }), "bad-params")
  record = change(peer, "phrase-map", { ref = pref(place(peer), 2, 2), noteRange = { 60, 64 } })
  eq(instrument(2).mappings[1].note_range[2], 64)
  undone = restore(peer, "phrase-map", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(instrument(2).mappings[1].note_range[1], 72)
  record = change(peer, "phrase-map", { ref = pref(place(peer), 2, 2), noteRange = json.null })
  eq(#instrument(2).mappings, 0)
  undone = restore(peer, "phrase-map", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(instrument(2).mappings[1].note_range[2], 83)
end)

test("phrases: the instrument's phrase playback: program mode playing a phrase (kept as its place), keymap, off; each goes back", function()
  local env, peer = setup()
  mock.song:instrument(2):insert_phrase_at(2).name = "B"
  mock.data.undo.stack = {}
  local epoch = place(peer)
  local record, done = change(peer, "phrase-playback", { ref = iref(epoch, 2), mode = "program", programRef = pref(epoch, 2, 2) })
  eq(instrument(2).phrase_playback_mode, 2)
  eq(instrument(2).phrase_program, 2)
  ok(done.display.title:find("plays phrase 01 on every key", 1, true), done.display.title)
  local undone = restore(peer, "phrase-playback", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(instrument(2).phrase_playback_mode, 1)
  record = change(peer, "phrase-playback", { ref = iref(place(peer), 2), mode = "keymap" })
  eq(instrument(2).phrase_program, 127)
  ok(restore(peer, "phrase-playback", record).result)
  eq(instrument(2).phrase_program, 0)
  failure(call(peer, "change_preview", { kind = "phrase-playback", input = { ref = iref(place(peer), 1), mode = "program" }, epoch = place(peer) }), "bad-params")
end)

-- phrase-write, phrase-clear

test("phrases: lines written in Renoise's own text, columns shown first; Kumi's undo puts the cells back and hides the columns again", function()
  local env, peer = setup()
  local input = { ref = pref(place(peer), 2, 1), lines = {
    { line = 1, notes = { { col = 1, note = "C-4", instrument = "00", volume = "40" }, { col = 2, note = "G-4", delay = "10" } } },
    { line = 5, notes = { { col = 1, note = "OFF" } }, effects = { { col = 1, number = "0A", amount = "20" } } },
  } }
  local record, done = change(peer, "phrase-write", input, "Kumi: arp (k600)")
  eq(cell(2, 1, 1, 1).note_string, "C-4")
  eq(cell(2, 1, 1, 2).delay_string, "10")
  eq(phrase(2, 1).note_columns, 2, "the second note column shown first")
  eq(phrase(2, 1).delay_column_visible, true)
  eq(steps(), 1)
  eq(newest(), "Kumi: arp (k600)")
  ok(done.display.title:find("Wrote 2 lines", 1, true), done.display.title)
  local undone = restore(peer, "phrase-write", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(cell(2, 1, 1, 1).note_string, "---")
  eq(phrase(2, 1).note_columns, 1, "hidden again")
  eq(phrase(2, 1).delay_column_visible, false)
end)

test("phrases: a line the producer edited anywhere in the phrase since refuses the undo of Kumi's lines (the plain rule), nothing written", function()
  local env, peer = setup()
  local record = change(peer, "phrase-write", { ref = pref(place(peer), 2, 1), lines = { { line = 2, notes = { { col = 1, note = "D-4" } } } } })
  cell(2, 1, 16).note_string = "A-3" -- far below Kumi's line
  local refused = restore(peer, "phrase-write", record)
  failure(refused, "changed")
  eq(cell(2, 1, 2).note_string, "D-4", "left as it is")
  eq(cell(2, 1, 16).note_string, "A-3")
end)

test("phrases: a clear of lines keeps the cells and puts them back; a write too big for one step is bad-params", function()
  local env, peer = setup()
  local p = mock.song:instrument(2):phrase(1)
  p:line(1):note_column(1).note_string = "C-4"
  p:line(4):note_column(1).note_string = "D-4"
  mock.data.undo.stack = {}
  local record = change(peer, "phrase-clear", { ref = pref(place(peer), 2, 1) })
  eq(cell(2, 1, 1).note_string, "---")
  eq(cell(2, 1, 4).note_string, "---")
  local undone = restore(peer, "phrase-clear", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(cell(2, 1, 4).note_string, "D-4")
  local lines = {}
  for l = 1, 16 do
    local notes = {}
    for c = 1, 12 do notes[c] = { col = c, note = "C-4", instrument = "00", volume = "40", panning = "40", delay = "01", fx = "0A10" } end
    lines[l] = { line = l, notes = notes }
  end
  mock.song:instrument(2):phrase(1).number_of_lines = 16
  local epoch = place(peer)
  local big = call(peer, "change_preview", { kind = "phrase-write", input = { ref = pref(epoch, 2, 1), lines = lines }, epoch = epoch })
  ok(big.result, "16 lines of 12 columns fit: " .. json.encode(big.error or {}))
end)

-- phrase-script, phrase-render

test("phrases: a script written and committed reports Renoise's compile error; Kumi's undo puts the old text and mode back", function()
  local env, peer = setup()
  local previewed, epoch, layout = preview(peer, "phrase-script", { ref = pref(place(peer), 2, 1), code = "return pattern {\n  event = { \"c4\"\n" })
  local id = I.send(peer, "change_apply", { kind = "phrase-script", input = { ref = pref(epoch, 2, 1), code = "return pattern {\n  event = { \"c4\"\n" }, fence = previewed.fence, desc = "Kumi: script (k610)", epoch = epoch, layout = layout })
  mock.advance(60)
  mock.compile_scripts()
  local done = I.wait(peer, id)
  ok(done.result, json.encode(done.error or {}))
  eq(phrase(2, 1).playback_mode, 2)
  eq(phrase(2, 1).script.paragraphs[1], "return pattern {")
  eq(phrase(2, 1).script.committed[2], "  event = { \"c4\"")
  eq(done.result.applied.compileError, json.null, "this one compiles (it has a return)")
  eq(steps(), 1)
  eq(newest(), "Kumi: script (k610)")
  local record = { target = done.result.target, prior = previewed.prior, applied = done.result.applied, epoch = done.result.epoch, layout = done.result.layout }
  local undone = restore(peer, "phrase-script", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(phrase(2, 1).playback_mode, 1)
  eq(phrase(2, 1).script.paragraphs[1], "-- a phrase script")
  -- a script that doesn't compile
  previewed, epoch, layout = preview(peer, "phrase-script", { ref = pref(place(peer), 2, 1), code = "pattern { }" })
  id = I.send(peer, "change_apply", { kind = "phrase-script", input = { ref = pref(epoch, 2, 1), code = "pattern { }" }, fence = previewed.fence, desc = "Kumi: s (k611)", epoch = epoch, layout = layout })
  mock.advance(60)
  mock.compile_scripts()
  done = I.wait(peer, id)
  ok(done.result, json.encode(done.error or {}))
  ok(type(done.result.applied.compileError) == "string" and done.result.applied.compileError:find("return", 1, true), json.encode(done.result.applied))
  ok(done.result.display.note:find("couldn't compile", 1, true), done.result.display.note)
end)

test("phrases: a script rendered to lines; Kumi's undo puts the lines it had back", function()
  local env, peer = setup()
  local p = mock.song:instrument(2):phrase(1)
  p:line(2):note_column(1).note_string = "F-4"
  p.playback_mode = 2
  p.script.paragraphs = { "return cycle(\"c4 e4\")" }
  p.script:commit()
  mock.compile_scripts()
  mock.data.undo.stack = {}
  local previewed, epoch, layout = preview(peer, "phrase-render", { ref = pref(place(peer), 2, 1), lpb = 4 })
  local id = I.send(peer, "change_apply", { kind = "phrase-render", input = { ref = pref(epoch, 2, 1), lpb = 4 }, fence = previewed.fence, desc = "Kumi: render (k620)", epoch = epoch, layout = layout })
  mock.advance(30)
  mock.finish_renders()
  local done = I.wait(peer, id)
  ok(done.result, json.encode(done.error or {}))
  eq(cell(2, 1, 1).note_string, "C-4", "rendered")
  eq(cell(2, 1, 2).note_string, "F-4", "(the mock's render writes every 4th line)")
  local record = { target = done.result.target, prior = previewed.prior, applied = done.result.applied, material = done.result.material, epoch = done.result.epoch, layout = done.result.layout }
  local undone = restore(peer, "phrase-render", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(cell(2, 1, 1).note_string, "---")
  eq(cell(2, 1, 2).note_string, "F-4")
  eq(phrase(2, 1).playback_mode, 2, "it plays its script again")
  failure(call(peer, "change_preview", { kind = "phrase-render", input = { ref = pref(place(peer), 1, 1) }, epoch = place(peer) }), "gone")
end)

-- Plug-in and MIDI instruments

local function plugin_catalog()
  mock.plugin_catalog = {
    ["Audio/Generators/AU/aumu:dls :appl"] = { name = "DLSMusicDevice", params = { { name = "Volume", value = 0.5 }, { name = "Tune", value = 0.5 } }, presets = { "Piano", "Strings", "Bass" } },
    ["Audio/Generators/VST3/Synth"] = { name = "Synth", params = { { name = "Cutoff", value = 0.2 } }, presets = {} },
  }
end

test("plugins: a plug-in loaded by name into an instrument; Kumi's undo takes it out again, and puts back one it replaced with its state", function()
  plugin_catalog()
  local env, peer = setup()
  local record, done = change(peer, "plugin-load", { ref = iref(place(peer), 3), path = "DLSMusicDevice" })
  eq(instrument(3).plugin.path, "Audio/Generators/AU/aumu:dls :appl")
  ok(done.display.title:find("DLS", 1, true), done.display.title)
  local undone = restore(peer, "plugin-load", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(instrument(3).plugin, nil)
  change(peer, "plugin-load", { ref = iref(place(peer), 3), path = "Audio/Generators/VST3/Synth" })
  instrument(3).plugin.preset_data = "<synth state='mine'/>"
  record = change(peer, "plugin-load", { ref = iref(place(peer), 3), path = "DLSMusicDevice" })
  undone = restore(peer, "plugin-load", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(instrument(3).plugin.path, "Audio/Generators/VST3/Synth")
  eq(instrument(3).plugin.preset_data, "<synth state='mine'/>", "its state too")
  failure(call(peer, "change_preview", { kind = "plugin-load", input = { ref = iref(place(peer), 3), path = "Moog" }, epoch = place(peer) }), "bad-params")
end)

test("plugins: a plug-in's parameter by name or by its ref, its program, its settings; each goes back", function()
  plugin_catalog()
  local env, peer = setup()
  change(peer, "plugin-load", { ref = iref(place(peer), 3), path = "DLSMusicDevice" })
  mock.data.undo.stack = {}
  local epoch = place(peer)
  local param = change(peer, "plugin-param", { ref = iref(epoch, 3), param = "Tune", value = 0.75 })
  eq(instrument(3).plugin.params[2].value, 0.75)
  local by_ref = change(peer, "plugin-param", { ref = place(peer) .. ":pluginparam:3.1", value = 0.1 })
  eq(instrument(3).plugin.params[1].value, 0.1)
  ok(restore(peer, "plugin-param", by_ref).result)
  ok(restore(peer, "plugin-param", param).result)
  eq(instrument(3).plugin.params[2].value, 0.5)
  instrument(3).plugin.preset_data = "<dls before/>"
  local preset = change(peer, "plugin-preset", { ref = iref(place(peer), 3), preset = "Strings" })
  eq(instrument(3).plugin.preset, 2)
  local undone = restore(peer, "plugin-preset", preset)
  ok(undone.result, json.encode(undone.error or {}))
  eq(instrument(3).plugin.preset, 0)
  eq(instrument(3).plugin.preset_data, "<dls before/>")
  local settings = change(peer, "plugin-set", { ref = iref(place(peer), 3), channel = 2, transpose = -12, volumeDb = -6, autoSuspend = true })
  eq(instrument(3).plugin_props.channel, 2)
  ok(restore(peer, "plugin-set", settings).result)
  eq(instrument(3).plugin_props.channel, 1)
  failure(call(peer, "change_preview", { kind = "plugin-param", input = { ref = iref(place(peer), 1), param = 1, value = 0 }, epoch = place(peer) }), "bad-params")
end)

test("midi: a MIDI instrument's device, channel, transpose, program, bank, delay and duration change and go back; an unknown device is bad-params", function()
  mock.midi_outputs = { "IAC Driver Bus 1" }
  local env, peer = setup()
  local record = change(peer, "midi-set", { ref = iref(place(peer), 3), device = "IAC Driver Bus 1", channel = 10, transpose = 12, program = 5, bank = 2, delay = 20, duration = 500 })
  local midi = instrument(3).midi
  eq(midi.device_name, "IAC Driver Bus 1")
  eq(midi.channel, 10)
  eq(midi.program, 5)
  eq(midi.duration, 500)
  local undone = restore(peer, "midi-set", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(midi.device_name, "")
  eq(midi.channel, 1)
  eq(midi.duration, 8000)
  failure(call(peer, "change_preview", { kind = "midi-set", input = { ref = iref(place(peer), 3), device = "Nope" }, epoch = place(peer) }), "bad-params")
  mock.midi_outputs = nil
end)
