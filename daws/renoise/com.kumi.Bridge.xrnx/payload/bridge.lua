-- The payload's entry: setup, the server, requests, song observers and events.

local import = ...
local json = import("json")
local wire = import("wire")
local server = import("server")
local sync = import("sync")
local status = import("status")
local config = import("config")
local discovery = import("discovery")
local log = import("log")
local VERSION = import("version")

local M = {}

M.RETRY_MS = 1000
M.SETUP_RETRY_MS = 5000

-- Renoise replaces os.clock with a high-precision timer in seconds.
function M.now_ms()
  return os.clock() * 1000
end

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

-- Unix milliseconds: os.time's second, refined by the timer since that second was seen to start.
local function unix_ms()
  local now, seconds = M.now_ms(), os.time()
  if seconds ~= b.anchor_seconds then
    b.anchor_seconds, b.anchor_ms = seconds, now
  end
  return seconds * 1000 + math.min(now - b.anchor_ms, 999)
end

local function identity()
  return status.identity(b.memory, host.bundle_path)
end

local function write_discovery()
  if not (b.cfg and b.port) then return end
  local pid = status.pid() or 0
  local resource = status.resource(host.bundle_path)
  local path = b.cfg.discovery_dir .. "/" .. discovery.file_name(pid, resource)
  local ok, err = discovery.write(path, {
    schema = discovery.SCHEMA,
    daw = "renoise",
    pid = pid,
    host = "127.0.0.1",
    port = b.port,
    protocol = wire.PROTOCOL,
    version = VERSION,
    dawVersion = renoise.RENOISE_VERSION,
    resource = resource,
    project = status.project(b.memory),
    startedMs = b.started_ms,
  })
  if ok then
    if b.discovery_path and b.discovery_path ~= path then discovery.remove(b.discovery_path) end
    b.discovery_path = path
  else
    log.write("error", "could not write the discovery file: " .. tostring(err))
  end
end

local function configure()
  local cfg, err = config.load(host.bundle_path)
  if not cfg then
    if b.setup_error ~= err then
      b.setup_error = err
      log.write("error", "not set up: " .. err)
      show_status("Kumi: the bridge isn't set up yet; run kumi setup")
    end
    b.retry_ms = M.now_ms() + M.SETUP_RETRY_MS
    return false
  end
  b.cfg, b.setup_error = cfg, nil
  if cfg.log_dir then log.configure(cfg.log_dir .. "/renoise-bridge.log") end
  return true
end

local function server_running()
  local ok, running = pcall(function() return st.server ~= nil and st.server.is_running end)
  return ok and running
end

local function listen()
  local last_err
  for _, port in ipairs(config.candidate_ports(b.cfg.port)) do
    local socket, err = host.listen(port)
    if socket then
      b.port, b.listen_error = port, nil
      log.write("info", "listening on 127.0.0.1:" .. port)
      write_discovery()
      return true
    end
    last_err = err
  end
  if b.listen_error ~= last_err then
    b.listen_error = last_err
    log.write("error", "could not listen: " .. tostring(last_err))
    show_status("Kumi: the bridge could not open a port")
  end
  b.retry_ms = M.now_ms() + M.RETRY_MS
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

local function send_project()
  local project = status.project(b.memory)
  observers:changed("project", project)
  conns:event("project", project)
  write_discovery()
end

local function flush_events()
  if not observers:attached() then return end
  local song = renoise.song()
  if not song then return end
  local dirty = observers:take_dirty()
  if dirty.selection then
    local value = selection(song)
    if observers:changed("selection", value) then conns:event("selection", value) end
  end
  if dirty.transport then
    local value = transport(song)
    if observers:changed("transport", value) then conns:event("transport", value) end
  end
  if dirty.project and observers:changed("project", status.project(b.memory)) then send_project() end
end

-- Requests

local handlers = {}

function handlers.ping()
  return json.object()
end

function handlers.status()
  return status.status(b.memory, host.bundle_path)
end

handlers["renoise.reload"] = function()
  host.request_reload()
  return { reloading = true }
end

function M.handle(_, request)
  local handler = handlers[request.method]
  if not handler then error({ code = "unknown-method", message = "no method " .. request.method }) end
  return handler(request.params)
end

-- Lifecycle, called by main.lua

function M.start(h, reloading)
  host, st = h, h.state
  st.bridge = st.bridge or { memory = {}, retry_ms = 0, started_ms = 0 }
  b = st.bridge
  if b.started_ms == 0 then b.started_ms = unix_ms() end
  conns = server.new({
    connections = st.connections,
    now_ms = function() return M.now_ms() end,
    unix_ms = unix_ms,
    secret = function()
      local secret, why = config.secret(b.cfg)
      if not secret then log.write("error", "refusing a connection: " .. tostring(why)) end
      return secret, why
    end,
    identity = identity,
    on_request = M.handle,
    on_authenticated = function(record)
      log.write("info", "Kumi connected from " .. record.key)
      show_status("Kumi connected")
    end,
    log = log.write,
  })
  observers = sync.new(guard)
  status.cache = b
  if b.cfg and b.cfg.log_dir then log.configure(b.cfg.log_dir .. "/renoise-bridge.log") end
  if not reloading or not server_running() then
    if configure() then listen() end
  end
  attach(renoise.song())
  log.write("info", (reloading and "reloaded" or "started") .. " bridge " .. VERSION)
  log.flush()
end

function M.stop(_, reloading)
  if observers then observers:detach() end
  if not reloading then
    if conns then conns:close_all("unload") end
    host.close_server()
    discovery.remove(b.discovery_path)
    b.discovery_path = nil
    log.write("info", "stopped")
  end
  log.flush()
end

function M.tick()
  unix_ms()
  if not b.cfg then
    if M.now_ms() >= b.retry_ms and configure() then listen() end
  elseif not server_running() and M.now_ms() >= b.retry_ms then
    listen()
  end
  flush_events()
  conns:tick()
  b.ticks = (b.ticks or 0) + 1
  if b.ticks % 20 == 0 then log.flush() end
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
    b.retry_ms = M.now_ms() + M.RETRY_MS
  end
end

function M.on_new_document()
  attach(renoise.song())
  send_project()
end

function M.on_release_document()
  observers:detach()
  b.memory.song_id = nil
  conns:event("project", json.null)
end

function M.on_will_save()
  local song = renoise.song()
  if song then status.write_song_id(b.memory, song) end
end

function M.on_saved()
  send_project()
end

function M.on_reload_failed(err)
  log.write("error", "reload failed, keeping the running payload: " .. tostring(err))
end

return M
