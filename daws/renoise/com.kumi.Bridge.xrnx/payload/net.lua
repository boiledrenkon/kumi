-- Connections: NDJSON framing, loopback-only peers, the per-connection HMAC handshake, limits
-- before auth, and an outbox that sends big frames a slice at a time.

local import = ...
local json = import("json")
local sha256 = import("sha256")

local find, sub, byte, format = string.find, string.sub, string.byte, string.format
local concat = table.concat

local M = {}

M.PROTOCOL = "kumi-daw/1"
M.MAX_PREAUTH_LINE = 4096
M.MAX_FRAME = 64 * 1024 * 1024
M.MAX_PREAUTH_CONNECTIONS = 4
M.AUTH_TIMEOUT_S = 5
M.SEND_CHUNK = 64 * 1024
M.SEND_NOW_BYTES = 256 * 1024
M.SEND_BUDGET_BYTES = 1024 * 1024

local LOOPBACK_PEERS = { ["127.0.0.1"] = true, ["::1"] = true, ["::ffff:127.0.0.1"] = true }

-- Framing

function M.framer(max_bytes)
  return { parts = {}, count = 0, size = 0, max = max_bytes }
end

-- Returns the complete lines in the chunk (without "\n", "\r\n" or a leading BOM), and
-- "oversized" once a line passes the limit.
function M.feed(framer, chunk)
  local lines, n, start = {}, 0, 1
  while true do
    local nl = find(chunk, "\n", start, true)
    if not nl then break end
    local piece = sub(chunk, start, nl - 1)
    if framer.size + #piece > framer.max then return lines, "oversized" end
    local line = piece
    if framer.count > 0 then
      framer.count = framer.count + 1
      framer.parts[framer.count] = piece
      line = concat(framer.parts, "", 1, framer.count)
      framer.parts, framer.count = {}, 0
    end
    framer.size = 0
    if byte(line, -1) == 13 then line = sub(line, 1, -2) end
    if sub(line, 1, 3) == "\239\187\191" then line = sub(line, 4) end
    n = n + 1
    lines[n] = line
    start = nl + 1
  end
  if start <= #chunk then
    local rest = sub(chunk, start)
    if framer.size + #rest > framer.max then return lines, "oversized" end
    framer.count = framer.count + 1
    framer.parts[framer.count] = rest
    framer.size = framer.size + #rest
  end
  return lines
end

-- Nonces and MACs

local entropy, counter = nil, 0

-- A fresh table's address, a little entropy where nothing better exists.
local function address()
  local t = setmetatable({}, nil)
  return tostring(t)
end

local function gather_entropy()
  local f = io.open("/dev/urandom", "rb")
  if f then
    local bytes = f:read(32)
    f:close()
    if bytes and #bytes == 32 then return bytes end
  end
  return tostring(os.time()) .. tostring(os.clock()) .. address() .. tostring(math.random())
end

-- 32 hex characters, unpredictable without the secret even where /dev/urandom is missing.
function M.nonce(secret)
  entropy = entropy or gather_entropy()
  counter = counter + 1
  local input = format("%d:%d:%.6f:%s", counter, os.time(), os.clock(), address())
  return sub(sha256.tohex(sha256.hmac(entropy .. (secret or ""), input)), 1, 32)
end

function M.client_proof(secret, server_nonce, client_nonce)
  return sha256.hmac_hex(secret, M.PROTOCOL .. " client\n" .. server_nonce .. "\n" .. client_nonce)
end

function M.server_proof(secret, client_nonce, server_nonce)
  return sha256.hmac_hex(secret, M.PROTOCOL .. " server\n" .. client_nonce .. "\n" .. server_nonce)
end

-- Connections

local function key_of(client)
  return tostring(client.peer_address) .. ":" .. tostring(client.peer_port)
end

-- ctx: connections (table), now() in seconds, secret() -> secret or nil and why,
-- hello() -> the status table, on_request(conn, request) -> result, or raises {code, message},
-- log(level, message).
function M.new(ctx)
  return setmetatable({ ctx = ctx, heartbeat_seq = 0 }, { __index = M })
end

function M:count_pending()
  local count = 0
  for _, conn in pairs(self.ctx.connections) do
    if conn.phase ~= "open" and conn.phase ~= "closing" then count = count + 1 end
  end
  return count
end

