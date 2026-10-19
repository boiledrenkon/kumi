-- Kumi's changes to the devices on a track: bypass and parameter values (and, below, putting a device in, taking it out and
-- moving it).

local import = ...
local refs = import("refs")
local rows = import("rows")
local base = import("changes_base")

local bad, fail = base.bad, base.fail
local M = {}

-- A device is found by its ref (`E:device:track.device`), a parameter by `E:param:track.device.parameter`; both are looked at
-- again by their place. The track's first device is its mixer device, which is the track's fader and pan: not changed here.
local function device_of(song, env, reference, what, count)
  if type(reference) ~= "string" then bad(what .. ": ref must be a " .. (count == 3 and "parameter" or "device") .. " ref") end
  local numbers = refs.expect(reference, count == 3 and "param" or "device", count, env.epoch())
  local index, at = numbers[1], numbers[2]
  local track = base.track_at(song, index)
  if at < 1 or at > #track.devices then fail("gone", "that device isn't on the track any more") end
  if at == 1 then bad("a track's first device is its mixer device: change its volume and pan with track-volume and track-pan") end
  local device = track:device(at)
  local name = device.display_name ~= "" and device.display_name or device.name
  return index, at, track, device, name .. " on " .. base.named(index, track), numbers[3]
end

local function device_target(env, index, at)
  return { ref = refs.make(env.epoch(), "device", index, at), kind = "device" }
end

local function device_prop(spec)
  spec.locate = function(song, env, input)
    local index, at, _, device, name = device_of(song, env, input.ref, spec.kind, 2)
    return { object = device, name = name, place = { track = index, device = at }, target = device_target(env, index, at) }
  end
  spec.reread = function(song, place)
    if place.track > rows.track_count(song) then return nil end
    local track = song:track(place.track)
    if place.device > #track.devices then return nil end
    return track:device(place.device)
  end
  base.register(spec)
end

device_prop {
  kind = "device-bypass", noun = "bypass", undoable = true,
  read = function(device) return { bypass = not device.is_active } end,
  wanted = function(_, input)
    if type(input.bypass) ~= "boolean" then bad("bypass must be true or false") end
    return { bypass = input.bypass }
  end,
  write = function(device, state) device.is_active = not state.bypass end,
  same = function(a, b) return a.bypass == b.bypass end,
  title = function(name, _, to) return name .. (to.bypass and " bypassed" or " switched on") end,
}

-- A parameter's value is in its own range; text is parsed by Renoise. A Receiver of the #Send device is a send track's number.
local function param_state(parameter)
  return { value = parameter.value, valueString = parameter.value_string }
end

local function receiver_check(device, parameter, value, song)
  if device.device_path:find("#Send", 1, true) and parameter.name == "Receiver" then
    local sends = song.send_track_count
    if value ~= math.floor(value) or value < 0 or value >= sends then
      bad(string.format("Receiver is a send track's number from 0 to %d (there are %d send tracks)", sends - 1, sends))
    end
  end
end

local param_spec = {
  kind = "device-param", noun = "parameter", undoable = true,
  read = function(parameter) return param_state(parameter) end,
  wanted = function(parameter, input)
    if type(input.value) == "number" then
      if input.value ~= input.value or input.value < parameter.value_min or input.value > parameter.value_max then
        bad(string.format("value must be from %s to %s", tostring(parameter.value_min), tostring(parameter.value_max)))
      end
      return { value = input.value }
    elseif type(input.valueString) == "string" then
      return { valueString = input.valueString }
    end
    bad("value (a number in the parameter's own range) or valueString (text as Renoise shows it)")
  end,
  write = function(parameter, state)
    if state.value ~= nil then
      parameter.value = state.value
    else
      parameter.value_string = state.valueString
    end
  end,
  same = function(a, b)
    if a.value ~= nil and b.value ~= nil then return base.close(a.value, b.value) end
    return a.valueString == b.valueString
  end,
  title = function(name, _, to) return name .. " " .. (to.valueString or tostring(to.value)) end,
}

param_spec.locate = function(song, env, input)
  local index, at, _, device, name, k = device_of(song, env, input.ref, param_spec.kind, 3)
  if k < 1 or k > #device.parameters then fail("gone", "that parameter isn't on the device any more") end
  local parameter = device:parameter(k)
  if input.value ~= nil then receiver_check(device, parameter, input.value, song) end
  return {
    object = parameter, name = name .. " " .. parameter.name, place = { track = index, device = at, param = k },
    target = { ref = refs.make(env.epoch(), "param", index, at, k), kind = "param" },
  }
end
param_spec.reread = function(song, place)
  if place.track > rows.track_count(song) then return nil end
  local track = song:track(place.track)
  if place.device > #track.devices then return nil end
  local device = track:device(place.device)
  if place.param > #device.parameters then return nil end
  return device:parameter(place.param)
end
-- The producer's undo moves a parameter: asked of its observable, once.
param_spec.watch = function(env, loc) env.watch_param(loc.place.track, loc.place.device, loc.place.param) end
base.register(param_spec)

return M
