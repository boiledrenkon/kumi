-- What the change tests share: a song, a host on the wire, and the calls a host makes (preview, apply, restore), against the
-- mock Renoise, which keeps an undo step for every callback that writes.
local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local H = T.harness

local K = {}
local next_id = 90000

local function drain(peer)
  repeat
    local taken = peer:take()
    if #taken > 0 then H.send(peer, { type = "credit", bytes = #taken }) end
  until #taken == 0
end

-- Sends a request and runs the clock until its response comes; the events that came meanwhile are kept in `peer.events`.
function K.call(peer, method, params, ticks)
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

-- A song of four tracks (Kick, Hat, Bass, Pad) with the mixer device every Renoise track has, a master and a send; three
-- patterns of 32 lines in the sequence 1, 2, 1, 3.
function K.song_options(extra)
  local spec = {
    name = "Lab", file_name = "/s/lab.xrns", bpm = 120, lpb = 4, mixer_device = true,
    tracks = { { name = "Kick", color = { 255, 0, 0 } }, { name = "Hat" }, { name = "Bass", note_columns = 2 }, { name = "Pad" } },
    send_tracks = 1, patterns = 3, pattern_lines = 32, sequence_length = 4, sequence = { 1, 2, 1, 3 }, instruments = 1,
  }
  for k, v in pairs(extra or {}) do spec[k] = v end
  return spec
end

function K.setup(options, extra)
  options = options or {}
  options.song = K.song_options(extra)
  local env = H.setup(options)
  local peer = H.connect(env)
  H.frames(peer)
  mock.data.undo.stack, mock.data.undo.redo = {}, {}
  return env, peer
end

function K.place(peer)
  local focus = K.call(peer, "focus").result
  return focus.epoch, focus.layout
end

function K.ref(epoch, kind, ...) return epoch .. ":" .. kind .. ":" .. table.concat({ ... }, ".") end

function K.preview(peer, kind, input)
  local epoch, layout = K.place(peer)
  local response = K.call(peer, "change_preview", { kind = kind, input = input, epoch = epoch, layout = layout })
  T.ok(response and response.result, kind .. " preview: " .. json.encode(response and response.error or {}))
  return response.result, epoch, layout
end

function K.ask(peer, method, kind, input)
  local epoch, layout = K.place(peer)
  return K.call(peer, method, { kind = kind, input = input, epoch = epoch, layout = layout }), epoch, layout
end

function K.apply(peer, kind, input, desc, fence, epoch, layout)
  return K.call(peer, "change_apply", { kind = kind, input = input, fence = fence, desc = desc, epoch = epoch, layout = layout })
end

-- Preview and apply; the record a host keeps (with the epoch and layout the apply answered in).
function K.change(peer, kind, input, desc)
  local previewed, epoch, layout = K.preview(peer, kind, input)
  local done = K.apply(peer, kind, input, desc or ("Kumi: " .. kind .. " (k1x)"), previewed.fence, epoch, layout)
  T.ok(done and done.result, kind .. " apply: " .. json.encode(done and done.error or {}))
  local result = done.result
  return {
    kind = kind, target = result.target or previewed.target, prior = previewed.prior, applied = result.applied, created = result.created,
    material = result.material, epoch = result.epoch, layout = result.layout, result = result, previewed = previewed,
  }
end

function K.restore(peer, record, desc, more)
  local params = {
    kind = record.kind, target = record.target, prior = record.prior, applied = record.applied, created = record.created,
    material = record.material, desc = desc or ("Kumi: undo " .. record.kind .. " (k2x)"), epoch = record.epoch, layout = record.layout,
  }
  for k, v in pairs(more or {}) do params[k] = v end
  return K.call(peer, "change_restore", params, 800)
end

function K.failure(response, code)
  T.ok(response and response.error, "expected an error, got " .. json.encode(response and response.result or "nothing"))
  T.eq(response.error.code, code, "error code (" .. tostring(response.error.message) .. ")")
  return response.error.message
end

-- The song's undo: how many steps, and the newest one's name.
function K.steps() return #mock.data.undo.stack end
function K.newest() local top = mock.data.undo.stack[#mock.data.undo.stack] return top and top.desc end

-- The undone_in_daw events told since the last look (after a few ticks), as "how:desc" texts.
function K.told(peer)
  H.tick(3)
  drain(peer)
  local out = {}
  local function take(frame)
    if frame.type == "event" and frame.name == "undone_in_daw" then
      for _, desc in ipairs(frame.data.descs) do out[#out + 1] = frame.data.how .. ":" .. desc end
    end
  end
  for _, frame in ipairs(peer.events or {}) do take(frame) end
  peer.events = {}
  for _, line in ipairs(peer:frames(nil)) do take(assert(json.decode(line))) end
  return table.concat(out, ",")
end

return K
