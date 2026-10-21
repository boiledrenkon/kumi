-- Kumi's changes to an instrument's modulation sets and sample FX chains (M4, n-instruments): sets and their devices, chains and
-- their devices, each applied, put back, refused when changed, gone, back already; the places followed.
local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local test, eq, ok = T.test, T.eq, T.ok
local I = T.instruments
local setup, place, preview, apply, change, restore, failure, call = I.setup, I.place, I.preview, I.apply, I.change, I.restore, I.failure, I.call
local steps, instrument, iref, sref = I.steps, I.instrument, I.iref, I.sref

local function catalog()
  mock.device_catalog = {
    ["Audio/Effects/Native/EQ 5"] = { { name = "Gain 1", value = 0, min = -20, max = 20 }, { name = "Gain 2", value = 0, min = -20, max = 20 } },
    ["Audio/Effects/Native/Gainer"] = { { name = "Gain", value = 1, min = 0, max = 4 } },
    ["Audio/Effects/Native/Delay"] = { { name = "Feedback", value = 0.3, min = 0, max = 1 } },
  }
end

-- Drums with two samples, one modulation set (AHDSR on volume, Envelope on pitch) that the first sample uses, and one FX chain
-- (an EQ 5) that the second uses.
local function modfx_song(peer)
  catalog()
  local data = instrument(1)
  local set = mock.set_data({ name = "Amp" })
  set.devices[1] = mock.modulation_device_data("Modulation/AHDSR", 1)
  set.devices[1].params[1].value = 0.1
  set.devices[2] = mock.modulation_device_data("Modulation/Envelope", 3)
  set.devices[2].points = { { time = 1, value = 1, scaling = 0 }, { time = 32, value = 0.2, scaling = 0.5 } }
  data.sets[1] = set
  local chain = mock.chain_data({ name = "Tone" })
  chain.devices[2] = { name = "EQ 5", path = "Audio/Effects/Native/EQ 5", params = { { name = "Gain 1", value = 3, min = -20, max = 20 }, { name = "Gain 2", value = 0, min = -20, max = 20 } }, active = true, preset_data = "<eq gain=3/>" }
  data.chains[1] = chain
  data.samples[1].modulation_set_index = 1
  data.samples[2].device_chain_index = 1
  return place(peer)
end

