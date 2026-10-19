-- Kumi's changes to devices (M4 n-devices): presets, preset data, names and looks; the Doofer, Splitter and Notepad edits
-- Kumi's host composes (written here, fenced by the shape the host edited from); preset_read. Against the mock Renoise.
local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness

local next_id = 9000

local function drain(peer)
  repeat
    local taken = peer:take()
    if #taken > 0 then H.send(peer, { type = "credit", bytes = #taken }) end
  until #taken == 0
end

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

-- An EQ 5 with Renoise's XML preset data (two gains saved as Gain0/Gain1, two presets with their own values), a Gainer, and
-- a Doofer whose chain is in its XML.
local function eq5(extra)
  local data = {
    name = "EQ 5", path = "Audio/Effects/Native/EQ 5", slot = "Eq5Device",
    params = {
      { name = "Gain 1 (100 Hz)", element = "Gain0", value = 0, min = -20, max = 20 },
      { name = "Gain 2 (1.00 kHz)", element = "Gain1", value = 0, min = -20, max = 20 },
    },
    presets = { "Init", "Bright" }, preset = 1, preset_values = { [1] = { 0, 0 }, [2] = { -2, 6 } }, maximized = true,
  }
  for k, v in pairs(extra or {}) do data[k] = v end
  return data
end

local function gainer()
  return {
    name = "Gainer", path = "Audio/Effects/Native/Gainer", slot = "GainerDevice",
    params = { { name = "Gain", element = "Volume", value = 1, min = 0, max = 4 }, { name = "Panning", element = "Panning", value = 0.5, min = 0, max = 1 } },
    presets = { "Init" }, preset = 1,
  }
end

local DOOFER_BODY = "    <Macro0>\n      <Value>50</Value>\n      <Name>Macro 1</Name>\n    </Macro0>\n    <NumActiveMacros>4</NumActiveMacros>\n"
  .. "    <DeviceChain>\n      <Devices>\n        <GainerDevice type=\"GainerDevice\">\n          <Volume>\n            <Value>1.0</Value>\n"
  .. "          </Volume>\n        </GainerDevice>\n      </Devices>\n    </DeviceChain>"

local function doofer()
  local params = {}
  for k = 1, 8 do params[k] = { name = "Macro " .. k, value = 50, min = 0, max = 100, element = "Macro" .. (k - 1) } end
  return { name = "Doofer", path = "Audio/Effects/Native/Doofer", slot = "DooferDevice", params = params, xml_body = DOOFER_BODY, presets = { "Init" }, preset = 1 }
end

local function setup(tracks)
  local env = H.setup({ song = {
    name = "Lab", file_name = "/s/lab.xrns", bpm = 120, lpb = 4, mixer_device = true,
    tracks = tracks or {
      { name = "Kick" }, { name = "Bass", devices = { eq5(), gainer() } }, { name = "Pad", devices = { doofer() } },
    },
    send_tracks = 1, patterns = 2, pattern_lines = 16, instruments = 1,
  } })
  local peer = H.connect(env)
  H.frames(peer)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  return env, peer
end

local function place(peer)
  local focus = call(peer, "focus").result
  return focus.epoch, focus.layout
end

