-- Kumi's bridge for Renoise.
-- This loader stays small and stable: it owns the listening socket, the timer and Renoise's hooks,
-- and loads the payload (payload/*.lua), which a reload swaps without closing the port.

local tool = renoise.tool()
local BUNDLE = tool.bundle_path
if not BUNDLE:match("[/\\]$") then BUNDLE = BUNDLE .. "/" end

local TICK_MS = 50

-- Everything that must stay referenced (the sockets above all) hangs off this table.
local state = {
  server = nil,
  connections = {},
  payload = nil,
  reload_requested = false,
}
KUMI_BRIDGE = state

local function report(where, err)
  local payload = state.payload
  local ok = payload and payload.log_error and pcall(payload.log_error, where, err)
  if not ok then print("Kumi bridge: " .. where .. ": " .. tostring(err)) end
end

-- Renoise disables a notifier behind a modal dialog when it raises, so nothing raises past here.
local function guard(where, fn)
  return function(...)
    local ok, result = xpcall(fn, debug.traceback, ...)
    if not ok then
      report(where, result)
      return nil
    end
    return result
  end
end

local function load_payload()
  local loaded = {}
  local function import(name)
    local module = loaded[name]
    if module == nil then
      local chunk, err = loadfile(BUNDLE .. "payload/" .. name .. ".lua")
      if not chunk then error(err, 0) end
      module = chunk(import)
      loaded[name] = module
    end
    return module
  end
  return import("bridge")
end

local function call_payload(name, ...)
  local payload = state.payload
  if payload and payload[name] then return payload[name](...) end
end

local forwarder = {
  socket_accepted = guard("socket_accepted", function(client) call_payload("on_accept", client) end),
  socket_message = guard("socket_message", function(client, data) call_payload("on_message", client, data) end),
  socket_error = guard("socket_error", function(message) call_payload("on_socket_error", message) end),
}

local LOOPBACK = { ["127.0.0.1"] = true, ["::1"] = true, ["localhost"] = true }

local host = { bundle_path = BUNDLE, state = state, tick_ms = TICK_MS }

function host.close_server()
  local server = state.server
  state.server = nil
  if server then
    pcall(function()
      if server.is_running then server:stop() end
      server:close()
    end)
  end
end

-- Listens on 127.0.0.1 only; returns the server, or nil and why.
function host.listen(port)
  host.close_server()
  local server, err = renoise.Socket.create_server("127.0.0.1", port, renoise.Socket.PROTOCOL_TCP)
  if not server then return nil, err or "could not listen" end
  if not LOOPBACK[server.local_address] then
    pcall(function() server:close() end)
    return nil, "listening on " .. tostring(server.local_address) .. " is not loopback"
  end
  server:run(forwarder)
  state.server = server
  return server
end

function host.request_reload()
  state.reload_requested = true
end

local function start()
  if state.payload then return end
  local ok, payload = xpcall(load_payload, debug.traceback)
  if not ok then
    report("load", payload)
    return
  end
  state.payload = payload
  payload.start(host, false)
end

local function stop()
  if state.payload then guard("stop", state.payload.stop)(host, false) end
  state.payload = nil
  host.close_server()
  for key, connection in pairs(state.connections) do
    state.connections[key] = nil
    pcall(function() connection.client:close() end)
  end
end

local function swap_payload()
  local ok, fresh = xpcall(load_payload, debug.traceback)
  if not ok then
    report("reload", fresh)
    call_payload("on_reload_failed", fresh)
    return
  end
  if state.payload then guard("stop", state.payload.stop)(host, true) end
  state.payload = fresh
  guard("start", fresh.start)(host, true)
end

local tick = guard("tick", function()
  if state.reload_requested then
    state.reload_requested = false
    swap_payload()
  end
  call_payload("tick")
end)

local function hook(observable, where, fn)
  local guarded = guard(where, fn)
  if not observable:has_notifier(guarded) then observable:add_notifier(guarded) end
end

hook(tool.tool_finished_loading_observable, "finished_loading", start)
hook(tool.tool_will_unload_observable, "will_unload", function()
  if tool:has_timer(tick) then tool:remove_timer(tick) end
  stop()
end)
hook(tool.app_new_document_observable, "new_document", function()
  start()
  call_payload("on_new_document")
end)
hook(tool.app_release_document_observable, "release_document", function() call_payload("on_release_document") end)
hook(tool.app_will_save_document_observable, "will_save", function() call_payload("on_will_save") end)
hook(tool.app_saved_document_observable, "saved", function() call_payload("on_saved") end)

if not tool:has_timer(tick) then tool:add_timer(tick, TICK_MS) end
guard("start", start)()
