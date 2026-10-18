-- Renoise's sockets on one side, wire connections on the other: loopback peers only, connection
-- limits, requests answered in order, heartbeats, events, and an outbox that sends big frames a
-- slice at a time. Socket closes wait for the next tick, since closing inside Renoise's socket
-- callbacks can upset it.

local import = ...
local wire = import("wire")

local sub = string.sub

local M = {}
M.__index = M

M.SEND_CHUNK = 64 * 1024
M.SEND_NOW_BYTES = 256 * 1024
M.SEND_TICK_BYTES = 1024 * 1024

local LOOPBACK_PEERS = { ["127.0.0.1"] = true, ["::1"] = true, ["::ffff:127.0.0.1"] = true }

local function key_of(client)
  return tostring(client.peer_address) .. ":" .. tostring(client.peer_port)
end

-- ctx: connections (kept by main.lua across payload reloads), now_ms(), unix_ms(), secret() -> secret or
-- nil and why, identity() -> the welcome's fields, on_request(record, request) -> result or raises
-- {code, message}, on_authenticated(record), log(level, message).
function M.new(ctx)
  return setmetatable({ ctx = ctx }, M)
end

function M:counts()
  local waiting, authenticated = 0, 0
  for _, record in pairs(self.ctx.connections) do
    if not record.closing and record.conn then
      if record.conn:authenticated() then authenticated = authenticated + 1 else waiting = waiting + 1 end
    end
  end
  return waiting, authenticated
end

function M:close(record, reason)
  if record.closing then return end
  record.closing = true
  self.ctx.log("info", "closing " .. record.key .. ": " .. tostring(reason))
end

local function raw_send(self, record, text)
  local ok, sent = pcall(function()
    if not record.client.is_open then return false end
    return record.client:send(text)
  end)
  if not ok or not sent then
    record.outbox = {}
    record.gone = true
    self:close(record, "send failed")
    return false
  end
  return true
end

function M:flush(record, budget)
  local sent = 0
  while record.outbox[1] and sent < budget do
    local item = record.outbox[1]
    local piece = sub(item.text, item.offset, item.offset + M.SEND_CHUNK - 1)
    if not raw_send(self, record, piece) then return sent end
    sent = sent + #piece
    item.offset = item.offset + #piece
    if item.offset > #item.text then table.remove(record.outbox, 1) end
  end
  return sent
end

function M:send(record, text)
  if not text or record.gone then return end
  record.outbox[#record.outbox + 1] = { text = text, offset = 1 }
  if #record.outbox == 1 then self:flush(record, M.SEND_NOW_BYTES) end
end

function M:accept(client)
  local now = self.ctx.now_ms()
  local record = { key = key_of(client), client = client, anchor = client, outbox = {}, accepted_ms = now }
  self.ctx.connections[record.key] = record
  if not LOOPBACK_PEERS[client.peer_address] then
    self:close(record, "peer " .. tostring(client.peer_address) .. " is not loopback")
    return record
  end
  local waiting, authenticated = self:counts()
  if waiting >= wire.MAX_UNAUTHENTICATED or authenticated >= wire.MAX_AUTHENTICATED then
    self:send(record, wire.encode_frame("refused", { reason = "limit" }))
    self:close(record, "too many connections")
    return record
  end
  local secret, why = self.ctx.secret()
  if not secret then
    self:close(record, "no secret: " .. tostring(why))
    return record
  end
  record.conn = wire.connection(secret, now)
  return record
end

function M:apply(record, actions)
  for _, action in ipairs(actions) do
    if action.send then
      self:send(record, action.send)
    elseif action.close then
      self:close(record, action.close)
    elseif action.authenticated then
      record.heartbeat_ms = self.ctx.now_ms() + wire.HEARTBEAT_MS
      if self.ctx.on_authenticated then self.ctx.on_authenticated(record) end
    elseif action.request then
      self:answer(record, action.request)
    end
  end
end

function M:answer(record, request)
  local deadline = request.deadlineMs
  if type(deadline) == "number" and self.ctx.unix_ms() > deadline then
    return self:send(record, wire.error(request.id, "deadline", "the deadline passed before the work started"))
  end
  local ok, result = xpcall(self.ctx.on_request, debug.traceback, record, request)
  if ok then
    local text, err = wire.ok(request.id, result)
    if not text then
      self.ctx.log("error", request.method .. ": could not encode the result: " .. tostring(err))
      text = wire.error(request.id, "internal", "could not encode the result: " .. tostring(err))
    end
    return self:send(record, text)
  end
  if type(result) == "table" then
    return self:send(record, wire.error(request.id, result.code or "failed", tostring(result.message or "")))
  end
  self.ctx.log("error", request.method .. " failed: " .. tostring(result))
  self:send(record, wire.error(request.id, "internal", (tostring(result):match("^[^\n]*"))))
end

function M:receive(client, data)
  local record = self.ctx.connections[key_of(client)]
  if not record then record = self:accept(client) end
  record.client = client
  if record.closing or not record.conn then return end
  self:apply(record, record.conn:on_bytes(data, self.ctx.identity))
end

-- An event to every authenticated connection, each numbered on its own connection.
function M:event(name, data)
  for _, record in pairs(self.ctx.connections) do
    if record.conn and not record.closing and record.conn:authenticated() then
      local text, err = record.conn:event(name, data)
      if text then
        self:send(record, text)
      else
        self.ctx.log("error", "could not encode event " .. name .. ": " .. tostring(err))
      end
    end
  end
end

function M:authenticated_count()
  local _, authenticated = self:counts()
  return authenticated
end

local function is_gone(record)
  if record.gone then return true end
  local ok, open = pcall(function() return record.client.is_open end)
  return not ok or not open
end

-- From the timer: handshake deadlines, heartbeats, outboxes, deferred closes.
function M:tick()
  local now = self.ctx.now_ms()
  for key, record in pairs(self.ctx.connections) do
    local conn = record.conn
    if conn and not record.closing then
      self:apply(record, conn:on_tick(now))
      if conn:authenticated() and not record.closing and now >= (record.heartbeat_ms or now) then
        self:send(record, wire.encode_frame("heartbeat", { ms = math.floor(now - record.accepted_ms) }))
        record.heartbeat_ms = math.max((record.heartbeat_ms or now) + wire.HEARTBEAT_MS, now + wire.HEARTBEAT_MS / 2)
      end
    end
    if record.outbox[1] then self:flush(record, M.SEND_TICK_BYTES) end
    if is_gone(record) then
      self.ctx.connections[key] = nil
      if not record.closing then self.ctx.log("info", "connection " .. key .. " went away") end
    elseif record.closing and not record.outbox[1] then
      self.ctx.connections[key] = nil
      pcall(function() record.client:close() end)
    end
  end
end

-- Says bye to authenticated connections and closes every connection now.
function M:close_all(reason)
  for key, record in pairs(self.ctx.connections) do
    if record.conn and record.conn:authenticated() and not record.closing then
      self:send(record, wire.encode_frame("bye", { reason = reason }))
      self:flush(record, M.SEND_TICK_BYTES)
    end
    self.ctx.connections[key] = nil
    pcall(function() record.client:close() end)
  end
end

return M
