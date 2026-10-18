local T = ...
local mock = T.mock
local json = T.load_payload_module("json")
local sha256 = T.load_payload_module("sha256")
local test, eq, ok = T.test, T.eq, T.ok

local PROTOCOL = "kumi-daw/1"
local PORT = 19876

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

-- Installs a copy of the tool into a fresh preferences folder and loads it in a fresh mock Renoise.
local function setup(options)
  options = options or {}
  local env = {}
  env.dir = T.temp_dir(options.name)
  env.bundle = env.dir .. "/Renoise/V3.5.4/Scripts/Tools/com.kumi.Bridge.xrnx/"
  env.state_dir = env.dir .. "/state"
  env.secret = options.secret or "0123456789abcdef0123456789abcdef-kumi"
  assert(os.execute(string.format("mkdir -p '%s' '%s' && cp -R '%s'. '%s'", env.bundle, env.state_dir, T.tool_dir, env.bundle)) == 0)
  env.secret_file = env.state_dir .. "/renoise.secret"
  write(env.secret_file, env.secret .. "\n")
  env.reference = env.bundle .. "bridge-reference.json"
  if not options.no_reference then
    write(env.reference, assert(json.encode({ version = 1, port = options.port or PORT, secretFile = env.secret_file, stateDir = env.state_dir })))
  end
  mock.reset({ bundle_path = env.bundle, version = options.version, platform = options.platform, fresh_wrappers = options.fresh_wrappers })
  for _, port in ipairs(options.busy_ports or {}) do mock.busy_ports[port] = true end
  if options.song ~= false then mock.song = mock.make_song(options.song) end
  dofile(env.bundle .. "main.lua")
  env.payload = function() return KUMI_BRIDGE.payload end
  mock.fire(mock.tool.tool_finished_loading_observable)
  if mock.song then mock.fire(mock.tool.app_new_document_observable) end
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