-- Closing a socket inside its own callback can upset Renoise, so closes wait for the next tick.
function M:close(conn, reason)
  if conn.phase == "closing" then return end
  conn.phase = "closing"
  conn.close_reason = reason
  self.ctx.log("info", "closing " .. conn.key .. ": " .. tostring(reason))
end

function M:accept(client)
  local key = key_of(client)
  local conn = {
    key = key,
    client = client,
    anchor = client,
    framer = M.framer(M.MAX_PREAUTH_LINE),
    phase = "hello",
    deadline = self.ctx.now() + M.AUTH_TIMEOUT_S,
    outbox = {},
    first_bytes = true,
  }
  local pending = self:count_pending()
  self.ctx.connections[key] = conn
  if not LOOPBACK_PEERS[client.peer_address] then
    self:close(conn, "peer is not loopback")
  elseif pending >= M.MAX_PREAUTH_CONNECTIONS then
    self:close(conn, "too many connections waiting to authenticate")
  end
  return conn
end

function M:find(client)
  local conn = self.ctx.connections[key_of(client)]
  if conn then conn.client = client end
  return conn
end

local function raw_send(self, conn, text)
  local ok, sent = pcall(function()
    if not conn.client.is_open then return false end
    return conn.client:send(text)
  end)
  if not ok or not sent then
    self:close(conn, "send failed")
    return false
  end
  return true
end

-- Sends what fits in the byte budget; returns the bytes sent.
function M:flush(conn, budget)
  local sent = 0
  while conn.outbox[1] and sent < budget do
    local item = conn.outbox[1]
    local piece = sub(item.text, item.offset, item.offset + M.SEND_CHUNK - 1)
    if not raw_send(self, conn, piece) then
      conn.outbox = {}
      return sent
    end
    sent = sent + #piece
    item.offset = item.offset + #piece
    if item.offset > #item.text then table.remove(conn.outbox, 1) end
  end
  return sent
end

