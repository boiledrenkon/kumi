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
      -- Renoise closes a server's clients with it.
      for _, peer in ipairs(record.peers) do peer.open = false end
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
  -- Reading a property of a closed socket raises, as Renoise's do.
  local function while_open(value)
    return function()
      if not peer.open then error("socket is closed") end
      return value
    end
  end
  local wrapper = object("renoise.Socket.SocketClient", {
    peer_address = peer.address,
    peer_port = peer.port,
    local_address = peer.server.address,
    local_port = peer.server.port,
    send = function(_, data)
      if not peer.open then error("mock: send on a closed socket") end
      if peer.send_fails then return false end
      if peer.stalled then
        -- The peer stopped reading: what it hasn't read piles up in the socket's buffers, and once they are full a real
        -- send blocks Renoise's GUI thread until the peer reads again.
        peer.unread = peer.unread + #data
        if peer.unread > M.socket_buffer_bytes then M.blocked_sends = M.blocked_sends + 1 end
      end
      peer.inbox[#peer.inbox + 1] = data
      peer.sent_calls = peer.sent_calls + 1
      return true
    end,
    close = function()
      -- Renoise 3.5.4: closing a client its server accepted ends the server (its select fails with "Bad file
      -- descriptor", is_running turns false and every connection is lost), and closing one the peer had already closed
      -- crashes Renoise. Neither is ever right, so both are recorded for the tests to refuse.
      if not peer.open then
        M.crashes[#M.crashes + 1] = "closed a socket the peer had closed (Renoise crashes): " .. tostring(peer.port)
        error("mock: Renoise crashed (SIGSEGV in TLuaSocketBase::Close)", 2)
      else
        M.crashes[#M.crashes + 1] = "closed an accepted client (Renoise's server stops): " .. tostring(peer.port)
      end
      peer.server.running = false
      peer.open = false
      peer.closed_by_server = true
    end,
    receive = function() error("mock: receive doesn't work inside Renoise's socket callbacks") end,
  }, {
    is_open = function() return peer.open end,
    peer_address = while_open(peer.address),
    peer_port = while_open(peer.port),
    local_address = while_open(peer.server.address),
    local_port = while_open(peer.server.port),
  })
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

-- The peer stops reading (a suspended process): from now on its socket's buffers fill.
function Peer:stop_reading()
  self.stalled, self.unread = true, 0
end

function Peer:resume_reading()
  self.stalled, self.unread = false, 0
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
    unread = 0,
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

-- Time passes; with M.stall_timers set no timer runs meanwhile, as when Renoise's timers are held behind a modal dialog
-- or slowed in the background while its sockets go on calling in.
function M.advance(ms)
  local target = M.time + ms
  while not M.stall_timers do
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
  M.time = math.max(M.time, target)
end

-- Songs

-- (M.make_song and M.ops, the song and what the producer does to it, are in mock_song.lua.)
assert(loadfile(tests_dir .. "mock_song.lua"))()(M, object)

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
local real_popen = io.popen
local real_rename, real_remove = os.rename, os.remove

-- Windows' stock os.rename and os.remove take the ANSI code page: a path with UTF-8 letters is not found by them.
local function ansi_blind(path)
  return M.platform == "WINDOWS" and tostring(path):find("[\128-\255]") ~= nil
end

local function install_os_shims()
  -- Wall time follows the mock clock, from a fixed start.
  os.time = function(t)
    if t then return real_time(t) end
    return 1800000000 + math.floor(M.time / 1000)
  end
  -- Renoise's os.clock is a high-precision timer in seconds. Each read can cost time (M.clock_step_ms), as work does:
  -- it is how a test makes a job use up its slices.
  os.clock = function()
    M.time = M.time + (M.clock_step_ms or 0)
    return M.time / 1000
  end
  os.platform = function() return M.platform end
  os.mkdir = function(path)
    local ok = os.execute("mkdir " .. shell_quote(path) .. " 2>/dev/null")
    if ok == 0 or ok == true then return true end
    return nil, "mkdir failed"
  end
  os.rename = function(from, to)
    if ansi_blind(from) or ansi_blind(to) then return nil, from .. ": No such file or directory" end
    return real_rename(from, to)
  end
  os.remove = function(path)
    if ansi_blind(path) then return nil, path .. ": No such file or directory" end
    return real_remove(path)
  end
  -- Renoise's own: UTF-8 paths. Whether it replaces a file at the destination is a switch (M.move_replaces).
  os.move = function(from, to)
    if M.move_replaces == false and real_rename(to, to) then return nil, to .. ": file exists" end
    return real_rename(from, to)
  end
  io.exists = function(path)
    return real_rename(path, path) ~= nil
  end
  io.stat = function(path)
    if not (M.stat_modes[path] or io.exists(path)) then return nil, "no such file", 2 end
    return { type = "file", mode = M.stat_modes[path] or 33152, size = 0, mtime = M.stat_mtimes[path] or os.time() }
  end
  -- Names (not paths) of the files in a folder, optionally only those matching {"*.log", ...}.
  os.filenames = function(dir, extensions)
    local names = {}
    local listing = assert(real_popen("ls -1 '" .. dir:gsub("'", "'\\''") .. "' 2>/dev/null"))
    for name in listing:lines() do
      local wanted = extensions == nil
      for _, pattern in ipairs(extensions or {}) do
        local lua = "^" .. pattern:gsub("[%^%$%(%)%%%.%[%]%+%-%?]", "%%%0"):gsub("%*", ".*") .. "$"
        if name:find(lua) then wanted = true end
      end
      if wanted then names[#names + 1] = name end
    end
    listing:close()
    return names
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
  -- Calls that would crash the real Renoise.
  M.crashes = {}
  M.servers, M.busy_ports, M.timers, M.menu_entries = {}, {}, {}, {}
  M.stat_modes, M.stat_mtimes = {}, {}
  M.stall_timers = false
  M.clock_step_ms = 0
  -- Bytes a socket takes from a peer that doesn't read, and the sends that would have blocked because of it.
  M.socket_buffer_bytes, M.blocked_sends = options.socket_buffer_bytes or 256 * 1024, 0
  M.time = 0
  M.song = nil
  M.platform = options.platform or "MACINTOSH"
  M.move_replaces = options.move_replaces ~= false
  M.fresh_wrappers = options.fresh_wrappers ~= false
  install_os_shims()

  -- The window: which frames the producer has open.
  M.window = object("renoise.ApplicationWindow", {
    MIDDLE_FRAME_PATTERN_EDITOR = 1, MIDDLE_FRAME_MIXER = 2, MIDDLE_FRAME_INSTRUMENT_PHRASE_EDITOR = 3,
    MIDDLE_FRAME_INSTRUMENT_SAMPLE_KEYZONES = 4, MIDDLE_FRAME_INSTRUMENT_SAMPLE_EDITOR = 5,
    MIDDLE_FRAME_INSTRUMENT_SAMPLE_MODULATION = 6, MIDDLE_FRAME_INSTRUMENT_SAMPLE_EFFECTS = 7,
    MIDDLE_FRAME_INSTRUMENT_PLUGIN_EDITOR = 8, MIDDLE_FRAME_INSTRUMENT_MIDI_EDITOR = 9,
    LOWER_FRAME_TRACK_DSPS = 1, LOWER_FRAME_TRACK_AUTOMATION = 2,
    active_middle_frame = 1, active_middle_frame_observable = M.observable(),
    pattern_matrix_is_visible = false, pattern_matrix_is_visible_observable = M.observable(),
    lower_frame_is_visible = false, lower_frame_is_visible_observable = M.observable(),
    active_lower_frame = 1, active_lower_frame_observable = M.observable(),
  })

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
    window = M.window,
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
    Track = object("renoise.Track", { TRACK_TYPE_SEQUENCER = 1, TRACK_TYPE_MASTER = 2, TRACK_TYPE_SEND = 3,
      TRACK_TYPE_GROUP = 4, MUTE_STATE_ACTIVE = 1, MUTE_STATE_OFF = 2, MUTE_STATE_MUTED = 3 }),
    Sample = object("renoise.Sample", { LOOP_MODE_OFF = 1, LOOP_MODE_FORWARD = 2, LOOP_MODE_REVERSE = 3, LOOP_MODE_PING_PONG = 4 }),
    InstrumentPhrase = object("renoise.InstrumentPhrase", { KEY_TRACKING_NONE = 1, KEY_TRACKING_TRANSPOSE = 2, KEY_TRACKING_OFFSET = 3 }),
    ApplicationWindow = M.window,
  })
  return M
end

return M
