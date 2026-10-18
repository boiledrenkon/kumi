-- A host held back by credit (a suspended terminal, a window smaller than an answer): the producer's work in Renoise must
-- not retire it, and one that is retired is told so without a torn line.
local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local test, eq, ok = T.test, T.eq, T.ok
local H = T.harness

local function record_of(peer) return KUMI_BRIDGE.connections[peer.address .. ":" .. peer.port] end

-- A host that has used up the welcome's credit: asks for an answer of `size` bytes and stops reading.
local function paused_host(size)
  local env = H.setup({ song = { name = "S", tracks = { { name = "A" }, { name = "B" } } } })
  local peer = H.connect(env)
  H.frames(peer)
  env.payload().handlers["test.big"] = function() return { blob = string.rep("z", size) } end
  mock.socket_buffer_bytes = 1024 * 1024
  peer:stop_reading()
  H.send(peer, { type = "request", id = 7, method = "test.big", params = {} })
  H.tick(2)
  return env, peer, record_of(peer)
end

-- What a host does when it reads again: takes what is there, gives credit back for those bytes, until nothing more comes.
-- Returns every whole line and the bytes left of a line that never ended.
local function read_everything(peer, rounds)
  peer:resume_reading()
  local text = ""
  for _ = 1, rounds or 60 do
    repeat
      local taken = peer:take()
      text = text .. taken
      if #taken > 0 then H.send(peer, { type = "credit", bytes = #taken }) end
    until #taken == 0
    H.tick()
  end
  -- (split with find: a pattern would take quadratic time over one line of 200 KB)
  local lines, at = {}, 1
  while true do
    local nl = text:find("\n", at, true)
    if not nl then break end
    lines[#lines + 1] = text:sub(at, nl - 1)
    at = nl + 1
  end
  return lines, text:sub(at)
end

local function frame_types(lines)
  local out = {}
  for _, line in ipairs(lines) do
    local frame = json.decode(line)
    ok(frame, "every line is whole JSON: " .. line:sub(1, 60))
    out[#out + 1] = frame
  end
  return out
end

test("retire: a producer's selection changes don't retire a host held back for credit, and the latest one follows the answer", function()
  local env, peer, rec = paused_host(200 * 1024)
  ok(#rec.outbox == 1 and rec.outbox[1].offset > 1, "a line part sent is waiting for credit")
  for i = 1, 300 do
    mock.song.selected_track_index = (i % 2) + 1
    H.tick()
  end
  ok(not rec.closing, "not retired")
  ok((rec.unsolicited or 0) < 4096, "the queue holds the latest event, not 300: " .. tostring(rec.unsolicited))
  eq(#rec.outbox, 1)
  local lines, partial = read_everything(peer)
  eq(partial, "", "no line left unfinished")
  local frames = frame_types(lines)
  local answer, selections, last
  for _, f in ipairs(frames) do
    if f.type == "response" and f.id == 7 then answer = f end
    if f.type == "event" and f.name == "selection" then selections = (selections or 0) + 1; last = f end
  end
  ok(answer and #answer.result.blob == 200 * 1024, "the answer came whole")
  eq(selections, 1, "one selection event, the latest")
  eq(last.data.track, (300 % 2) + 1)
  ok(not rec.closing)
  H.no_dialogs()
end)

test("retire: events keep their order: what was kept goes out before a track shift that came after it", function()
  local env, peer, rec = paused_host(200 * 1024)
  mock.song.selected_track_index = 2
  H.tick()
  mock.ops.insert_track(1, { name = "New" })
  mock.song.selected_track_index = 1
  H.tick()
  local lines = read_everything(peer)
  local names = {}
  for _, f in ipairs(frame_types(lines)) do
    if f.type == "event" then names[#names + 1] = f.name .. ":" .. f.seq end
  end
  eq(table.concat(names, ","), "selection:1,tracks:2,selection:3", "the numbers have no gap")
  H.no_dialogs()
end)

test("retire: 64 KiB of unasked output retires a host, but the line part sent is finished and then the bye comes", function()
  local env, peer, rec = paused_host(200 * 1024)
  local head = rec.outbox[1]
  local size = #head.text
  for _ = 1, 700 do mock.ops.insert_track(1, { name = "x" }) end
  ok(rec.closing, "retired past 64 KiB of events")
  eq(rec.outbox[1], head, "the line part sent is still the head")
  eq(#rec.outbox, 2, "and the bye is behind it")
  local lines, partial = read_everything(peer)
  eq(partial, "", "no line left unfinished")
  local frames = frame_types(lines)
  eq(frames[#frames].type, "bye", "the last thing the host gets")
  eq(frames[#frames].reason, "overload")
  local answers = 0
  for _, f in ipairs(frames) do if f.type == "response" and f.id == 7 then answers = answers + 1 end end
  eq(answers, 1, "the answer it was reading came whole before the bye")
  eq(#rec.outbox, 0)
  H.no_dialogs()
end)

test("retire: more than 64 answers waiting retires the host with the same care", function()
  local env, peer, rec = paused_host(200 * 1024)
  for i = 1, 70 do H.send(peer, { type = "request", id = 100 + i, method = "ping", params = {} }) end
  H.tick(2)
  ok(rec.closing, "retired")
  local lines, partial = read_everything(peer)
  eq(partial, "")
  local frames = frame_types(lines)
  eq(frames[#frames].type, "bye")
  ok(#frames <= 3, "the answer and the bye, not the pings: " .. #frames)
  H.no_dialogs()
end)

test("retire: a retired host that is still owed output takes credit frames and nothing else", function()
  local env, peer, rec = paused_host(200 * 1024)
  for _ = 1, 700 do mock.ops.insert_track(1, { name = "x" }) end
  ok(rec.closing)
  peer:resume_reading()
  peer:take()
  local before = rec.credit
  H.send(peer, { type = "request", id = 55, method = "ping", params = {} })
  H.tick(2)
  eq(#rec.requests, 0, "a request is not queued")
  H.send(peer, { type = "credit", bytes = 1024 * 1024 })
  H.tick(2)
  ok(rec.credit ~= before or #rec.outbox == 0, "the grant was applied")
  local text = peer:take()
  ok(not text:find('"id":55', 1, true), "and the request was not answered")
  H.no_dialogs()
end)

test("retire: a retired connection's read is dropped, not run to the end", function()
  local env, peer, rec = paused_host(1)
  -- a read that takes ten thousand slices
  env.payload().handlers["test.slow"] = function()
    return { kumi_job = function(job)
      for _ = 1, 10000 do job.slice() end
      return {}
    end }
  end
  local timers = #mock.timers
  mock.clock_step_ms = 1
  H.send(peer, { type = "request", id = 8, method = "test.slow", params = {} })
  eq(#mock.timers, timers + 1, "a read is pending")
  -- 260 requests wait behind it (the cap is 256)
  for i = 1, 260 do H.send(peer, { type = "request", id = 200 + i, method = "ping", params = {} }) end
  mock.clock_step_ms = 0
  ok(rec.closing, "retired for too many requests waiting")
  eq(#mock.timers, timers, "and its read was dropped with it")
  eq(rec.job, nil)
  H.no_dialogs()
end)