local function handshake(env, peer, client_nonce)
  client_nonce = client_nonce or string.rep("c1", 16)
  send(peer, { type = "hello", protocol = PROTOCOL, clientNonce = client_nonce })
  local challenge = frames(peer)[1]
  eq(challenge.type, "challenge")
  eq(challenge.protocol, PROTOCOL)
  eq(challenge.daw, "renoise")
  ok(challenge.serverNonce:match("^%x+$") and #challenge.serverNonce == 32, "server nonce")
  send(peer, { type = "auth", mac = sha256.hmac_hex(env.secret, PROTOCOL .. " client\n" .. challenge.serverNonce .. "\n" .. client_nonce) })
  local welcome = frames(peer)[1]
  if not welcome then
    tick(4)
    welcome = frames(peer)[1]
  end
  eq(welcome.type, "welcome")
  eq(welcome.mac, sha256.hmac_hex(env.secret, PROTOCOL .. " server\n" .. client_nonce .. "\n" .. challenge.serverNonce), "server proof")
  return welcome
end

local function connect(env, options)
  local peer = mock.connect(options)
  local welcome = handshake(env, peer)
  return peer, welcome
end

local function request(peer, method, params, id)
  send(peer, { id = id or "r1", method = method, params = params })
  local got = frames(peer)
  return got[#got]
end

local function events(peer, name)
  local out = {}
  for _, f in ipairs(frames(peer)) do
    if f.type == "event" and (not name or f.event == name) then out[#out + 1] = f end
  end
  return out
end

test("bridge: listens on 127.0.0.1 at the configured port and writes the discovery file", function()
  local env = setup()
  local server = mock.server()
  ok(server, "a server")
  eq(server.address, "127.0.0.1")
  eq(server.port, PORT)
  local discovery = json.decode(read(env.state_dir .. "/discovery/renoise-4242.json"))
  ok(discovery, "discovery file")
  eq(discovery.daw, "renoise")
  eq(discovery.port, PORT)
  eq(discovery.pid, 4242)
  eq(discovery.protocol, PROTOCOL)
  eq(discovery.resourcePath, env.dir .. "/Renoise/V3.5.4")
  eq(discovery.project.unsaved, true)
  eq(#mock.popen_calls, 1, "the pid is looked up once")
  no_dialogs()
end)

test("bridge: handshake proves both sides and the welcome carries hello", function()
  local env = setup({ song = { name = "Tune", file_name = "/songs/tune.xrns", playing = true } })
  local _, welcome = connect(env)
  local hello = welcome.hello
  eq(hello.daw, "renoise")
  eq(hello.dawVersion, "3.5.4")
  eq(hello.apiVersion, 6.2)
  eq(hello.demo, false)
  eq(hello.bridgeVersion, "1.0.0")
  eq(hello.port, PORT)
  eq(hello.project.name, "Tune")
  eq(hello.project.path, "/songs/tune.xrns")
  eq(hello.project.unsaved, false)
  ok(hello.project.id:match("^%x+$") and #hello.project.id == 32)
  eq(hello.playing, true)
  eq(mock.statuses[#mock.statuses], "Kumi connected")
  no_dialogs()
end)

test("bridge: a wrong MAC is refused and the connection closed", function()
  local env = setup()
  local peer = mock.connect()
  send(peer, { type = "hello", protocol = PROTOCOL, clientNonce = string.rep("ab", 16) })
  frames(peer)
  send(peer, { type = "auth", mac = string.rep("0", 64) })
  local got = frames(peer)
  eq(got[1].type, "error")
  eq(got[1].code, "auth")
  tick()
  ok(peer.closed_by_server, "closed")
  eq(#frames(peer), 0, "nothing after a failed auth")
  ok(env)
  no_dialogs()
end)

test("bridge: a first line that looks like HTTP gets no answer", function()
  setup()
  for _, first in ipairs({ "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n", "POST /mcp HTTP/1.1\r\n", "OPTIONS * HTTP/1.1\r\n", "\22\3\1\0\165" }) do
    local peer = mock.connect()
    peer:send(first)
    tick()
    eq(peer:take(), "", "no reply to " .. string.format("%q", first))
    ok(peer.closed_by_server, "closed")
  end
  no_dialogs()
end)

test("bridge: a peer that isn't loopback is refused", function()
  setup()
  local peer = mock.connect({ address = "192.168.1.20" })
  send(peer, { type = "hello", protocol = PROTOCOL, clientNonce = string.rep("ab", 16) })
  tick()
  eq(peer:take(), "")
  ok(peer.closed_by_server)
  no_dialogs()
end)

test("bridge: at most four connections wait to authenticate", function()
  setup()
  local peers = {}
  for i = 1, 5 do peers[i] = mock.connect() end
  tick()
  for i = 1, 4 do ok(not peers[i].closed_by_server, "peer " .. i .. " kept") end
  ok(peers[5].closed_by_server, "the fifth is closed")
  no_dialogs()
end)

test("bridge: a connection that doesn't authenticate in time is closed", function()
  local env = setup()
  local slow = mock.connect()
  send(slow, { type = "hello", protocol = PROTOCOL, clientNonce = string.rep("ab", 16) })
  local fine = connect(env)
  mock.advance(4000)
  ok(not slow.closed_by_server)
  mock.advance(3000)
  ok(slow.closed_by_server, "closed after the deadline")
  ok(not fine.closed_by_server, "an authenticated connection stays")
  no_dialogs()
end)

test("bridge: an oversized first line is refused before auth", function()
  setup()
  local peer = mock.connect()
  peer:send("{" .. string.rep(" ", 5000))
  tick()
  ok(peer.closed_by_server)
  eq(peer:take(), "")
  no_dialogs()
end)

test("bridge: a bad hello is refused", function()
  setup()
  for _, hello in ipairs({
    { type = "hello", protocol = "renoise/v0", clientNonce = string.rep("ab", 16) },
    { type = "hello", protocol = PROTOCOL, clientNonce = "short" },
    { type = "hello", protocol = PROTOCOL, clientNonce = string.rep("AB", 16) },
    { type = "auth", mac = "x" },
    { id = "1", method = "status" },
  }) do
    local peer = mock.connect()
    send(peer, hello)
    local got = frames(peer)
    eq(got[1].type, "error")
    tick()
    ok(peer.closed_by_server)
  end
  no_dialogs()
end)

test("bridge: requests after auth", function()
  local env = setup()
  local peer = connect(env)
  local reply = request(peer, "status", nil, "a-1")
  eq(reply.id, "a-1")
  eq(reply.ok, true)
  eq(reply.result.daw, "renoise")
  reply = request(peer, "ping", {}, "a-2")
  eq(reply.result.pong, true)
  reply = request(peer, "nope", nil, "a-3")
  eq(reply.ok, false)
  eq(reply.error.code, "unknown_method")
  send(peer, { id = "has space", method = "status" })
  eq(frames(peer)[1].code, "invalid")
  send(peer, { id = "a-4" })
  eq(frames(peer)[1].error.code, "invalid")
  send(peer, { id = "a-5", method = "status", params = "x" })
  eq(frames(peer)[1].error.code, "invalid")
  peer:send("not json\n")
  eq(frames(peer)[1].code, "invalid")
  ok(not peer.closed_by_server, "bad frames after auth don't close")
  no_dialogs()
end)

test("bridge: selection changes become one event per tick, deduplicated", function()
  local env = setup()
  local peer = connect(env)
  local song = mock.song
  song.selected_track_index = 2
  song.selected_track_index = 3
  song.selected_instrument_index = 4
  tick()
  local got = events(peer, "selection.changed")
  eq(#got, 1)
  eq(got[1].data.track, 3)
  eq(got[1].data.instrument, 4)
  eq(got[1].data.pattern, 1)
  local first_seq = got[1].seq
  song.selected_track_index = 4
  song.selected_track_index = 3
  tick()
  eq(#events(peer, "selection.changed"), 0, "back to the same selection is no event")
  song.selected_pattern_index = 7
  tick()
  got = events(peer, "selection.changed")
  eq(got[1].data.pattern, 7)
  eq(got[1].seq, first_seq + 1)
  no_dialogs()
end)

test("bridge: transport changes become events", function()
  local env = setup()
  local peer = connect(env)
  mock.song.transport.playing = true
  mock.song.transport.bpm = 140
  tick()
  local got = events(peer, "transport.changed")
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
  local got = events(peer)
  eq(got[1].event, "project.released")
  eq(got[1].data.name, "Old")
  eq(got[2].event, "project.changed")
  eq(got[2].data.name, "New")
  eq(got[2].data.path, "/s/new.xrns")
  ok(got[2].seq > got[1].seq)
  old.selected_track_index = 5
  tick()
  eq(#events(peer, "selection.changed"), 0, "the old song is silent")
  local discovery = json.decode(read(env.state_dir .. "/discovery/renoise-4242.json"))
  eq(discovery.project.path, "/s/new.xrns")
  no_dialogs()
end)

test("bridge: the song id is stored in tool_data on save and read back", function()
  local env = setup()
  local peer, welcome = connect(env)
  local id = welcome.hello.project.id
  mock.save_song("/s/saved.xrns")
  eq(json.decode(mock.song.tool_data).songId, id, "tool_data holds the id")
  local saved = events(peer, "project.saved")[1]
  eq(saved.data.id, id)
  eq(saved.data.path, "/s/saved.xrns")
  eq(saved.data.unsaved, false)
  mock.load_song({ name = "Reopened", file_name = "/s/saved.xrns", tool_data = mock.song.tool_data })
  local changed = events(peer, "project.changed")[1]
  eq(changed.data.id, id, "same id after reopening")
  mock.load_song({ name = "Other" })
  ok(events(peer, "project.changed")[1].data.id ~= id, "another song, another id")
  no_dialogs()
end)

test("bridge: tool_data from other versions is kept when the id is added", function()
  local env = setup({ song = { tool_data = '{"note":"keep me"}' } })
  connect(env)
  mock.save_song("/s/x.xrns")
  local data = json.decode(mock.song.tool_data)
  eq(data.note, "keep me")
  ok(data.songId)
  no_dialogs()
end)

test("bridge: heartbeats about once a second", function()
  local env = setup()
  local peer = connect(env)
  mock.song.selected_track_index = 2
  mock.advance(1000)
  local beats, last_event = {}, nil
  for _, f in ipairs(frames(peer)) do
    if f.type == "heartbeat" then beats[#beats + 1] = f end
    if f.type == "event" then last_event = f end
  end
  eq(#beats, 1)
  eq(beats[1].lastEventSeq, last_event.seq)
  eq(beats[1].busy.rendering, false)
  mock.advance(3000)
  local count = 0
  for _, f in ipairs(frames(peer)) do
    if f.type == "heartbeat" then count = count + 1 end
  end
  eq(count, 3)
  no_dialogs()
end)

test("bridge: unloading closes everything and removes the discovery file", function()
  local env = setup()
  local peer = connect(env)
  local song = mock.song
  local discovery = env.state_dir .. "/discovery/renoise-4242.json"
  ok(read(discovery))
  mock.fire(mock.tool.tool_will_unload_observable)
  eq(mock.server(), nil, "server closed")
  ok(peer.closed_by_server, "connection closed")
  eq(read(discovery), nil, "discovery removed")
  eq(#mock.timers, 0, "timer removed")
  eq(mock.notifier_count(song.selected_track_index_observable), 0, "observers detached")
  ok(read(env.state_dir .. "/renoise-bridge.log"):find("stopped"), "logged")
  no_dialogs()
end)

test("bridge: waits for setup, then starts without a restart", function()
  local env = setup({ no_reference = true })
  eq(mock.server(), nil)
  eq(#mock.statuses, 1, "says so once")
  ok(mock.statuses[1]:find("kumi setup"))
  mock.advance(2000)
  eq(#mock.statuses, 1, "not repeated")
  write(env.reference, assert(json.encode({ version = 1, port = PORT, secretFile = env.secret_file, stateDir = env.state_dir })))
  mock.advance(6000)
  ok(mock.server(), "listening once set up")
  connect(env)
  no_dialogs()
end)

test("bridge: a busy port moves to the next one", function()
  local env = setup({ busy_ports = { PORT, PORT + 1 } })
  eq(mock.server().port, PORT + 2)
  eq(json.decode(read(env.state_dir .. "/discovery/renoise-4242.json")).port, PORT + 2)
  local _, welcome = connect(env)
  eq(welcome.hello.port, PORT + 2)
  no_dialogs()
end)

test("bridge: the server is recreated after it dies", function()
  local env = setup()
  local first = mock.server()
  first.running = false
  mock.socket_error("connection reset")
  first.running = true
  mock.socket_error("connection reset")
  ok(mock.server() == first, "an error while running keeps the server")
  first.running = false
  first.open = false
  mock.advance(1500)
  local second = mock.server()
  ok(second and second ~= first, "a new server")
  eq(second.port, PORT)
  connect(env)
  no_dialogs()
end)

test("bridge: reload swaps the payload and keeps the port and the connection", function()
  local env = setup()
  local peer = connect(env)
  local server = mock.server()
  local old_payload = env.payload()
  local reply = request(peer, "reload", nil, "rl")
  eq(reply.result.reloading, true)
  tick()
  ok(env.payload() ~= old_payload, "new payload")
  ok(mock.server() == server and server.running, "same server")
  local got = frames(peer)
  eq(got[1].event, "bridge.reloading")
  reply = request(peer, "status", nil, "after")
  eq(reply.ok, true)
  eq(mock.notifier_count(mock.song.selected_track_index_observable), 1, "attached once")
  mock.song.selected_track_index = 9
  tick()
  local selection = events(peer, "selection.changed")[1]
  eq(selection.data.track, 9)
  ok(selection.seq > 0)
  no_dialogs()
end)

test("bridge: a broken payload on reload keeps the running one", function()
  local env = setup()
  local peer = connect(env)
  local old_payload = env.payload()
  write(env.bundle .. "payload/status.lua", "this is not lua")
  request(peer, "reload")
  tick()
  ok(env.payload() == old_payload, "kept")
  eq(request(peer, "status", nil, "still").ok, true)
  ok(read(env.state_dir .. "/renoise-bridge.log"):find("reload failed"), "logged")
  no_dialogs()
end)

test("bridge: an error in a song read is logged, never shown", function()
  local env = setup()
  local peer = connect(env)
  local playing = mock.song.transport.playing_observable
  mock.impl(mock.song).transport = nil
  mock.fire(playing)
  tick(3)
  ok(read(env.state_dir .. "/renoise-bridge.log"):find("not mocked"), "logged")
  ok(not peer.closed_by_server)
  no_dialogs()
end)

test("bridge: a secret file others can read is refused", function()
  local env = setup()
  mock.stat_modes[env.secret_file] = 33188
  local peer = mock.connect()
  send(peer, { type = "hello", protocol = PROTOCOL, clientNonce = string.rep("ab", 16) })
  local got = frames(peer)
  eq(got[1].code, "not_configured")
  ok(got[1].message:find("readable"))
  no_dialogs()
end)

test("bridge: a short secret is refused", function()
  local env = setup({ secret = "short" })
  local peer = mock.connect()
  send(peer, { type = "hello", protocol = PROTOCOL, clientNonce = string.rep("ab", 16) })
  eq(frames(peer)[1].code, "not_configured")
  ok(env)
  no_dialogs()
end)

test("bridge: the demo is detected", function()
  local env = setup({ version = "3.5.4 Demo" })
  local _, welcome = connect(env)
  eq(welcome.hello.demo, true)
  eq(welcome.hello.dawVersion, "3.5.4")
  no_dialogs()
end)

test("bridge: works with no song yet, then attaches when one arrives", function()
  local env = setup({ song = false })
  local peer, welcome = connect(env)
  eq(welcome.hello.project, json.null)
  mock.load_song({ name = "First" })
  eq(events(peer, "project.changed")[1].data.name, "First")
  mock.song.selected_track_index = 2
  tick()
  eq(#events(peer, "selection.changed"), 1)
  no_dialogs()
end)

test("bridge: a big frame goes out in slices over ticks", function()
  local env = setup({ song = { name = string.rep("n", 300 * 1024) } })
  local peer = connect(env)
  send(peer, { id = "big", method = "status" })
  tick(2)
  local got = frames(peer)
  eq(got[#got].id, "big")
  eq(#got[#got].result.project.name, 300 * 1024)
  ok(peer.sent_calls > 5, "sent in pieces")
  no_dialogs()
end)

test("bridge: Windows has no pid lookup and keys discovery by port", function()
  local env = setup({ platform = "WINDOWS" })
  eq(#mock.popen_calls, 0, "no process started")
  local discovery = json.decode(read(env.state_dir .. "/discovery/renoise-port-" .. PORT .. ".json"))
  eq(discovery.pid, json.null)
  no_dialogs()
end)

test("bridge: one socket wrapped in several Lua objects is one connection", function()
  local env = setup({ fresh_wrappers = true })
  local peer = connect(env)
  eq(request(peer, "status", nil, "w").ok, true)
  local count = 0
  for _ in pairs(KUMI_BRIDGE.connections) do count = count + 1 end
  eq(count, 1)
  no_dialogs()
end)
