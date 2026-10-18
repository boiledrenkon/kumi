-- Reads bridge-reference.json, which Kumi's lifecycle writes into the bundle, and the secret it
-- names. The reference never holds the secret itself.

local import = ...
local json = import("json")
local wire = import("wire")

local M = {}

M.REFERENCE = "bridge-reference.json"
M.SCHEMA = "kumi-daw-reference/1"
M.MAX_SMALL_FILE = 64 * 1024
M.FIRST_PORT = 9780
M.PORTS = 10

local function windows()
  return os.platform ~= nil and os.platform() == "WINDOWS"
end

-- A small regular file's text, or nil and why.
local function read_small(path)
  if io.stat then
    local stat = io.stat(path)
    if not stat then return nil, "missing" end
    if stat.type and stat.type ~= "file" then return nil, "not a regular file" end
    if stat.size and stat.size > M.MAX_SMALL_FILE then return nil, "too large" end
  end
  local f, err = io.open(path, "rb")
  if not f then return nil, err end
  local text = f:read(M.MAX_SMALL_FILE + 1) or ""
  f:close()
  if #text > M.MAX_SMALL_FILE then return nil, "too large" end
  if not json.valid_utf8(text) then return nil, "not UTF-8" end
  return text
end

local function absolute(path)
  return type(path) == "string" and (path:match("^/") or path:match("^%a:[/\\]") or path:match("^\\\\")) ~= nil
end

-- Returns {secret_file, discovery_dir, port, log_dir}, or nil and why.
function M.load(bundle_path)
  local text, err = read_small(bundle_path .. M.REFERENCE)
  if not text then return nil, M.REFERENCE .. ": " .. tostring(err) end
  local value, decode_err = json.decode(text)
  if type(value) ~= "table" or json.is_array(value) then
    return nil, M.REFERENCE .. " is not a reference: " .. tostring(decode_err or "not an object")
  end
  if value.schema ~= M.SCHEMA then return nil, M.REFERENCE .. " has an unknown schema " .. tostring(value.schema) end
  if value.daw ~= "renoise" then return nil, M.REFERENCE .. " is for " .. tostring(value.daw) end
  if not absolute(value.secretFile) or not absolute(value.discoveryDir) then
    return nil, M.REFERENCE .. " paths must be absolute"
  end
  local port = value.port
  if port ~= nil and port ~= json.null and not (type(port) == "number" and port == math.floor(port) and port >= 0 and port <= 65535) then
    return nil, M.REFERENCE .. " has an invalid port"
  end
  local log_dir = value.logDir
  if log_dir ~= nil and log_dir ~= json.null and type(log_dir) ~= "string" then
    return nil, M.REFERENCE .. " has an invalid logDir"
  end
  return {
    secret_file = value.secretFile,
    discovery_dir = value.discoveryDir,
    port = type(port) == "number" and port or nil,
    log_dir = type(log_dir) == "string" and log_dir ~= "" and log_dir or nil,
  }
end

-- The ports to try, in order: from the reference's port to the end of Renoise's range, then the
-- range's start up to it. A port outside the range is ignored.
function M.candidate_ports(start)
  local ports, first = {}, 0
  if start and start >= M.FIRST_PORT and start < M.FIRST_PORT + M.PORTS then first = start - M.FIRST_PORT end
  for i = 0, M.PORTS - 1 do
    ports[#ports + 1] = M.FIRST_PORT + (first + i) % M.PORTS
  end
  return ports
end

-- Group or other access in a Unix mode, read either as st_mode or as octal digits written in
-- decimal; nil when it can't tell.
local function group_or_other_access(mode)
  if type(mode) ~= "number" then return nil end
  if mode >= 32768 then return mode % 64 ~= 0 end
  if mode < 1000 and not tostring(mode):find("[89]") then return mode % 100 ~= 0 end
  return nil
end
M.group_or_other_access = group_or_other_access

-- Reads the secret fresh for each connection, so a new one takes effect without a reload.
function M.secret(cfg)
  if not cfg then return nil, "not set up" end
  local text, err = read_small(cfg.secret_file)
  if not text then return nil, "secret file: " .. tostring(err) end
  local stat = not windows() and io.stat and io.stat(cfg.secret_file)
  if stat and group_or_other_access(stat.mode) then
    return nil, "secret file: other accounts can read it"
  end
  local secret, why = wire.secret_from_file_text(text)
  if not secret then return nil, "secret: " .. why end
  return secret
end

return M
