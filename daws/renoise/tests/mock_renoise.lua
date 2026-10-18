-- A mock of the parts of Renoise's Lua API the bridge uses, for stock LuaJIT. Every member name
-- is checked against renoise_api.lua (generated from Renoise's API definitions), so a name
-- Renoise doesn't have fails the test. It also records what Renoise would show as a dialog: an
-- error escaping a notifier, timer or socket callback, and any modal call.

local tests_dir = ...
local api = dofile(tests_dir .. "renoise_api.lua")

local M = {}

M.NIL = setmetatable({}, { __tostring = function() return "NIL" end })

local member_sets = {}
for name, class in pairs(api) do
  local set = {}
  for _, member in ipairs(class.members) do set[member] = true end
  member_sets[name] = set
end

local function has_member(class_name, key)
  local set = member_sets[class_name]
  if not set then return false end
  if set[key] or api[class_name .. "." .. tostring(key)] then return true end
  for _, base in ipairs(api[class_name].bases) do
    if has_member(base, key) then return true end
  end
  return false
end
M.has_member = has_member

local impl_of = setmetatable({}, { __mode = "k" })

function M.impl(proxy)
  return impl_of[proxy]
end

-- A strict object of a Renoise class. getters compute members on each read.
local function object(class_name, fields, getters)
  assert(api[class_name], "mock: unknown class " .. class_name)
  getters = getters or {}
  local proxy = {}
  impl_of[proxy] = fields
  return setmetatable(proxy, {
    __index = function(_, key)
      if not has_member(class_name, key) then
        error("renoise mock: " .. class_name .. " has no member '" .. tostring(key) .. "'", 2)
      end
      if getters[key] then return getters[key]() end
      local value = fields[key]
      if value == nil then
        error("renoise mock: " .. class_name .. "." .. tostring(key) .. " is not mocked", 2)
      end
      if value == M.NIL then return nil end
      return value
    end,
    __newindex = function(_, key, value)
      if not has_member(class_name, key) then
        error("renoise mock: " .. class_name .. " has no member '" .. tostring(key) .. "'", 2)
      end
      local old = fields[key]
      fields[key] = value == nil and M.NIL or value
      local observable = fields[key .. "_observable"]
      if observable and old ~= value then M.fire(observable) end
    end,
    __tostring = function() return class_name end,
  })
end
M.object = object

local function record_error(where, err)
  M.errors[#M.errors + 1] = where .. ": " .. tostring(err)
end

-- Observables

function M.observable()
  local fields = { notifiers = {} }
  local function find(a, b)
    local fn, obj = a, nil
    if type(a) ~= "function" then obj, fn = a, b end
    for i, n in ipairs(fields.notifiers) do
      if n.fn == fn and n.obj == obj then return i, fn, obj end
    end
    return nil, fn, obj
  end
  fields.add_notifier = function(_, a, b)
    local i, fn, obj = find(a, b)
    if i then error("the notifier was already added", 2) end
    fields.notifiers[#fields.notifiers + 1] = { fn = fn, obj = obj }
  end
  fields.remove_notifier = function(_, a, b)
    local i = find(a, b)
    if not i then error("the notifier was not added", 2) end
    table.remove(fields.notifiers, i)
  end
  fields.has_notifier = function(_, a, b)
    return find(a, b) ~= nil
  end
  return object("renoise.Document.Observable", fields)
end

-- Calls the notifiers as Renoise does: one that raises is reported and disabled.
function M.fire(observable, ...)
  local fields = impl_of[observable]
  local list = {}
  for i, n in ipairs(fields.notifiers) do list[i] = n end
  for _, n in ipairs(list) do
    local ok, err
    if n.obj ~= nil then ok, err = pcall(n.fn, n.obj, ...) else ok, err = pcall(n.fn, ...) end
    if not ok then
      record_error("notifier", err)
      for i, m in ipairs(fields.notifiers) do
        if m == n then table.remove(fields.notifiers, i) break end
      end
    end
  end
end

function M.notifier_count(observable)
  return #impl_of[observable].notifiers
end

-- Sockets

local function new_socket_api()
  local fields = { PROTOCOL_TCP = 1, PROTOCOL_UDP = 2 }
  fields.create_server = function(address, port, protocol)
    if type(address) ~= "string" then error("mock: create_server without an address listens on every interface") end
    if protocol ~= nil and protocol ~= 1 then error("mock: only TCP is mocked") end
    if M.busy_ports[port] then return nil, "Address already in use" end
    for _, s in ipairs(M.servers) do
      if s.port == port and s.open then return nil, "Address already in use" end
    end
    local record = { address = address, port = port, open = true, running = false, peers = {} }
    local server_fields = { local_address = address, local_port = port }
    server_fields.run = function(_, notifier)
      if record.running then error("mock: the server is already running") end
      record.notifier = notifier
      record.running = true
    end
    server_fields.stop = function() record.running = false end
    server_fields.close = function()
      record.running = false
      record.open = false
    end
    server_fields.wait = function() error("mock: server:wait blocks Renoise's GUI") end
    record.proxy = object("renoise.Socket.SocketServer", server_fields, {
      is_running = function() return record.running end,
      is_open = function() return record.open end,
    })
    M.servers[#M.servers + 1] = record
    return record.proxy
  end
  fields.create_client = function() error("mock: create_client is not mocked") end
  return object("renoise.Socket", fields)
end

-- The running server on port (or the newest running one).
function M.server(port)
  for i = #M.servers, 1, -1 do
    local s = M.servers[i]
    if s.running and (not port or s.port == port) then return s end
  end
  return nil
end

local Peer = {}
Peer.__index = Peer

function Peer:wrap()
  if self.wrapper and not M.fresh_wrappers then return self.wrapper end
  local peer = self
  local wrapper = object("renoise.Socket.SocketClient", {
    peer_address = peer.address,
    peer_port = peer.port,
    local_address = peer.server.address,
    local_port = peer.server.port,
    send = function(_, data)
      if not peer.open then error("mock: send on a closed socket") end
      peer.inbox[#peer.inbox + 1] = data
      peer.sent_calls = peer.sent_calls + 1
      return true
    end,
    close = function()
      peer.open = false
      peer.closed_by_server = true
    end,
    receive = function() error("mock: receive doesn't work inside Renoise's socket callbacks") end,
  }, { is_open = function() return peer.open end })
  self.wrapper = wrapper
  return wrapper
end

local function callback(server, name, ...)
  local notifier = server.notifier
  if not server.running or not notifier or not notifier[name] then return end
  local ok, err = pcall(notifier[name], ...)
  if not ok then record_error("socket " .. name, err) end
end

-- Data from the peer to the bridge.
function Peer:send(data)
  assert(self.open, "mock: the peer is closed")
  callback(self.server, "socket_message", self:wrap(), data)
end

-- Everything the bridge sent, as text, consumed.
function Peer:take()
  local text = table.concat(self.inbox)
  self.inbox = {}
  self.partial = (self.partial or "") .. text
  return text
end

-- The complete frames the bridge sent since the last call, decoded with decode.
function Peer:frames(decode)
  self:take()
  local frames = {}
  while true do
    local nl = self.partial:find("\n", 1, true)
    if not nl then break end
    local line = self.partial:sub(1, nl - 1)
    self.partial = self.partial:sub(nl + 1)
    frames[#frames + 1] = decode and assert(decode(line)) or line
  end
  return frames
end

function Peer:close()
  self.open = false
end

local next_peer_port = 50000

-- A client connecting to the bridge's server.
function M.connect(options)
  options = options or {}
  local server = assert(M.server(options.port), "mock: no running server")
  next_peer_port = next_peer_port + 1
  local peer = setmetatable({
    server = server,
    address = options.address or "127.0.0.1",
    port = options.peer_port or next_peer_port,
    open = true,
    inbox = {},
    sent_calls = 0,
  }, Peer)
  server.peers[#server.peers + 1] = peer
  callback(server, "socket_accepted", peer:wrap())
  return peer
end

function M.socket_error(message, port)
  local server = M.server(port)
  if server then callback(server, "socket_error", message) end
end

-- Timers

function M.advance(ms)
  local target = M.time + ms
  while true do
    local next_timer
    for _, t in ipairs(M.timers) do
      if t.due <= target and (not next_timer or t.due < next_timer.due) then next_timer = t end
    end
    if not next_timer then break end
    M.time = next_timer.due
    next_timer.due = next_timer.due + next_timer.interval
    local ok, err = pcall(next_timer.fn)
    if not ok then record_error("timer", err) end
  end
  M.time = target
end

-- Songs

function M.make_song(options)
  options = options or {}
  local transport = object("renoise.Transport", {
    playing = options.playing or false,
    playing_observable = M.observable(),
    bpm = options.bpm or 120,
    bpm_observable = M.observable(),
    lpb = options.lpb or 4,
    lpb_observable = M.observable(),
  })
  return object("renoise.Song", {
    name = options.name or "",
    name_observable = M.observable(),
    file_name = options.file_name or "",
    tool_data = options.tool_data or M.NIL,
    transport = transport,
    rendering = false,
    tracks_observable = M.observable(),
    sequencer_track_count = options.sequencer_tracks or 1,
    send_track_count = options.send_tracks or 0,
    selected_track_index = 1,
    selected_track_index_observable = M.observable(),
    selected_instrument_index = 1,
    selected_instrument_index_observable = M.observable(),
    selected_pattern_index = 1,
    selected_pattern_index_observable = M.observable(),
    selected_sequence_index = 1,
    selected_sequence_index_observable = M.observable(),
  })
end

-- Replaces the song as Renoise does: release the old document, then announce the new one.
function M.load_song(options)
  if M.song then M.fire(M.tool.app_release_document_observable) end
  M.song = M.make_song(options)
  M.fire(M.tool.app_new_document_observable)
  return M.song
end

function M.save_song(path)
  M.fire(M.tool.app_will_save_document_observable)
  impl_of[M.song].file_name = path
  M.fire(M.tool.app_saved_document_observable)
end

-- Host OS functions Renoise adds to Lua

local function shell_quote(s)
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

local real_time = os.time

local function install_os_shims()
  -- Wall time follows the mock clock, from a fixed start.
  os.time = function(t)
    if t then return real_time(t) end
    return 1800000000 + math.floor(M.time / 1000)
  end
  -- Renoise's os.clock is a high-precision timer in seconds.
  os.clock = function() return M.time / 1000 end
  os.platform = function() return M.platform end
  os.mkdir = function(path)
    local ok = os.execute("mkdir " .. shell_quote(path) .. " 2>/dev/null")
    if ok == 0 or ok == true then return true end
    return nil, "mkdir failed"
  end
  io.exists = function(path)
    return os.rename(path, path) ~= nil
  end
  io.stat = function(path)
    if not io.exists(path) then return nil, "no such file", 2 end
    return { type = "file", mode = M.stat_modes[path] or 33152, size = 0 }
  end
  io.popen = function(command)
    M.popen_calls[#M.popen_calls + 1] = command
    local done = false
    return {
      read = function()
        if done then return nil end
        done = true
        return "4242"
      end,
      close = function() return true end,
    }
  end
end

-- Builds a fresh Renoise: the tool at bundle_path, no song yet.
function M.reset(options)
  options = options or {}
  M.errors, M.modals, M.statuses, M.popen_calls = {}, {}, {}, {}
  M.servers, M.busy_ports, M.timers, M.menu_entries = {}, {}, {}, {}
  M.stat_modes = {}
  M.time = 0
  M.song = nil
  M.platform = options.platform or "MACINTOSH"
  M.fresh_wrappers = options.fresh_wrappers ~= false
  install_os_shims()

  local function modal(name)
    return function(_, ...)
      M.modals[#M.modals + 1] = { name, ... }
      record_error("modal", name)
    end
  end
  M.app = object("renoise.Application", {
    show_status = function(_, message) M.statuses[#M.statuses + 1] = message end,
    show_message = modal("show_message"),
    show_error = modal("show_error"),
    show_warning = modal("show_warning"),
    show_prompt = modal("show_prompt"),
    show_custom_prompt = modal("show_custom_prompt"),
    prompt_for_path = modal("prompt_for_path"),
    log_filename = "Log.txt",
  })

  local function find_timer(fn)
    for i, t in ipairs(M.timers) do
      if t.fn == fn then return i end
    end
  end
  M.tool = object("renoise.ScriptingTool", {
    bundle_path = options.bundle_path or "/nonexistent/",
    preferences = M.NIL,
    tool_finished_loading_observable = M.observable(),
    tool_will_unload_observable = M.observable(),
    app_became_active_observable = M.observable(),
    app_resigned_active_observable = M.observable(),
    app_idle_observable = M.observable(),
    app_release_document_observable = M.observable(),
    app_new_document_observable = M.observable(),
    app_will_save_document_observable = M.observable(),
    app_saved_document_observable = M.observable(),
    add_timer = function(_, fn, interval)
      if type(fn) ~= "function" then error("mock: only function timers are mocked") end
      if not (interval > 0) then error("timer interval must be > 0") end
      if find_timer(fn) then error("the timer was already added") end
      M.timers[#M.timers + 1] = { fn = fn, interval = interval, due = M.time + interval }
    end,
    remove_timer = function(_, fn)
      local i = find_timer(fn)
      if not i then error("the timer was not added") end
      table.remove(M.timers, i)
    end,
    has_timer = function(_, fn) return find_timer(fn) ~= nil end,
    add_menu_entry = function(_, entry)
      if M.menu_entries[entry.name] then error("menu entry '" .. entry.name .. "' already exists") end
      M.menu_entries[entry.name] = entry
    end,
    has_menu_entry = function(_, name) return M.menu_entries[name] ~= nil end,
    remove_menu_entry = function(_, name) M.menu_entries[name] = nil end,
  })

  renoise = object("renoise", {
    API_VERSION = 6.2,
    RENOISE_VERSION = options.version or "3.5.4",
    app = function() return M.app end,
    song = function() return M.song end,
    tool = function() return M.tool end,
    Socket = new_socket_api(),
  })
  return M
end

return M
