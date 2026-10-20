-- Kumi's reads of Renoise's instruments (M4, n-instruments): an instrument's settings and what it plays through, a sample's
-- settings and the set and chain it uses, a phrase's settings, script and keys, an instrument's modulation sets and sample FX
-- chains and the parameters of their devices and of a plug-in; and the FOCUS body's keyzones, macros and phrases with their keys.
local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness
local I, F = T.instruments, T.modfx
local call, place, instrument, change = I.call, I.place, I.instrument, I.change

local function items(peer, params)
  local response = call(peer, "read", params)
  ok(response.result, json.encode(response.error or {}))
  return response.result.items
end

local function row(peer, reference)
  local response = call(peer, "read", { ref = reference })
  ok(response.result, reference .. ": " .. json.encode(response.error or {}))
  return response.result.items and response.result.items[1] or response.result
end

local function refused(response, code)
  ok(response.error, "refused")
  eq(response.error.code, code)
end

local function null(value) return value == nil or value == json.null end

test("instrument reads: an instrument's modulation sets with their devices, its FX chains with theirs, and those devices' parameters", function()
  local _, peer, E = F.setup()
  local sets = items(peer, { kind = "modset", parent = E .. ":instrument:1" })
  eq(#sets, 1)
  eq(sets[1].ref, E .. ":modset:1.1")
  eq(sets[1].name, "Amp")
  eq(sets[1].inputs.cutoff, 63.5, "each input in its own range")
  eq(#sets[1].devices, 2)
  local ahdsr, envelope = sets[1].devices[1], sets[1].devices[2]
  eq(ahdsr.ref, E .. ":moddevice:1.1.1")
  eq(ahdsr.type, "AHDSR", "the kind, without the target Renoise puts before it")
  eq(ahdsr.target, "volume")
  eq(ahdsr.operator, "multiply")
  eq(#ahdsr.params, 8)
  eq(ahdsr.params[1].value, 0.1)
  eq(envelope.type, "Envelope")
  eq(envelope.target, "pitch")
  eq(envelope.points, 2)
  eq(row(peer, E .. ":moddevice:1.1.2").type, "Envelope", "a device read again by its ref")
  local chains = items(peer, { kind = "chain", parent = E .. ":instrument:1" })
  eq(#chains, 1)
  eq(chains[1].ref, E .. ":chain:1.1")
  eq(chains[1].name, "Tone")
  eq(chains[1].outputRouting, "Current Track")
  eq(#chains[1].devices, 2, "the mixer first")
  eq(chains[1].devices[1].path, "Audio/Effects/Native/InstrumentVolPan")
  eq(chains[1].devices[2].ref, E .. ":chaindevice:1.1.2")
  eq(chains[1].devices[2].paramCount, 2)
  local params = items(peer, { kind = "param", parent = E .. ":chaindevice:1.1.2" })
  eq(#params, 2)
  eq(params[1].ref, E .. ":chainparam:1.1.2.1")
  eq(params[1].name, "Gain 1")
  eq(params[1].value, 3)
  eq(row(peer, E .. ":chainparam:1.1.2.1").name, "Gain 1")
  eq(row(peer, E .. ":chain:1.1").name, "Tone")
  eq(#items(peer, { kind = "modset", parent = E .. ":instrument:3" }), 1, "a new instrument's one set (lab)")
  eq(#items(peer, { kind = "chain", parent = E .. ":instrument:3" }), 0)
  refused(call(peer, "read", { kind = "param", parent = E .. ":instrument:1" }), "bad-params")
  refused(call(peer, "read", { ref = E .. ":chainparam:1.1.2.9" }), "bad-params")
  refused(call(peer, "read", { kind = "chain", parent = E .. ":instrument:9" }), "bad-params")
  H.no_dialogs()
end)

test("instrument reads: a sample's settings and the set and chain it uses (as refs); an instrument's settings, plug-in and parameters", function()
  local _, peer, E = F.setup()
  instrument(1).samples[2].new_note_action = 3
  instrument(1).samples[2].loop_mode = 2
  instrument(1).samples[2].beat_sync_enabled = true
  instrument(1).samples[2].panning = 0
  local samples = items(peer, { kind = "sample", parent = E .. ":instrument:1" })
  eq(samples[1].modulationSetRef, E .. ":modset:1.1")
  ok(null(samples[1].fxChainRef), "no chain")
  eq(samples[2].fxChainRef, E .. ":chain:1.1")
  ok(null(samples[2].modulationSetRef), "no set")
  eq(samples[2].nna, "continue")
  eq(samples[2].loop, "forward")
  eq(samples[2].loopStart, 1)
  eq(samples[2].beatSync, true)
  eq(samples[2].beatSyncMode, "repitch")
  eq(samples[2].panning, -1)
  eq(samples[2].interpolation, "cubic")
  eq(samples[1].volumeDb, 0)
  eq(samples[1].frames, 1000)
  local drums = row(peer, E .. ":instrument:1")
  eq(drums.kind, "sample")
  eq(drums.overlap, "all")
  eq(drums.phrasePlayback, "program")
  eq(drums.modsetCount, 1)
  eq(drums.chainCount, 1)
  eq(drums.quantize, "none")
  eq(drums.monophonic, false)
  eq(drums.volumeDb, 0)
  mock.plugin_catalog = {
    ["Audio/Generators/AU/aumu:dls :appl"] = { name = "DLSMusicDevice", params = { { name = "Volume", value = 0.5 }, { name = "Tune", value = 0.5 } }, presets = { "Piano", "Strings" } },
  }
  change(peer, "plugin-load", { ref = E .. ":instrument:3", path = "DLSMusicDevice" })
  E = place(peer)
  local pad = row(peer, E .. ":instrument:3")
  eq(pad.kind, "plugin")
  eq(pad.pluginPath, "Audio/Generators/AU/aumu:dls :appl")
  eq(pad.pluginParamCount, 2)
  eq(pad.pluginChannel, 1)
  local params = items(peer, { kind = "param", parent = E .. ":instrument:3" })
  eq(#params, 2)
  eq(params[2].ref, E .. ":pluginparam:3.2")
  eq(params[2].name, "Tune")
  eq(row(peer, E .. ":pluginparam:3.2").name, "Tune")
  H.no_dialogs()
end)

test("instrument reads: a phrase's settings, script and errors, and its keys; the FOCUS body's keyzones, macros and phrases with their keys", function()
  local _, peer = I.setup()
  local E = place(peer)
  local phrase = instrument(2).phrases[1]
  phrase.playback_mode = 2
  phrase.script.compile_error = "syntax error: '}' expected"
  local row1 = row(peer, E .. ":phrase:2.1")
  eq(row1.playback, "script")
  eq(row1.scriptLines, 2)
  eq(row1.compileError, "syntax error: '}' expected")
  ok(null(row1.keyRange), "not mapped")
  phrase.playback_mode = 1
  change(peer, "phrase-map", { ref = E .. ":phrase:2.1", noteRange = { 48, 59 } })
  E = place(peer)
  local mapped = row(peer, E .. ":phrase:2.1")
  eq(mapped.keyRange[1], 48)
  eq(mapped.keyRange[2], 59)
  eq(mapped.baseNoteInKeymap, 48)
  change(peer, "phrase-playback", { ref = E .. ":instrument:2", mode = "program", programRef = E .. ":phrase:2.1" })
  E = place(peer)
  local body = call(peer, "focus_body", { kind = "instrument", ref = E .. ":instrument:2" }).result
  eq(body.kind, "sample")
  eq(body.phrasePlayback, "program")
  eq(body.phraseProgramRef, E .. ":phrase:2.1")
  eq(body.phraseProgram, 1, "its place among the phrases")
  eq(#body.phraseRanges, 1)
  eq(body.phraseRanges[1][1], 48)
  eq(body.phraseRanges[1][2], 59)
  eq(#body.keyzones, 1)
  eq(body.keyzones[1].sample, "Saw")
  eq(body.keyzones[1].noteRange[2], 119)
  instrument(1).samples[2].mute_group = 2
  instrument(1).samples[3].layer = 2
  instrument(1).overlap = 1
  local drums = call(peer, "focus_body", { kind = "instrument", ref = E .. ":instrument:1" }).result
  eq(drums.overlap, "cycle")
  eq(drums.keyzones[2].muteGroup, 2)
  eq(drums.keyzones[3].layer, "note-off")
  ok(drums.keyzones[1].layer == nil and drums.keyzones[1].muteGroup == nil, "only what isn't the default")
  eq(#drums.phraseRanges, 0)
  eq(#drums.macros, 8)
  H.no_dialogs()
end)
