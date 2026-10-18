-- The payload's entry: config, the server, connections, requests, song observers and events.

local import = ...
local json = import("json")
local net = import("net")
local sync = import("sync")
local status = import("status")
local config = import("config")
local discovery = import("discovery")
local log = import("log")

local M = {}

M.PORT_TRIES = 10
M.RETRY_S = 1
M.HEARTBEAT_TICKS = 20
M.now = os.time

local host, st, b, conns, observers

function M.log_error(where, err)
  log.write("error", where .. ": " .. tostring(err))
end

local function guard(where, fn)
  return function(...)
    local ok, result = xpcall(fn, debug.traceback, ...)
    if not ok then
      M.log_error(where, result)
      return nil
    end
    return result
  end
end

local function show_status(message)
  pcall(function() renoise.app():show_status(message) end)
end

local function emit(name, data)
  b.event_seq = b.event_seq + 1
  conns:broadcast({ type = "event", event = name, seq = b.event_seq, data = data })
end

function M.hello()
  return status.hello(b.memory, host.bundle_path, b.port)
end

local function discovery_path()
  local id = status.pid() and tostring(status.pid()) or ("port-" .. tostring(b.port))
  return b.cfg.state_dir .. "/discovery/renoise-" .. id .. ".json"
end

local function write_discovery()
  if not (b.cfg and b.port) then return end
  local path = discovery_path()
  local hello = M.hello()
  hello.version = 1
  hello.startedAt = b.started_at
  local ok, err = discovery.write(path, hello)
  if ok then
    b.discovery_path = path
  else
    log.write("error", "could not write the discovery file: " .. tostring(err))
  end
end

local function configure()
  local cfg, err = config.load(host.bundle_path)
  if not cfg then
    if b.config_error ~= err then
      b.config_error = err
      log.write("error", "not set up: " .. err)
      show_status("Kumi: the bridge isn't set up yet; run kumi setup")
    end
    b.retry_at = M.now() + M.RETRY_S * 5
    return false
  end
  b.cfg, b.config_error = cfg, nil
  log.configure(cfg.state_dir .. "/renoise-bridge.log")
  return true
end

local function server_running()
  local ok, running = pcall(function() return st.server ~= nil and st.server.is_running end)
  return ok and running
end

local function listen()
  local last_err
  for offset = 0, M.PORT_TRIES - 1 do
    local port = b.cfg.port + offset
    if port > 65535 then break end
    local server, err = host.listen(port)
    if server then
      b.port = port
      b.listen_error = nil
      log.write("info", "listening on 127.0.0.1:" .. port)
      write_discovery()
      return true
    end
    last_err = err
  end
  if b.listen_error ~= last_err then
    b.listen_error = last_err
    log.write("error", "could not listen: " .. tostring(last_err))
    show_status("Kumi: the bridge could not open its port")
  end
  b.retry_at = M.now() + M.RETRY_S
  return false
end

local function selection(song)
  return {
    track = song.selected_track_index,
    instrument = song.selected_instrument_index,
    pattern = song.selected_pattern_index,
    sequence = song.selected_sequence_index,
  }
end

local function transport(song)
  local t = song.transport
  return { playing = t.playing, bpm = t.bpm, lpb = t.lpb }
end

local function attach(song)
  observers:detach()
  if not song then return end
  observers:watch(song.selected_track_index_observable, "selection")
  observers:watch(song.selected_instrument_index_observable, "selection")
  observers:watch(song.selected_pattern_index_observable, "selection")
  observers:watch(song.selected_sequence_index_observable, "selection")
  observers:watch(song.transport.playing_observable, "transport")
  observers:watch(song.transport.bpm_observable, "transport")
  observers:watch(song.transport.lpb_observable, "transport")
  observers:watch(song.name_observable, "project")
  observers:changed("selection", selection(song))
  observers:changed("transport", transport(song))
  observers:changed("project", status.project(b.memory))
end