local function setup_modfx()
  local env, peer = setup()
  -- (the song's instruments are watched as they were loaded: watched anew, with the set and chain)
  mock.fire(mock.tool.app_new_document_observable)
  mock.data.undo.stack = {}
  local epoch = modfx_song(peer)
  mock.fire(mock.tool.app_new_document_observable)
  return env, peer, place(peer)
end

local function modset_ref(epoch, i, m) return epoch .. ":modset:" .. i .. "." .. m end
local function moddevice_ref(epoch, i, m, d) return epoch .. ":moddevice:" .. i .. "." .. m .. "." .. d end
local function chain_ref(epoch, i, c) return epoch .. ":chain:" .. i .. "." .. c end
local function chaindevice_ref(epoch, i, c, d) return epoch .. ":chaindevice:" .. i .. "." .. c .. "." .. d end
-- (for the reads' tests: Drums with its set and chain)
T.modfx = { setup = setup_modfx, catalog = catalog }

-- Modulation sets

test("modfx: a modulation set added at 1 renumbers the samples' sets (Renoise's own); Kumi's undo removes it, refused once a sample uses it", function()
  local env, peer, epoch = setup_modfx()
  local record, done = change(peer, "modset-add", { ref = iref(epoch, 1), index = 1, name = "Filter" })
  eq(#instrument(1).sets, 2)
  eq(instrument(1).sets[1].name, "Filter")
  eq(instrument(1).samples[1].modulation_set_index, 2, "the sample keeps its set")
  eq(done.created.kind, "modset")
  local undone = restore(peer, "modset-add", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(#instrument(1).sets, 1)
  eq(instrument(1).samples[1].modulation_set_index, 1)
  record = change(peer, "modset-add", { ref = iref(place(peer), 1) })
  instrument(1).samples[2].modulation_set_index = 2 -- the producer uses it
  failure(restore(peer, "modset-add", record), "changed")
  eq(#instrument(1).sets, 2)
end)

test("modfx: a deleted modulation set comes back with its devices, parameters and points, after the set before it, and its samples use it again", function()
  local env, peer, epoch = setup_modfx()
  local record, done = change(peer, "modset-delete", { ref = modset_ref(epoch, 1, 1) })
  eq(#instrument(1).sets, 0)
  eq(instrument(1).samples[1].modulation_set_index, 0, "Renoise gave the sample none")
  eq(#done.material.users, 1)
  mock.song:instrument(1):insert_sample_modulation_set_at(1).name = "New"
  local undone = restore(peer, "modset-delete", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(#instrument(1).sets, 2)
  eq(instrument(1).sets[2].name, "New", "first, as it was: no set was before it")
  local back = instrument(1).sets[1]
  eq(back.name, "Amp")
  eq(#back.devices, 2)
  eq(back.devices[1].params[1].value, 0.1)
  eq(back.devices[2].target, 3)
  eq(back.devices[2].points[2].time, 32)
  eq(back.devices[2].points[2].value, 0.2)
  eq(instrument(1).samples[1].modulation_set_index, 1, "the sample uses it again")
  local again = restore(peer, "modset-delete", record)
  ok(again.result, json.encode(again.error or {}))
  ok(again.result.display.title:find("back already", 1, true), again.result.display.title)
end)

test("modfx: a set's name, pitch range, filter and inputs change and go back; an unknown filter is bad-params", function()
  local env, peer, epoch = setup_modfx()
  local record = change(peer, "modset-set", { ref = modset_ref(epoch, 1, 1), name = "Pluck", pitchRange = 24, filterType = "LP Moog", cutoff = 0.4, drive = 0.2 })
  local set = instrument(1).sets[1]
  eq(set.name, "Pluck")
  eq(set.pitch_range, 24)
  eq(set.filter_type, "LP Moog")
  eq(set.inputs.cutoff, 0.4)
  local undone = restore(peer, "modset-set", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(set.name, "Amp")
  eq(set.pitch_range, 12)
  eq(set.filter_type, "None")
  eq(set.inputs.cutoff, 63.5)
  -- (each input in its own range: panning -1 to 1, cutoff 0 to 127)
  failure(call(peer, "change_preview", { kind = "modset-set", input = { ref = modset_ref(place(peer), 1, 1), panning = 1.5 }, epoch = place(peer) }), "bad-params")
  ok(call(peer, "change_preview", { kind = "modset-set", input = { ref = modset_ref(place(peer), 1, 1), cutoff = 100 }, epoch = place(peer) }).result)
  failure(call(peer, "change_preview", { kind = "modset-set", input = { ref = modset_ref(place(peer), 1, 1), filterType = "Wah" }, epoch = place(peer) }), "bad-params")
end)

-- Modulation devices

test("modfx: a modulation device added by name for a target; Kumi's undo removes it, refused after the producer turned it", function()
  local env, peer, epoch = setup_modfx()
  local record, done = change(peer, "moddevice-add", { ref = modset_ref(epoch, 1, 1), path = "LFO", target = "cutoff" })
  local set = instrument(1).sets[1]
  eq(#set.devices, 3)
  eq(set.devices[3].name, "Cutoff LFO", "Renoise names it by its target")
  eq(set.devices[3].target, 4)
  ok(done.created.ref:match(":moddevice:1%.1%.3$"), done.created.ref)
  local undone = restore(peer, "moddevice-add", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(#set.devices, 2)
  record = change(peer, "moddevice-add", { ref = modset_ref(place(peer), 1, 1), path = "Modulation/Fader", target = "volume", index = 1 })
  set.devices[1].params[1].value = 0.9 -- the producer
  failure(restore(peer, "moddevice-add", record), "changed")
  eq(#set.devices, 3)
  failure(call(peer, "change_preview", { kind = "moddevice-add", input = { ref = modset_ref(place(peer), 1, 1), path = "Wobbler", target = "volume" }, epoch = place(peer) }), "bad-params")
  failure(call(peer, "change_preview", { kind = "moddevice-add", input = { ref = modset_ref(place(peer), 1, 1), path = "LFO", target = "speed" }, epoch = place(peer) }), "bad-params")
end)

test("modfx: a deleted modulation device comes back with its parameters and points; a moved one goes back; a move is refused with macro mappings", function()
  local env, peer, epoch = setup_modfx()
  local record = change(peer, "moddevice-delete", { ref = moddevice_ref(epoch, 1, 1, 2) })
  local set = instrument(1).sets[1]
  eq(#set.devices, 1)
  local undone = restore(peer, "moddevice-delete", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(#set.devices, 2)
  eq(set.devices[2].name, "Pitch Envelope")
  eq(set.devices[2].points[2].scaling, 0.5)
  local moved = change(peer, "moddevice-move", { ref = moddevice_ref(place(peer), 1, 1, 1), index = 2 })
  eq(set.devices[2].name, "Volume AHDSR")
  eq(set.devices[2].params[1].value, 0.1, "its settings went with it")
  undone = restore(peer, "moddevice-move", moved)
  ok(undone.result, json.encode(undone.error or {}))
  eq(set.devices[1].name, "Volume AHDSR")
  instrument(1).macros[1].mappings = { { name = "Attack" } }
  failure(call(peer, "change_preview", { kind = "moddevice-move", input = { ref = moddevice_ref(place(peer), 1, 1, 1), index = 2 }, epoch = place(peer) }), "bad-params")
end)

test("modfx: a modulation device's operator, polarity and envelope points change and go back; its parameter by name too", function()
  local env, peer, epoch = setup_modfx()
  local record = change(peer, "moddevice-set", { ref = moddevice_ref(epoch, 1, 1, 2), operator = "multiply", bipolar = true, points = { { time = 16, value = 0.5 }, { time = 1, value = 0 } }, length = 32 })
  local device = instrument(1).sets[1].devices[2]
  eq(device.operator, 3)
  eq(device.bipolar, true)
  eq(device.points[1].time, 1, "sorted by time")
  eq(device.points[2].value, 0.5)
  eq(device.length, 32)
  local undone = restore(peer, "moddevice-set", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(device.operator, 1)
  eq(#device.points, 2)
  eq(device.points[2].time, 32)
  eq(device.length, 513, "Renoise's default length")
  local param = change(peer, "moddevice-param", { ref = moddevice_ref(place(peer), 1, 1, 1), param = "Release", value = 0.75 })
  eq(instrument(1).sets[1].devices[1].params[5].value, 0.75)
  undone = restore(peer, "moddevice-param", param)
  ok(undone.result, json.encode(undone.error or {}))
  eq(instrument(1).sets[1].devices[1].params[5].value, 0.5)
  failure(call(peer, "change_preview", { kind = "moddevice-set", input = { ref = moddevice_ref(place(peer), 1, 1, 1), points = { { time = 1, value = 1 } } }, epoch = place(peer) }), "bad-params")
end)

test("places, modulation device × a device made before it by the producer: the undo of its parameter follows it", function()
  local env, peer, epoch = setup_modfx()
  local record = change(peer, "moddevice-param", { ref = moddevice_ref(epoch, 1, 1, 1), param = 1, value = 0.3 })
  mock.song:instrument(1):sample_modulation_set(1):insert_device_at("Modulation/LFO", 1, 1)
  eq(instrument(1).sets[1].devices[2].name, "Volume AHDSR")
  local undone = restore(peer, "moddevice-param", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(instrument(1).sets[1].devices[2].params[1].value, 0.1)
  eq(instrument(1).sets[1].devices[1].params[1].value, 0.5, "the LFO untouched")
end)

-- Sample FX chains

test("modfx: a chain added with its output; Kumi's undo removes it, refused once a sample uses it; a bad output is bad-params", function()
  local env, peer, epoch = setup_modfx()
  local record, done = change(peer, "chain-add", { ref = iref(epoch, 1), name = "Wet", outputRouting = "04: Pad" })
  eq(#instrument(1).chains, 2)
  eq(instrument(1).chains[2].output_routing, "04: Pad")
  eq(done.created.kind, "chain")
  local undone = restore(peer, "chain-add", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(#instrument(1).chains, 1)
  record = change(peer, "chain-add", { ref = iref(place(peer), 1) })
  instrument(1).samples[1].device_chain_index = 2
  failure(restore(peer, "chain-add", record), "changed")
  failure(call(peer, "change_preview", { kind = "chain-add", input = { ref = iref(place(peer), 1), outputRouting = "Nowhere" }, epoch = place(peer) }), "bad-params")
end)

test("modfx: a deleted chain comes back with its devices' settings, its mixer and output, and its sample uses it again", function()
  local env, peer, epoch = setup_modfx()
  instrument(1).chains[1].devices[1].params[2].value = 0.7 -- the chain's mixer volume
  instrument(1).chains[1].output_routing = "03: Bass"
  local record = change(peer, "chain-delete", { ref = chain_ref(epoch, 1, 1) })
  eq(#instrument(1).chains, 0)
  eq(instrument(1).samples[2].device_chain_index, 0)
  local undone = restore(peer, "chain-delete", record)
  ok(undone.result, json.encode(undone.error or {}))
  local chain = instrument(1).chains[1]
  eq(chain.name, "Tone")
  eq(chain.output_routing, "03: Bass")
  eq(chain.devices[1].params[2].value, 0.7)
  eq(chain.devices[2].path, "Audio/Effects/Native/EQ 5")
  eq(chain.devices[2].preset_data, "<eq gain=3/>")
  eq(instrument(1).samples[2].device_chain_index, 1)
end)

test("modfx: a chain's name and output change and go back", function()
  local env, peer, epoch = setup_modfx()
  local record = change(peer, "chain-set", { ref = chain_ref(epoch, 1, 1), name = "Bright", outputRouting = "02: Hat" })
  eq(instrument(1).chains[1].name, "Bright")
  eq(instrument(1).chains[1].output_routing, "02: Hat")
  local undone = restore(peer, "chain-set", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(instrument(1).chains[1].output_routing, "Current Track")
end)

test("modfx: chain devices: added by name after the mixer, bypassed, moved, a parameter by name, deleted with its settings; each goes back", function()
  local env, peer, epoch = setup_modfx()
  local chain = instrument(1).chains[1]
  local added = change(peer, "chaindevice-add", { ref = chain_ref(epoch, 1, 1), path = "Gainer" })
  eq(chain.devices[3].path, "Audio/Effects/Native/Gainer")
  local bypassed = change(peer, "chaindevice-bypass", { ref = chaindevice_ref(place(peer), 1, 1, 3), bypass = true })
  eq(chain.devices[3].active, false)
  local moved = change(peer, "chaindevice-move", { ref = chaindevice_ref(place(peer), 1, 1, 3), index = 2 })
  eq(chain.devices[2].path, "Audio/Effects/Native/Gainer")
  local param = change(peer, "chaindevice-param", { ref = chaindevice_ref(place(peer), 1, 1, 3), param = "Gain 1", value = -6 })
  eq(chain.devices[3].params[1].value, -6)
  local by_ref = change(peer, "chaindevice-param", { ref = place(peer) .. ":chainparam:1.1.3.2", value = 2 })
  eq(chain.devices[3].params[2].value, 2)
  ok(restore(peer, "chaindevice-param", by_ref).result)
  ok(restore(peer, "chaindevice-param", param).result)
  eq(chain.devices[3].params[1].value, 3)
  ok(restore(peer, "chaindevice-move", moved).result)
  eq(chain.devices[3].path, "Audio/Effects/Native/Gainer")
  ok(restore(peer, "chaindevice-bypass", bypassed).result)
  eq(chain.devices[3].active, true)
  ok(restore(peer, "chaindevice-add", added).result)
  eq(#chain.devices, 2)
  local deleted = change(peer, "chaindevice-delete", { ref = chaindevice_ref(place(peer), 1, 1, 2) })
  eq(#chain.devices, 1)
  local undone = restore(peer, "chaindevice-delete", deleted)
  ok(undone.result, json.encode(undone.error or {}))
  eq(chain.devices[2].preset_data, "<eq gain=3/>")
  failure(call(peer, "change_preview", { kind = "chaindevice-delete", input = { ref = chaindevice_ref(place(peer), 1, 1, 1) }, epoch = place(peer) }), "bad-params")
  failure(call(peer, "change_preview", { kind = "chaindevice-add", input = { ref = chain_ref(place(peer), 1, 1), path = "Nope" }, epoch = place(peer) }), "bad-params")
end)

test("modfx: after Renoise's own undo and redo of a set, a modulation device, a chain or a chain device Kumi added, Kumi's undo takes it away", function()
  local cases = {
    { "modset-add", function(E) return { ref = iref(E, 1), name = "Filter" } end, function() return #instrument(1).sets end, 1 },
    { "moddevice-add", function(E) return { ref = modset_ref(E, 1, 1), path = "LFO", target = "pitch" } end, function() return #instrument(1).sets[1].devices end, 2 },
    { "chain-add", function(E) return { ref = iref(E, 1), name = "Verb" } end, function() return #instrument(1).chains end, 1 },
    { "chaindevice-add", function(E) return { ref = chain_ref(E, 1, 1), path = "Gainer" } end, function() return #instrument(1).chains[1].devices end, 2 },
  }
  for _, case in ipairs(cases) do
    local env, peer = setup_modfx()
    local record = change(peer, case[1], case[2](place(peer)))
    mock.song:undo()
    eq(case[3](), case[4], case[1] .. ": Renoise's undo")
    mock.song:redo()
    eq(case[3](), case[4] + 1, case[1] .. ": Renoise's redo")
    local undone = restore(peer, case[1], record)
    ok(undone.result, case[1] .. ": what Renoise's redo made again is Kumi's: " .. json.encode(undone.error or {}))
    eq(case[3](), case[4], case[1])
  end
end)

test("modfx: Renoise's own undo and redo of a modulation device's move are told, and Kumi's undo moves it back after them", function()
  local env, peer, epoch = setup_modfx()
  local function names()
    local out = {}
    for k, d in ipairs(instrument(1).sets[1].devices) do out[k] = d.path:match("([^/]+)$") end
    return table.concat(out, ",")
  end
  eq(names(), "AHDSR,Envelope")
  local record = change(peer, "moddevice-move", { ref = moddevice_ref(epoch, 1, 1, 1), index = 2 }, "Kumi: move (km51)")
  eq(names(), "Envelope,AHDSR")
  I.told(peer)
  mock.song:undo()
  eq(names(), "AHDSR,Envelope")
  local events = I.told(peer)
  eq(#events, 1, "the undo told")
  eq(events[1].data.how, "undo")
  mock.song:redo()
  eq(names(), "Envelope,AHDSR")
  events = I.told(peer)
  eq(#events, 1, "the redo told")
  eq(events[1].data.how, "redo")
  local undone = restore(peer, "moddevice-move", record, nil, { of = "Kumi: move (km51)" })
  ok(undone.result, "the move Renoise's redo made again: " .. json.encode(undone.error or {}))
  eq(names(), "AHDSR,Envelope")
end)

test("places, chain device deleted × a device the producer made and deleted at its place since: it goes back after the mixer", function()
  local env, peer, epoch = setup_modfx()
  local chain = mock.song:instrument(1):sample_device_chain(1)
  chain:insert_device_at("Audio/Effects/Native/Gainer", 3)
  local deleted = change(peer, "chaindevice-delete", { ref = chaindevice_ref(place(peer), 1, 1, 2) })
  chain:insert_device_at("Audio/Effects/Native/Delay", 2)
  chain:delete_device_at(2)
  local undone = restore(peer, "chaindevice-delete", deleted)
  ok(undone.result, json.encode(undone.error or {}))
  local devices = instrument(1).chains[1].devices
  eq(devices[2].path, "Audio/Effects/Native/EQ 5")
  eq(devices[3].path, "Audio/Effects/Native/Gainer")
end)

test("places, chain device × the producer's device made before it: Kumi's undo of its bypass follows it; deleted: gone", function()
  local env, peer, epoch = setup_modfx()
  local record = change(peer, "chaindevice-bypass", { ref = chaindevice_ref(epoch, 1, 1, 2), bypass = true })
  mock.song:instrument(1):sample_device_chain(1):insert_device_at("Audio/Effects/Native/Delay", 2)
  local undone = restore(peer, "chaindevice-bypass", record)
  ok(undone.result, json.encode(undone.error or {}))
  eq(instrument(1).chains[1].devices[3].active, true)
  record = change(peer, "chaindevice-bypass", { ref = chaindevice_ref(place(peer), 1, 1, 3), bypass = true })
  mock.song:instrument(1):sample_device_chain(1):delete_device_at(3)
  failure(restore(peer, "chaindevice-bypass", record), "gone")
end)

test("modfx: Kumi's undo of a chain device's move refuses it moved again in Renoise since, unless discard (#361 N12)", function()
  local env, peer, epoch = setup_modfx()
  local chain = instrument(1).chains[1]
  -- Mixer, EQ 5, Gainer, Delay.
  ok(change(peer, "chaindevice-add", { ref = chain_ref(epoch, 1, 1), path = "Gainer" }))
  ok(change(peer, "chaindevice-add", { ref = chain_ref(place(peer), 1, 1), path = "Delay" }))
  local moved = change(peer, "chaindevice-move", { ref = chaindevice_ref(place(peer), 1, 1, 2), index = 4 })
  eq(chain.devices[4].path, "Audio/Effects/Native/EQ 5")
  -- The producer moves it to place 3.
  mock.song:instrument(1):sample_device_chain(1):swap_devices_at(3, 4)
  T.harness.tick(2)
  eq(chain.devices[3].path, "Audio/Effects/Native/EQ 5")
  failure(restore(peer, "chaindevice-move", moved), "changed", "moved in Renoise")
  eq(chain.devices[3].path, "Audio/Effects/Native/EQ 5", "nothing moved")
  ok(restore(peer, "chaindevice-move", moved, nil, { discard = true }).result)
  eq(chain.devices[2].path, "Audio/Effects/Native/EQ 5")
end)
