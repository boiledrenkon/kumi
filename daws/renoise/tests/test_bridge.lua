-- The tool as Renoise runs it (main.lua and the payload) against the mock API, with a Lua client
-- playing Kumi's host.
local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local sha256 = T.load_payload_module("sha256")
local test, eq, ok = T.test, T.eq, T.ok

local PROTOCOL = "kumi-daw/1"
local PORT = 9780
local CLIENT_NONCE = "0123456789abcdef0123456789abcdef"

local function write(path, text)
  local f = assert(io.open(path, "wb"))
  f:write(text)
  f:close()
end

local function read(path)
  local f = io.open(path, "rb")
  if not f then return nil end
  local text = f:read("*a")
  f:close()
  return text
end

local function reference(env, fields)
  local value = { schema = "kumi-daw-reference/1", daw = "renoise", secretFile = env.secret_file,
    discoveryDir = env.discovery_dir, port = PORT, logDir = env.log_dir }
  for k, v in pairs(fields or {}) do value[k] = v end
  write(env.reference, assert(json.encode(value)))
end

-- Installs a copy of the tool into a fresh preferences folder and loads it in a fresh mock Renoise.
local function setup(options)
  options = options or {}
  local env = {}
  env.dir = T.temp_dir(options.name)
  env.resource = env.dir .. "/Renoise/V3.5.4"
  env.bundle = env.resource .. "/Scripts/Tools/com.kumi.Bridge.xrnx/"
  local kumi = env.dir .. "/kumi/bridges/renoise"
  env.discovery_dir = env.dir .. "/kumi/discovery"
  env.log_dir = env.dir .. "/kumi/logs"
  env.secret = options.secret or "kumi-test-secret-0123456789abcdefghijklmnopqrstuvwxyz"
  assert(os.execute(string.format("mkdir -p '%s' '%s' '%s' && cp -R '%s'. '%s'", env.bundle, kumi, env.log_dir, T.tool_dir, env.bundle)) == 0)
  env.secret_file = kumi .. "/bridge.secret"
  write(env.secret_file, env.secret .. "\n")
  env.reference = env.bundle .. "bridge-reference.json"
  if not options.no_reference then reference(env, options.reference) end
  mock.reset({ bundle_path = env.bundle, version = options.version, platform = options.platform, fresh_wrappers = options.fresh_wrappers })
  for _, port in ipairs(options.busy_ports or {}) do mock.busy_ports[port] = true end
  if options.song ~= false then mock.song = mock.make_song(options.song) end
  dofile(env.bundle .. "main.lua")
  env.payload = function() return KUMI_BRIDGE.payload end
  mock.fire(mock.tool.tool_finished_loading_observable)
  if mock.song then mock.fire(mock.tool.app_new_document_observable) end
  env.discovery_file = function(pid)
    return env.discovery_dir .. "/renoise-" .. (pid or 4242) .. "-" .. sha256.hex(env.resource):sub(1, 16) .. ".json"
  end
  return env
end