local function flush_events()
  if not observers:attached() then return end
  local song = renoise.song()
  if not song then return end
  local dirty = observers:take_dirty()
  if dirty.selection then
    local value = selection(song)
    if observers:changed("selection", value) then emit("selection.changed", value) end
  end
  if dirty.transport then
    local value = transport(song)
    if observers:changed("transport", value) then emit("transport.changed", value) end
  end
  if dirty.project then
    local value = status.project(b.memory)
    if observers:changed("project", value) then
      emit("project.updated", value)
      write_discovery()
    end
  end
end

local function busy()
  local song = renoise.song()
  return { rendering = song ~= nil and song.rendering or false }
end

-- Requests

local handlers = {}

function handlers.hello()
  return M.hello()
end
handlers.status = handlers.hello

function handlers.ping()
  return { pong = true, time = M.now() }
end

function handlers.reload()
  host.request_reload()
  return { reloading = true }
end

function M.handle(_, frame)
  local handler = handlers[frame.method]
  if not handler then error({ code = "unknown_method", message = "unknown method " .. frame.method }) end
  return handler(frame.params or {})
end

-- Lifecycle, called by main.lua

function M.start(h, reloading)
  host, st = h, h.state
  st.bridge = st.bridge or { memory = {}, event_seq = 0, heartbeat_seq = 0, ticks = 0, retry_at = 0, started_at = M.now() }
  b = st.bridge
  conns = net.new({
    connections = st.connections,
    now = function() return M.now() end,
    secret = function() return config.secret(b.cfg) end,
    hello = M.hello,
    on_request = M.handle,
    on_open = function(conn)
      log.write("info", "Kumi connected from " .. conn.key)
      show_status("Kumi connected")
    end,
    log = log.write,
  })
  conns.heartbeat_seq = b.heartbeat_seq
  observers = sync.new(guard)
  if not reloading or not server_running() then
    if configure() then listen() end
  end
  attach(renoise.song())
  log.write("info", (reloading and "reloaded" or "started") .. " bridge " .. import("version"))
end

function M.stop(_, reloading)
  if observers then observers:detach() end
  if conns then b.heartbeat_seq = conns.heartbeat_seq end
  if reloading then
    if conns then conns:broadcast({ type = "event", event = "bridge.reloading", seq = b.event_seq }) end
  else
    if conns then conns:close_all("the tool is unloading") end
    host.close_server()
    discovery.remove(b.discovery_path)
    b.discovery_path = nil
    log.write("info", "stopped")
  end
  log.flush()
end

function M.tick()
  b.ticks = b.ticks + 1
  if not b.cfg then
    if M.now() >= b.retry_at and configure() then listen() end
  elseif not server_running() and M.now() >= b.retry_at then
    listen()
  end
  flush_events()
  conns:tick()
  if b.ticks % M.HEARTBEAT_TICKS == 0 then
    if conns:open_count() > 0 then conns:heartbeat({ lastEventSeq = b.event_seq, busy = busy() }) end
    log.flush()
  end
end

function M.on_accept(client)
  conns:accept(client)
end

function M.on_message(client, data)
  conns:receive(client, data)
end

function M.on_socket_error(message)
  log.write("error", "socket error: " .. tostring(message))
  if not server_running() then
    host.close_server()
    b.retry_at = M.now() + M.RETRY_S
  end
end

function M.on_new_document()
  attach(renoise.song())
  local project = status.project(b.memory)
  emit("project.changed", project)
  write_discovery()
end

function M.on_release_document()
  local project = status.project(b.memory)
  observers:detach()
  emit("project.released", project)
  b.memory.song_id = nil
end

function M.on_will_save()
  local song = renoise.song()
  if song then status.write_song_id(b.memory, song) end
end

function M.on_saved()
  local project = status.project(b.memory)
  observers:changed("project", project)
  emit("project.saved", project)
  write_discovery()
end

function M.on_reload_failed(err)
  log.write("error", "reload failed, keeping the running payload: " .. tostring(err))
end

M.json = json

return M