function M:send_text(conn, text)
  if conn.phase == "closing" and not conn.flush_before_close then return end
  conn.outbox[#conn.outbox + 1] = { text = text .. "\n", offset = 1 }
  if #conn.outbox == 1 then self:flush(conn, M.SEND_NOW_BYTES) end
end

function M:send(conn, message)
  local text, err = json.encode(message)
  if not text then
    self.ctx.log("error", "could not encode a frame: " .. tostring(err))
    if message.id then
      text = json.encode({ id = message.id, ok = false, error = { code = "encode", message = tostring(err) } })
    else
      return
    end
  end
  self:send_text(conn, text)
end

-- Sends a frame and closes after it has gone out.
function M:fail(conn, code, message)
  conn.flush_before_close = true
  self:send(conn, { type = "error", code = code, message = message })
  self:close(conn, code)
end

local function looks_like_frame_start(data)
  local first = find(data, "[^ \t\r\n]")
  return not first or byte(data, first) == 123
end

local function valid_nonce(value)
  return type(value) == "string" and #value >= 32 and #value <= 128 and find(value, "^[0-9a-f]+$") ~= nil
end

function M:on_hello(conn, frame)
  if frame.type ~= "hello" or frame.protocol ~= M.PROTOCOL or not valid_nonce(frame.clientNonce) then
    return self:fail(conn, "protocol", "expected hello for " .. M.PROTOCOL)
  end
  local secret, err = self.ctx.secret()
  if not secret then
    return self:fail(conn, "not_configured", err)
  end
  conn.client_nonce = frame.clientNonce
  conn.server_nonce = M.nonce(secret)
  conn.phase = "auth"
  self:send(conn, { type = "challenge", protocol = M.PROTOCOL, daw = "renoise", serverNonce = conn.server_nonce })
end

function M:on_auth(conn, frame)
  local secret = frame.type == "auth" and type(frame.mac) == "string" and self.ctx.secret()
  local expected = secret and M.client_proof(secret, conn.server_nonce, conn.client_nonce)
  if not expected or not sha256.equal(expected, frame.mac) then
    return self:fail(conn, "auth", "authentication failed")
  end
  conn.phase = "open"
  conn.framer.max = M.MAX_FRAME
  self:send(conn, {
    type = "welcome",
    mac = M.server_proof(secret, conn.client_nonce, conn.server_nonce),
    hello = self.ctx.hello(),
  })
  if self.ctx.on_open then self.ctx.on_open(conn) end
end

local VALID_ID = "^[A-Za-z0-9_.:-]+$"

function M:on_request(conn, frame)
  local id = frame.id
  if type(id) ~= "string" or #id == 0 or #id > 128 or not find(id, VALID_ID) then
    return self:send(conn, { type = "error", code = "invalid", message = "a request needs a string id" })
  end
  if type(frame.method) ~= "string" then
    return self:send(conn, { id = id, ok = false, error = { code = "invalid", message = "a request needs a method" } })
  end
  if frame.params ~= nil and type(frame.params) ~= "table" then
    return self:send(conn, { id = id, ok = false, error = { code = "invalid", message = "params must be an object" } })
  end
  local ok, result = xpcall(self.ctx.on_request, debug.traceback, conn, frame)
  if ok then
    return self:send(conn, { id = id, ok = true, result = result == nil and json.object() or result })
  end
  local code, message = "internal", tostring(result)
  if type(result) == "table" then
    code, message = result.code or code, result.message or message
  else
    self.ctx.log("error", "request " .. frame.method .. " failed: " .. message)
    message = tostring(message):match("^[^\n]*")
  end
  self:send(conn, { id = id, ok = false, error = { code = code, message = message } })
end

function M:receive(client, data)
  local conn = self:find(client)
  if not conn then conn = self:accept(client) end
  if conn.phase == "closing" then return end
  if conn.first_bytes then
    conn.first_bytes = false
    if not looks_like_frame_start(data) then return self:close(conn, "not a Kumi client") end
  end
  local lines, err = M.feed(conn.framer, data)
  for _, line in ipairs(lines) do
    if conn.phase == "closing" then return end
    if line ~= "" then
      local frame, decode_err = json.decode(line)
      if type(frame) ~= "table" or json.is_array(frame) then
        if conn.phase ~= "open" then return self:close(conn, "not a Kumi client") end
        self:send(conn, { type = "error", code = "invalid", message = "frame is not a JSON object: " .. tostring(decode_err) })
      elseif conn.phase == "hello" then
        self:on_hello(conn, frame)
      elseif conn.phase == "auth" then
        self:on_auth(conn, frame)
      else
        self:on_request(conn, frame)
      end
    end
  end
  if err and conn.phase ~= "closing" then
    if conn.phase == "open" then
      self:fail(conn, "oversized", "frame is larger than " .. M.MAX_FRAME .. " bytes")
    else
      self:close(conn, "oversized first frame")
    end
  end
end

function M:broadcast(message)
  local text, err = json.encode(message)
  if not text then
    self.ctx.log("error", "could not encode an event: " .. tostring(err))
    return
  end
  for _, conn in pairs(self.ctx.connections) do
    if conn.phase == "open" then self:send_text(conn, text) end
  end
end

function M:open_count()
  local count = 0
  for _, conn in pairs(self.ctx.connections) do
    if conn.phase == "open" then count = count + 1 end
  end
  return count
end

-- Runs from the timer: deadlines, deferred closes, outboxes.
function M:tick()
  local now = self.ctx.now()
  for key, conn in pairs(self.ctx.connections) do
    if conn.phase ~= "open" and conn.phase ~= "closing" and now > conn.deadline then
      self:close(conn, "did not authenticate in time")
    end
    if conn.outbox[1] then self:flush(conn, M.SEND_BUDGET_BYTES) end
    local gone = not pcall(function()
      if not conn.client.is_open then error("closed") end
    end)
    if conn.phase == "closing" and (gone or not conn.flush_before_close or not conn.outbox[1]) then
      self.ctx.connections[key] = nil
      pcall(function() conn.client:close() end)
    elseif gone then
      self.ctx.connections[key] = nil
      self.ctx.log("info", "connection " .. key .. " went away")
    end
  end
end

function M:heartbeat(extra)
  self.heartbeat_seq = self.heartbeat_seq + 1
  local message = { type = "heartbeat", seq = self.heartbeat_seq }
  for k, v in pairs(extra or {}) do message[k] = v end
  self:broadcast(message)
end

function M:close_all(reason)
  for key, conn in pairs(self.ctx.connections) do
    self.ctx.connections[key] = nil
    conn.phase = "closing"
    self.ctx.log("info", "closing " .. key .. ": " .. reason)
    pcall(function() conn.client:close() end)
  end
end

return M