local function no_dialogs()
  eq(#mock.errors, 0, "errors Renoise would show: " .. table.concat(mock.errors, " | "))
  eq(#mock.modals, 0, "modal calls")
end

local function send(peer, value)
  peer:send(assert(json.encode(value)) .. "\n")
end

local function frames(peer)
  return peer:frames(json.decode)
end

local function tick(n)
  mock.advance(50 * (n or 1))
end

local function hmac(env, role, server_nonce, client_nonce)
  return sha256.hmac_hex(env.secret, PROTOCOL .. "\n" .. role .. "\n" .. (client_nonce or CLIENT_NONCE) .. "\n" .. server_nonce)
end

local function handshake(env, peer)
  send(peer, { type = "hello", protocol = PROTOCOL, nonce = CLIENT_NONCE, client = "test/1" })
  local challenge = frames(peer)[1]
  eq(challenge.type, "challenge")
  ok(challenge.nonce:match("^[0-9a-f]+$") and #challenge.nonce == 32, "server nonce")
  eq(challenge.proof, hmac(env, "server", challenge.nonce), "the bridge proves it knows the secret")
  send(peer, { type = "auth", proof = hmac(env, "client", challenge.nonce) })
  local welcome = frames(peer)[1]
  if not welcome then
    tick(4)
    welcome = frames(peer)[1]
  end
  eq(welcome.type, "welcome")
  ok(welcome.session:match("^[0-9a-f]+$") and #welcome.session == 32, "session")
  eq(welcome.maxFrameBytes, 64 * 1024 * 1024)
  eq(welcome.heartbeatMs, 1000)
  return welcome
end

local function connect(env, options)
  local peer = mock.connect(options)
  local welcome = handshake(env, peer)
  return peer, welcome
end

local next_id = 0
local function request(peer, method, params, extra)
  next_id = next_id + 1
  local frame = { type = "request", id = next_id, method = method, params = params }
  for k, v in pairs(extra or {}) do frame[k] = v end
  send(peer, frame)
  local got = frames(peer)
  local reply = got[#got]
  eq(reply.type, "response")
  eq(reply.id, next_id)
  return reply
end

local function events(peer, name)
  local out = {}
  for _, f in ipairs(frames(peer)) do
    if f.type == "event" and (not name or f.name == name) then out[#out + 1] = f end
  end
  return out
end

local function closed_silently(peer)
  tick()
  return peer.closed_by_server and peer:take() == ""
end

local function refused(peer, reason)
  local got = frames(peer)
  eq(#got, 1, "one frame")
  eq(got[1].type, "refused")
  eq(got[1].reason, reason)
  tick()
  ok(peer.closed_by_server, "closed after the refusal")
  return got[1]
end

test("bridge: listens on 127.0.0.1:9780 and writes the discovery file", function()
  local env = setup()
  local server = mock.server()
  eq(server.address, "127.0.0.1")
  eq(server.port, PORT)
  local discovery = json.decode(read(env.discovery_file()))
  ok(discovery, "discovery file " .. env.discovery_file())
  eq(discovery.schema, "kumi-daw-discovery/1")
  eq(discovery.daw, "renoise")
  eq(discovery.pid, 4242)
  eq(discovery.host, "127.0.0.1")
  eq(discovery.port, PORT)
  eq(discovery.protocol, PROTOCOL)
  eq(discovery.version, "1.0.0")
  eq(discovery.dawVersion, "3.5.4")
  eq(discovery.resource, env.resource)
  ok(discovery.project.id and discovery.project.path == nil, "an unsaved song has no path")
  ok(discovery.startedMs > 1.7e12, "unix ms")
  eq(#mock.popen_calls, 1, "the pid is looked up once")
  ok(read(env.log_dir .. "/renoise-bridge.log"):find("listening on 127.0.0.1:9780"), "logged")
  no_dialogs()
end)

test("bridge: the handshake proves both sides and the welcome says who it is", function()
  local env = setup({ song = { name = "Tune Title", file_name = "/songs/tune.xrns", playing = true } })
  local _, welcome = connect(env)
  eq(welcome.daw, "renoise")
  eq(welcome.bridge, "1.0.0")
  eq(welcome.dawVersion, "3.5.4")
  eq(welcome.pid, 4242)
  eq(welcome.resource, env.resource)
  eq(welcome.demo, false)
  eq(welcome.apiVersion, 6.2)
  eq(welcome.project.name, "tune.xrns")
  eq(welcome.project.title, "Tune Title")
  eq(welcome.project.path, "/songs/tune.xrns")
  ok(welcome.project.id:match("^%x+$") and #welcome.project.id == 32)
  eq(table.concat(welcome.methods, ","), "ping,status,renoise.reload")
  eq(table.concat(welcome.events, ","), "project,tracks,selection,transport")
  eq(mock.statuses[#mock.statuses], "Kumi connected")
  no_dialogs()
end)

test("bridge: a wrong proof is refused", function()
  setup()
  local peer = mock.connect()
  send(peer, { type = "hello", protocol = PROTOCOL, nonce = CLIENT_NONCE })
  frames(peer)
  send(peer, { type = "auth", proof = string.rep("0", 64) })
  refused(peer, "auth")
  no_dialogs()
end)

test("bridge: an HTTP request, a TLS hello or a byte order mark gets no reply", function()
  setup()
  for _, first in ipairs({ "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n", "POST /mcp HTTP/1.1\r\n", "PRI * HTTP/2.0\r\n", "\22\3\1\0\165", "\239\187\191{}\n", " {}\n" }) do
    local peer = mock.connect()
    peer:send(first)
    ok(closed_silently(peer), "closed without a reply: " .. string.format("%q", first))
  end
  no_dialogs()
end)

test("bridge: a peer that isn't loopback is closed before anything", function()
  setup()
  local peer = mock.connect({ address = "192.168.1.20" })
  send(peer, { type = "hello", protocol = PROTOCOL, nonce = CLIENT_NONCE })
  ok(closed_silently(peer))
  no_dialogs()
end)

test("bridge: at most four connections wait to authenticate", function()
  setup()
  local peers = {}
  for i = 1, 5 do peers[i] = mock.connect() end
  for i = 1, 4 do eq(#frames(peers[i]), 0, "peer " .. i .. " waits") end
  refused(peers[5], "connections")
  for i = 1, 4 do ok(not peers[i].closed_by_server, "peer " .. i .. " kept") end
  no_dialogs()
end)

test("bridge: at most four authenticated connections", function()
  local env = setup()
  for _ = 1, 4 do connect(env) end
  refused(mock.connect(), "connections")
  no_dialogs()
end)

test("bridge: the handshake must finish within 3 seconds", function()
  local env = setup()
  local slow = mock.connect()
  send(slow, { type = "hello", protocol = PROTOCOL, nonce = CLIENT_NONCE })
  frames(slow)
  local fine = connect(env)
  mock.advance(2900)
  ok(not slow.closed_by_server)
  mock.advance(200)
  local got = frames(slow)
  eq(got[1].type, "refused")
  eq(got[1].reason, "timeout")
  tick()
  ok(slow.closed_by_server, "closed after the deadline")
  ok(not fine.closed_by_server, "an authenticated connection has no deadline")
  no_dialogs()
end)

test("bridge: a peer that keeps sending is timed out at 3 seconds, timer or not", function()
  setup()
  local slow = mock.connect()
  send(slow, { type = "hello", protocol = PROTOCOL, nonce = CLIENT_NONCE })
  frames(slow)
  for _ = 1, 29 do
    mock.time = mock.time + 100 -- the clock moves, no timer runs
    slow:send("\n")
  end
  eq(#frames(slow), 0, "still waiting at 2.9 s")
  mock.time = mock.time + 100
  slow:send("\n")
  refused(slow, "timeout")
  no_dialogs()
end)

test("bridge: blank lines before auth count toward the 1024-byte limit", function()
  setup()
  local peer = mock.connect()
  send(peer, { type = "hello", protocol = PROTOCOL, nonce = CLIENT_NONCE })
  frames(peer)
  peer:send(string.rep("\n", 1024))
  eq(#frames(peer), 0)
  peer:send("\n")
  refused(peer, "limit")
  no_dialogs()
end)

test("bridge: a first line over 1024 bytes is refused", function()
  setup()
  local peer = mock.connect()
  peer:send("{" .. string.rep(" ", 1100))
  refused(peer, "limit")
  no_dialogs()
end)

test("bridge: bad hellos are refused", function()
  setup()
  local cases = {
    { { type = "hello", protocol = "kumi-daw/2", nonce = CLIENT_NONCE }, "protocol" },
    { { type = "hello", protocol = PROTOCOL, nonce = "short" }, "handshake" },
    { { type = "hello", protocol = PROTOCOL, nonce = CLIENT_NONCE:upper() }, "handshake" },
    { { type = "auth", proof = "x" }, "handshake" },
    { { type = "request", id = 1, method = "status" }, "handshake" },
  }
  for _, case in ipairs(cases) do
    local peer = mock.connect()
    send(peer, case[1])
    local frame = refused(peer, case[2])
    if case[2] == "protocol" then eq(frame.protocol, PROTOCOL) end
  end
  no_dialogs()
end)

test("bridge: requests after auth", function()
  local env = setup()
  local peer = connect(env)
  local reply = request(peer, "status")
  eq(reply.ok, true)
  eq(reply.result.daw, "renoise")
  eq(reply.result.busy, false)
  eq(reply.result.pid, 4242)
  eq(reply.result.methods, nil, "status is the identity plus busy")
  reply = request(peer, "ping", {})
  eq(reply.ok, true)
  eq(next(reply.result), nil)
  reply = request(peer, "nope")
  eq(reply.ok, false)
  eq(reply.error.code, "unknown-method")
  send(peer, { type = "later", anything = true })
  eq(#frames(peer), 0, "unknown frame types are ignored")
  reply = request(peer, "status", nil, { deadlineMs = 1000 })
  eq(reply.ok, false)
  eq(reply.error.code, "deadline")
  ok(not peer.closed_by_server)
  no_dialogs()
end)

test("bridge: after auth a line that isn't a frame closes without a reply", function()
  local env = setup()
  for _, line in ipairs({ "not json\n", '{"type":"request","id":0,"method":"ping"}\n', "[1]\n", '{"a":"\255"}\n' }) do
    local peer = connect(env)
    peer:send(line)
    ok(closed_silently(peer), string.format("%q", line))
  end
  no_dialogs()
end)

test("bridge: selection changes are one event per tick, numbered per connection", function()
  local env = setup()
  local peer = connect(env)
  local song = mock.song
  song.selected_track_index = 2
  song.selected_track_index = 3
  song.selected_instrument_index = 4
  tick()
  local got = events(peer, "selection")
  eq(#got, 1)
  eq(got[1].seq, 1)
  eq(got[1].data.track, 3)
  eq(got[1].data.instrument, 4)
  eq(got[1].data.pattern, 1)
  song.selected_track_index = 4
  song.selected_track_index = 3
  tick()
  eq(#events(peer, "selection"), 0, "back to the same selection is no event")
  local late = connect(env)
  song.selected_pattern_index = 7
  tick()
  got = events(peer, "selection")
  eq(got[1].seq, 2)
  eq(got[1].data.pattern, 7)
  eq(events(late, "selection")[1].seq, 1, "a new connection counts from 1")
  no_dialogs()
end)

test("bridge: transport changes are events", function()
  local env = setup()
  local peer = connect(env)
  mock.song.transport.playing = true
  mock.song.transport.bpm = 140
  tick()
  local got = events(peer, "transport")
  eq(#got, 1)
  eq(got[1].data.playing, true)
  eq(got[1].data.bpm, 140)
  eq(got[1].data.lpb, 4)
  no_dialogs()
end)

test("bridge: a new song detaches the old observers and attaches new ones", function()
  local env = setup({ song = { name = "Old" } })
  local peer = connect(env)
  local old = mock.song
  eq(mock.notifier_count(old.selected_track_index_observable), 1)
  mock.load_song({ name = "New", file_name = "/s/new.xrns" })
  eq(mock.notifier_count(old.selected_track_index_observable), 0, "old song detached")
  eq(mock.notifier_count(old.transport.playing_observable), 0)
  eq(mock.notifier_count(mock.song.selected_track_index_observable), 1, "new song attached once")
  local got = events(peer, "project")
  eq(#got, 2)
  eq(got[1].data, json.null, "the old song closed")
  eq(got[2].data.name, "new.xrns")
  eq(got[2].data.path, "/s/new.xrns")
  eq(got[2].seq, got[1].seq + 1)
  old.selected_track_index = 5
  tick()
  eq(#events(peer, "selection"), 0, "the old song is silent")
  eq(json.decode(read(env.discovery_file())).project.path, "/s/new.xrns", "discovery follows the song")
  no_dialogs()
end)

test("bridge: song ids are stored in tool_data on save and survive reopening", function()
  local env = setup()
  local peer, welcome = connect(env)
  local id = welcome.project.id
  mock.save_song("/s/saved.xrns")
  eq(json.decode(mock.song.tool_data).songId, id, "tool_data holds the id")
  local saved = events(peer, "project")[1]
  eq(saved.data.id, id)
  eq(saved.data.path, "/s/saved.xrns")
  mock.load_song({ file_name = "/s/saved.xrns", tool_data = mock.song.tool_data })
  eq(events(peer, "project")[2].data.id, id, "the same id after reopening")
  mock.load_song({ name = "Other" })
  ok(events(peer, "project")[2].data.id ~= id, "another song, another id")
  no_dialogs()
end)

test("bridge: a saved song without a stored id keeps one id across reopening", function()
  local env = setup({ song = { file_name = "/s/older.xrns" } })
  local _, welcome = connect(env)
  local id = welcome.project.id
  mock.load_song({ file_name = "/s/older.xrns" })
  local peer = connect(env)
  eq(request(peer, "status").result.project.id, id)
  no_dialogs()
end)

test("bridge: other keys in tool_data are kept", function()
  local env = setup({ song = { tool_data = '{"note":"keep me"}' } })
  connect(env)
  mock.save_song("/s/x.xrns")
  local data = json.decode(mock.song.tool_data)
  eq(data.note, "keep me")
  ok(data.songId)
  no_dialogs()
end)

test("bridge: heartbeats about once a second, only after auth", function()
  local env = setup()
  local waiting = mock.connect()
  local peer = connect(env)
  mock.advance(1000)
  local beats = {}
  for _, f in ipairs(frames(peer)) do
    if f.type == "heartbeat" then beats[#beats + 1] = f end
  end
  eq(#beats, 1)
  ok(beats[1].ms >= 1000 and beats[1].ms <= 1100, "ms since the connection opened")
  mock.advance(3000)
  local count = 0
  for _, f in ipairs(frames(peer)) do
    if f.type == "heartbeat" then count = count + 1 end
  end
  eq(count, 3)
  ok(not waiting:take():find("heartbeat"), "no heartbeats before auth")
  no_dialogs()
end)

test("bridge: unloading says bye, closes everything and removes the discovery file", function()
  local env = setup()
  local peer = connect(env)
  local song = mock.song
  ok(read(env.discovery_file()))
  mock.fire(mock.tool.tool_will_unload_observable)
  local got = frames(peer)
  eq(got[#got].type, "bye")
  eq(got[#got].reason, "shutdown", "disabled or reloaded")
  eq(mock.server(), nil, "server closed")
  ok(peer.closed_by_server, "connection closed")
  eq(read(env.discovery_file()), nil, "discovery removed")
  eq(#mock.timers, 0, "timer removed")
  eq(mock.notifier_count(song.selected_track_index_observable), 0, "observers detached")
  ok(read(env.log_dir .. "/renoise-bridge.log"):find("stopped"), "logged")
  no_dialogs()
end)

test("bridge: quitting Renoise says bye with quit", function()
  local env = setup()
  local peer = connect(env)
  mock.fire(mock.tool.app_release_document_observable)
  mock.fire(mock.tool.tool_will_unload_observable)
  local got = frames(peer)
  eq(got[#got].type, "bye")
  eq(got[#got].reason, "quit")
  no_dialogs()
end)

test("bridge: track count changes are events", function()
  local env = setup()
  local peer = connect(env)
  local fields = mock.impl(mock.song)
  fields.sequencer_track_count = 3
  fields.send_track_count = 1
  mock.fire(mock.song.tracks_observable)
  mock.fire(mock.song.tracks_observable)
  tick()
  local got = events(peer, "tracks")
  eq(#got, 1)
  eq(got[1].data.count, 5, "sequencer and send tracks plus the master")
  no_dialogs()
end)

test("bridge: waits for setup, then starts without a restart", function()
  local env = setup({ no_reference = true })
  eq(mock.server(), nil)
  eq(#mock.statuses, 1, "says so once")
  ok(mock.statuses[1]:find("kumi setup"))
  mock.advance(2000)
  eq(#mock.statuses, 1, "not repeated")
  reference(env)
  mock.advance(6000)
  ok(mock.server(), "listening once set up")
  connect(env)
  no_dialogs()
end)

test("bridge: a reference for another DAW or schema isn't used", function()
  setup({ reference = { daw = "reaper" } })
  eq(mock.server(), nil)
  setup({ reference = { schema = "kumi-daw-reference/2" } })
  eq(mock.server(), nil)
  no_dialogs()
end)

test("bridge: busy ports move along Renoise's range and wrap", function()
  local env = setup({ busy_ports = { PORT, PORT + 1 } })
  eq(mock.server().port, PORT + 2)
  eq(json.decode(read(env.discovery_file())).port, PORT + 2)
  connect(env)
  setup({ reference = { port = 9789 }, busy_ports = { 9789 } })
  eq(mock.server().port, 9780, "wraps to the range's start")
  setup({ reference = { port = 9765 } })
  eq(mock.server().port, 9780, "a port outside the range is ignored")
  no_dialogs()
end)

test("bridge: the server is recreated after it dies", function()
  local env = setup()
  local first = mock.server()
  mock.socket_error("connection reset")
  ok(mock.server() == first, "an error while it runs keeps the server")
  first.running = false
  first.open = false
  mock.advance(1500)
  local second = mock.server()
  ok(second and second ~= first, "a new server")
  eq(second.port, PORT)
  connect(env)
  no_dialogs()
end)

test("bridge: renoise.reload swaps the payload and keeps the port and the connection", function()
  local env = setup()
  local peer = connect(env)
  mock.song.selected_track_index = 2
  tick()
  eq(events(peer, "selection")[1].seq, 1)
  local server = mock.server()
  local old_payload = env.payload()
  eq(request(peer, "renoise.reload").result.reloading, true)
  tick()
  ok(env.payload() ~= old_payload, "new payload")
  ok(mock.server() == server and server.running, "same server")
  eq(request(peer, "status").ok, true)
  eq(mock.notifier_count(mock.song.selected_track_index_observable), 1, "attached once")
  mock.song.selected_track_index = 9
  tick()
  local selection = events(peer, "selection")[1]
  eq(selection.data.track, 9)
  eq(selection.seq, 2, "the connection's numbering goes on")
  ok(read(env.log_dir .. "/renoise-bridge.log"):find("reloaded bridge"), "the new payload logs")
  eq(#mock.popen_calls, 1, "the pid isn't looked up again")
  no_dialogs()
end)

test("bridge: an error while welcoming closes that connection, nothing else", function()
  local env = setup()
  local good = connect(env)
  local fields = mock.impl(mock.song)
  local saved = fields.name
  fields.name = nil
  local peer = mock.connect()
  send(peer, { type = "hello", protocol = PROTOCOL, nonce = CLIENT_NONCE })
  local challenge = frames(peer)[1]
  send(peer, { type = "auth", proof = hmac(env, "client", challenge.nonce) })
  tick()
  ok(peer.closed_by_server, "closed")
  fields.name = saved
  eq(request(good, "status").ok, true, "the other connection still answers")
  no_dialogs()
end)

test("bridge: a broken payload on reload keeps the running one", function()
  local env = setup()
  local peer = connect(env)
  local old_payload = env.payload()
  write(env.bundle .. "payload/status.lua", "this is not lua")
  request(peer, "renoise.reload")
  tick()
  ok(env.payload() == old_payload, "kept")
  eq(request(peer, "status").ok, true)
  ok(read(env.log_dir .. "/renoise-bridge.log"):find("reload failed"), "logged")
  no_dialogs()
end)

test("bridge: an error reading the song is logged, never shown", function()
  local env = setup()
  local peer = connect(env)
  local playing = mock.song.transport.playing_observable
  mock.impl(mock.song).transport = nil
  mock.fire(playing)
  tick(25)
  ok(read(env.log_dir .. "/renoise-bridge.log"):find("not mocked"), "logged")
  ok(not peer.closed_by_server)
  no_dialogs()
end)

test("bridge: a secret file others can read refuses connections", function()
  local env = setup()
  mock.stat_modes[env.secret_file] = 33188
  local peer = mock.connect()
  send(peer, { type = "hello", protocol = PROTOCOL, nonce = CLIENT_NONCE })
  ok(closed_silently(peer))
  tick(20)
  ok(read(env.log_dir .. "/renoise-bridge.log"):find("other accounts can read it"), "logged")
  no_dialogs()
end)

test("bridge: an invalid secret refuses connections", function()
  setup({ secret = "short" })
  local peer = mock.connect()
  send(peer, { type = "hello", protocol = PROTOCOL, nonce = CLIENT_NONCE })
  ok(closed_silently(peer))
  no_dialogs()
end)

test("bridge: the demo is detected", function()
  local env = setup({ version = "3.5.4 Demo" })
  local _, welcome = connect(env)
  eq(welcome.demo, true)
  eq(welcome.dawVersion, "3.5.4 Demo")
  no_dialogs()
end)

test("bridge: works with no song yet, then attaches when one arrives", function()
  local env = setup({ song = false })
  local peer, welcome = connect(env)
  eq(welcome.project, json.null)
  mock.load_song({ name = "First" })
  eq(events(peer, "project")[1].data.name, "First")
  mock.song.selected_track_index = 2
  tick()
  eq(#events(peer, "selection"), 1)
  no_dialogs()
end)

test("bridge: a big frame goes out in slices over ticks", function()
  local env = setup({ song = { name = string.rep("n", 600 * 1024) } })
  local peer = connect(env)
  send(peer, { type = "request", id = 99, method = "status" })
  tick(2)
  local got = frames(peer)
  eq(got[#got].id, 99)
  eq(#got[#got].result.project.name, 600 * 1024)
  ok(peer.sent_calls > 5, "sent in pieces")
  no_dialogs()
end)

test("bridge: Windows starts no process", function()
  local env = setup({ platform = "WINDOWS" })
  eq(#mock.popen_calls, 0, "no process started")
  ok(read(env.discovery_file(0)), "discovery without a pid")
  no_dialogs()
end)

test("bridge: one socket wrapped in several Lua objects is one connection", function()
  local env = setup({ fresh_wrappers = true })
  local peer = connect(env)
  eq(request(peer, "status").ok, true)
  local count = 0
  for _ in pairs(KUMI_BRIDGE.connections) do count = count + 1 end
  eq(count, 1)
  no_dialogs()
end)