local function steps() return #mock.data.undo.stack end
local function newest() local top = mock.data.undo.stack[#mock.data.undo.stack] return top and top.desc end

local function preview(peer, kind, input)
  local epoch, layout = place(peer)
  local response = call(peer, "change_preview", { kind = kind, input = input, epoch = epoch, layout = layout })
  ok(response and response.result, kind .. " preview: " .. json.encode(response and response.error or {}))
  return response.result, epoch, layout
end

local function apply(peer, kind, input, previewed, desc, epoch, layout)
  return call(peer, "change_apply", { kind = kind, input = input, fence = previewed.fence, desc = desc, epoch = epoch, layout = layout })
end

-- Preview and apply; the record the host would keep (with the apply's epoch and layout).
local function change(peer, kind, input, desc)
  local previewed, epoch, layout = preview(peer, kind, input)
  local done = apply(peer, kind, input, previewed, desc or ("Kumi: " .. kind .. " (k0001)"), epoch, layout)
  ok(done.result, kind .. " apply: " .. json.encode(done.error or {}))
  local record = done.result
  record.prior = previewed.prior
  return record, previewed
end

local function restore(peer, kind, record, desc, more)
  local params = {
    kind = kind, target = record.target, prior = record.prior, applied = record.applied, created = record.created,
    material = record.material, desc = desc or ("Kumi: undo " .. kind .. " (k0002)"), epoch = record.epoch, layout = record.layout,
  }
  for k, v in pairs(more or {}) do params[k] = v end
  return call(peer, "change_restore", params)
end

local function failure(response, code, pattern)
  ok(response and response.error, "expected an error, got " .. json.encode(response and response.result or {}))
  eq(response.error.code, code, "error code (" .. tostring(response.error.message) .. ")")
  if pattern then ok(tostring(response.error.message):find(pattern), response.error.message) end
end

local function device_data(t, d) return mock.data.tracks[t].devices[d] end
local function value(t, d, k) return device_data(t, d).params[k].value end

-- device-preset

test("devices m4: a preset is chosen in one step and its undo writes back the settings it replaced", function()
  local _, peer = setup()
  local epoch = place(peer)
  -- (the producer's own settings first: Gain 1 at -7)
  device_data(2, 2).params[1].value = -7
  local record, previewed = change(peer, "device-preset", { ref = epoch .. ":device:2.2", preset = "bright" }, "Kumi: EQ 5 on Bass: preset “Bright” (k1)")
  eq(previewed.display.title, "EQ 5 on Bass: preset \226\128\156Bright\226\128\157")
  eq(previewed.prior.preset, 1)
  eq(record.applied.preset, 2)
  eq(value(2, 2, 1), -2)
  eq(value(2, 2, 2), 6)
  eq(steps(), 1)
  eq(newest(), "Kumi: EQ 5 on Bass: preset “Bright” (k1)")
  ok(record.material.presetData:find("<Value>-7</Value>", 1, true), "the replaced settings are kept")
  eq(record.material.track, 2)
  eq(record.target.kind, "device")
  local back = restore(peer, "device-preset", record)
  ok(back.result, json.encode(back.error or {}))
  eq(value(2, 2, 1), -7, "the producer's gain is back")
  eq(value(2, 2, 2), 0)
  eq(steps(), 2)
  -- Asked again: back already, nothing written.
  local again = restore(peer, "device-preset", record)
  ok(again.result and again.result.undo.entry == false, json.encode(again))
  eq(steps(), 2)
  H.no_dialogs()
end)

test("devices m4: a preset is named by number, name or the last part of a name; a wrong one is refused with the list", function()
  local _, peer = setup()
  local epoch = place(peer)
  local by_number = preview(peer, "device-preset", { ref = epoch .. ":device:2.2", preset = 2 })
  eq(by_number.display.after.preset, "Bright")
  local response = call(peer, "change_preview", { kind = "device-preset", input = { ref = epoch .. ":device:2.2", preset = "Dark" }, epoch = epoch, layout = select(2, place(peer)) })
  failure(response, "bad-params", "Init, Bright")
  response = call(peer, "change_preview", { kind = "device-preset", input = { ref = epoch .. ":device:2.2", preset = 3 }, epoch = epoch, layout = select(2, place(peer)) })
  failure(response, "bad-params", "from 1 to 2")
  -- The mixer device is the track's fader.
  response = call(peer, "change_preview", { kind = "device-preset", input = { ref = epoch .. ":device:2.1", preset = 1 }, epoch = epoch, layout = select(2, place(peer)) })
  failure(response, "bad-params", "mixer")
end)

test("devices m4: a preset choice refuses a device whose settings moved since the preview (its fence), writing nothing", function()
  local _, peer = setup()
  local epoch, layout = place(peer)
  local input = { ref = epoch .. ":device:2.2", preset = "Bright" }
  local previewed = preview(peer, "device-preset", input)
  device_data(2, 2).params[2].value = 3 -- (the producer, between preview and apply)
  local done = apply(peer, "device-preset", input, previewed, "Kumi: x (k1)", epoch, layout)
  failure(done, "changed")
  eq(value(2, 2, 2), 3)
  eq(steps(), 0, "nothing written")
end)

test("devices m4: the undo of a preset refuses settings the producer changed since, and discard puts them back anyway", function()
  local _, peer = setup()
  local epoch = place(peer)
  local record = change(peer, "device-preset", { ref = epoch .. ":device:2.2", preset = 2 })
  device_data(2, 2).params[1].value = 11
  local refused = restore(peer, "device-preset", record)
  failure(refused, "changed", "changed in Renoise")
  eq(value(2, 2, 1), 11, "nothing written")
  local forced = restore(peer, "device-preset", record, nil, { discard = true })
  ok(forced.result, json.encode(forced.error or {}))
  eq(value(2, 2, 1), 0)
end)

test("devices m4: the undo of a preset finds the device through devices and tracks made, moved and deleted since", function()
  local _, peer = setup()
  local epoch = place(peer)
  local record = change(peer, "device-preset", { ref = epoch .. ":device:2.2", preset = 2 })
  local song = mock.song
  -- A device made before it on its track, one on another track, a track made before its track, a swap of the two after.
  song:track(2):insert_device_at("Audio/Effects/Native/Gainer", 2)
  song:track(1):insert_device_at("Audio/Effects/Native/Gainer", 2)
  mock.api.insert_track_at(1)
  H.tick(2)
  local layout_now = select(2, place(peer))
  ok(layout_now > record.layout, "the layout moved")
  local back = restore(peer, "device-preset", record)
  ok(back.result, json.encode(back.error or {}))
  eq(device_data(3, 3).params[1].value, 0, "the EQ (now track 3, device 3) has its settings back")
  eq(device_data(3, 3).name, "EQ 5")
end)

test("devices m4: the undo of a preset refuses what it can't follow (the EQ deleted, another device at its place)", function()
  local _, peer = setup()
  local epoch = place(peer)
  local record = change(peer, "device-preset", { ref = epoch .. ":device:2.2", preset = 2 })
  mock.song:track(2):delete_device_at(2)
  H.tick(2)
  failure(restore(peer, "device-preset", record), "gone", "deleted")
  -- Its track deleted.
  local _, peer2 = setup()
  local epoch2 = place(peer2)
  local record2 = change(peer2, "device-preset", { ref = epoch2 .. ":device:2.2", preset = 2 })
  mock.api.delete_track_at(2)
  H.tick(2)
  failure(restore(peer2, "device-preset", record2), "gone")
end)

test("devices m4: Renoise's own undo of a preset choice is told, and Kumi's undo then finds it back already", function()
  local _, peer = setup()
  local epoch = place(peer)
  local record = change(peer, "device-preset", { ref = epoch .. ":device:2.2", preset = 2 }, "Kumi: preset (kabc1)")
  peer.events = {}
  mock.api.undo()
  H.tick(4)
  local told
  for _, event in ipairs(peer.events) do
    if event.name == "undone_in_daw" then told = event.data end
  end
  call(peer, "ping")
  for _, event in ipairs(peer.events) do
    if event.name == "undone_in_daw" then told = event.data end
  end
  ok(told, "undone_in_daw")
  eq(told.descs[1], "Kumi: preset (kabc1)")
  eq(told.how, "undo")
  local back = restore(peer, "device-preset", record)
  ok(back.result and back.result.undo.entry == false, json.encode(back))
end)

-- device-preset-data

test("devices m4: preset data is written whole in one step and put back; another device's data is refused before Renoise crashes", function()
  local _, peer = setup()
  local epoch, layout = place(peer)
  local current = mock.song:track(2):device(2).active_preset_data
  local wanted = current:gsub("(<Gain1>%s*<Value>)[^<]*", "%1-4.5", 1)
  local record = change(peer, "device-preset-data", { ref = epoch .. ":device:2.2", presetData = wanted })
  eq(value(2, 2, 2), -4.5)
  eq(steps(), 1)
  local gainer_data = mock.song:track(2):device(3).active_preset_data
  local response = call(peer, "change_preview", { kind = "device-preset-data", input = { ref = epoch .. ":device:2.2", presetData = gainer_data }, epoch = epoch, layout = layout })
  failure(response, "bad-params", "GainerDevice")
  local newer = current:gsub('doc_version="14"', 'doc_version="15"', 1)
  response = call(peer, "change_preview", { kind = "device-preset-data", input = { ref = epoch .. ":device:2.2", presetData = newer }, epoch = epoch, layout = layout })
  failure(response, "bad-params", "newer")
  response = call(peer, "change_preview", { kind = "device-preset-data", input = { ref = epoch .. ":device:2.2", presetData = "<oops" }, epoch = epoch, layout = layout })
  failure(response, "bad-params", "isn't a device preset")
  local back = restore(peer, "device-preset-data", record)
  ok(back.result, json.encode(back.error or {}))
  eq(value(2, 2, 2), 0)
  H.no_dialogs()
end)

test("devices m4: preset data a plug-in of another kind would get is refused", function()
  local plugin = { name = "AUDelay", path = "Audio/Effects/AU/aufx:dely:appl", slot = "AudioPluginDevice",
    params = { { name = "Dry/Wet Mix", value = 0.5, min = 0, max = 1 } },
    xml_body = "    <PluginIdentifier>aufx:dely:appl</PluginIdentifier>" }
  local _, peer = setup({ { name = "Kick", devices = { plugin } } })
  local epoch, layout = place(peer)
  local data = mock.song:track(1):device(2).active_preset_data:gsub("aufx:dely:appl", "aufx:dist:appl")
  local response = call(peer, "change_preview", { kind = "device-preset-data", input = { ref = epoch .. ":device:1.2", presetData = data }, epoch = epoch, layout = layout })
  failure(response, "bad-params", "aufx:dist:appl")
end)

-- device-set

test("devices m4: a device is renamed and collapsed in one step, put back, and refused when renamed again since", function()
  local _, peer = setup()
  local epoch = place(peer)
  local record, previewed = change(peer, "device-set", { ref = epoch .. ":device:2.2", name = "Low cut", maximized = false }, "Kumi: rename (k1)")
  eq(previewed.display.title, "Renamed \226\128\156EQ 5\226\128\157 on \226\128\156Bass\226\128\157 \226\134\146 \226\128\156Low cut\226\128\157, collapsed")
  eq(device_data(2, 2).display_name, "Low cut")
  eq(device_data(2, 2).maximized, false)
  eq(steps(), 1)
  local back = restore(peer, "device-set", record)
  ok(back.result, json.encode(back.error or {}))
  eq(device_data(2, 2).display_name, "EQ 5")
  local record2 = change(peer, "device-set", { ref = place(peer) .. ":device:2.2", name = "Air" })
  mock.song:track(2):device(2).display_name = "Producer's"
  failure(restore(peer, "device-set", record2), "changed", "renamed")
  eq(device_data(2, 2).display_name, "Producer's")
end)

test("devices m4: a collapse alone is titled by the device; the same look again is unchanged and makes no step", function()
  local _, peer = setup()
  local epoch = place(peer)
  device_data(2, 2).maximized = true
  local previewed = preview(peer, "device-set", { ref = epoch .. ":device:2.2", maximized = false })
  eq(previewed.display.title, "EQ 5 on Bass collapsed")
  local same = preview(peer, "device-set", { ref = epoch .. ":device:2.2", maximized = true })
  eq(same.display.unchanged, true)
  local done = change(peer, "device-set", { ref = epoch .. ":device:2.2", maximized = true })
  eq(done.undo.entry, false)
  eq(steps(), 0)
end)

-- The host's XML kinds

test("devices m4: preset_read gives the host a device's preset data, its shape and where a record finds it", function()
  local _, peer = setup()
  local epoch, layout = place(peer)
  local read = call(peer, "preset_read", { ref = epoch .. ":device:3.2", epoch = epoch, layout = layout })
  ok(read.result, json.encode(read.error or {}))
  ok(read.result.presetData:find('<DeviceSlot type="DooferDevice">', 1, true))
  eq(read.result.path, "Audio/Effects/Native/Doofer")
  eq(read.result.at.track, 3)
  eq(read.result.parameters[1].name, "Macro 1")
  ok(read.result.shape:match("^%x+:%d+$"), read.result.shape)
  -- A macro moved (a value) leaves the shape as it was; a name changed doesn't.
  mock.song:track(3):device(2):parameter(1).value = 80
  local moved = call(peer, "preset_read", { ref = epoch .. ":device:3.2", epoch = epoch, layout = layout }).result
  eq(moved.shape, read.result.shape)
  ok(moved.print ~= read.result.print)
  -- By a record's place, after a device made before it.
  mock.song:track(3):insert_device_at("Audio/Effects/Native/Gainer", 2)
  H.tick(2)
  local e2, l2 = place(peer)
  local by_place = call(peer, "preset_read", { at = read.result.at, epoch = epoch, layout = layout })
  ok(by_place.result, json.encode(by_place.error or {}))
  eq(by_place.result.ref, e2 .. ":device:3.3")
  eq(l2, layout)
end)

test("devices m4: a host-made Doofer edit is written whole if its shape holds, and refused when it changed in between", function()
  local _, peer = setup()
  local epoch, layout = place(peer)
  local read = call(peer, "preset_read", { ref = epoch .. ":device:3.2", epoch = epoch, layout = layout }).result
  local edited = read.presetData:gsub("<Name>Macro 1</Name>", "<Name>Drive</Name>", 1)
  local fence = { id = read.id }
  local input = { ref = epoch .. ":device:3.2", presetData = edited, shape = read.shape, display = { title = "Doofer on Pad: macro 1 named “Drive”" } }
  local done = call(peer, "change_apply", { kind = "doofer-macro", input = input, fence = fence, desc = "Kumi: macro (k1)", epoch = epoch, layout = layout })
  ok(done.result, json.encode(done.error or {}))
  ok(device_data(3, 2).preset_data:find("<Name>Drive</Name>", 1, true))
  eq(done.result.display.title, "Doofer on Pad: macro 1 named “Drive”")
  eq(steps(), 1)
  -- The producer renames the macro in Renoise: the host's next edit from the old read is refused.
  device_data(3, 2).preset_data = device_data(3, 2).preset_data:gsub("<Name>Drive</Name>", "<Name>Theirs</Name>", 1)
  local stale = call(peer, "change_apply", { kind = "doofer-macro", input = input, fence = fence, desc = "Kumi: macro (k2)", epoch = epoch, layout = layout })
  failure(stale, "changed", "read it again")
  eq(steps(), 1)
  -- The restore the host composes, written by the record's place.
  local now = call(peer, "preset_read", { at = done.result.material, epoch = done.result.epoch, layout = done.result.layout }).result
  local material = done.result.material
  material.write = { presetData = now.presetData:gsub("<Name>Theirs</Name>", "<Name>Macro 1</Name>", 1), shape = now.shape }
  local back = call(peer, "change_restore", { kind = "doofer-macro", target = done.result.target, prior = {}, applied = done.result.applied,
    material = material, desc = "Kumi: undo macro (k3)", epoch = done.result.epoch, layout = done.result.layout })
  ok(back.result, json.encode(back.error or {}))
  ok(device_data(3, 2).preset_data:find("<Name>Macro 1</Name>", 1, true))
end)

test("devices m4: host-made edits go only to the devices they are for, with their own device type's data", function()
  local _, peer = setup()
  local epoch, layout = place(peer)
  local eq_read = call(peer, "preset_read", { ref = epoch .. ":device:2.2", epoch = epoch, layout = layout }).result
  local response = call(peer, "change_apply", { kind = "doofer-macro", input = { ref = epoch .. ":device:2.2", presetData = eq_read.presetData, shape = eq_read.shape },
    fence = { id = eq_read.id }, desc = "Kumi: x (k1)", epoch = epoch, layout = layout })
  failure(response, "bad-params", "isn't a device")
  local doofer_read = call(peer, "preset_read", { ref = epoch .. ":device:3.2", epoch = epoch, layout = layout }).result
  response = call(peer, "change_apply", { kind = "doofer-macro", input = { ref = epoch .. ":device:3.2", presetData = eq_read.presetData, shape = doofer_read.shape },
    fence = { id = doofer_read.id }, desc = "Kumi: x (k1)", epoch = epoch, layout = layout })
  failure(response, "bad-params", "crash")
  response = call(peer, "change_preview", { kind = "doofer-macro", input = { ref = epoch .. ":device:3.2" }, epoch = epoch, layout = layout })
  failure(response, "bad-params", "host")
  eq(steps(), 0)
  H.no_dialogs()
end)
