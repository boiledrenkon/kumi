-- Reads bridge-reference.json, which Kumi's installer writes into the bundle, and the secret it
-- points to. The reference holds no secret itself.

local import = ...
local json = import("json")

local M = {}

M.REFERENCE = "bridge-reference.json"
M.MIN_SECRET = 32

local function read_file(path)
  local f, err = io.open(path, "rb")
  if not f then return nil, err end
  local text = f:read("*a")
  f:close()
  return text
end

local function absolute(path)
  return type(path) == "string" and (path:match("^/") or path:match("^%a:[/\\]") or path:match("^\\\\")) ~= nil
end

-- Returns {port, secret_file, state_dir}, or nil and why.
function M.load(bundle_path)
  local text, err = read_file(bundle_path .. M.REFERENCE)
  if not text then return nil, "no " .. M.REFERENCE .. " (" .. tostring(err) .. ")" end
  local value, decode_err = json.decode(text)
  if type(value) ~= "table" then return nil, M.REFERENCE .. " is not JSON: " .. tostring(decode_err) end
  if value.version ~= 1 then return nil, M.REFERENCE .. " has an unknown version" end
  local port = value.port
  if type(port) ~= "number" or port ~= math.floor(port) or port < 1 or port > 65535 then
    return nil, M.REFERENCE .. " has no valid port"
  end
  if not absolute(value.secretFile) then return nil, M.REFERENCE .. " has no absolute secretFile" end
  if not absolute(value.stateDir) then return nil, M.REFERENCE .. " has no absolute stateDir" end
  return { port = port, secret_file = value.secretFile, state_dir = value.stateDir }
end

-- Group or other access on a Unix mode, when the mode is readable either as st_mode or as octal
-- digits written in decimal; nil when it can't tell.
local function group_or_other_access(mode)
  if type(mode) ~= "number" then return nil end
  if mode >= 32768 then return mode % 64 ~= 0 end
  if mode < 1000 and not tostring(mode):find("[89]") then return mode % 100 ~= 0 end
  return nil
end
M.group_or_other_access = group_or_other_access

-- Reads the secret fresh each time, so a rotated secret takes effect on the next connection.
function M.secret(cfg)
  if not cfg then return nil, "not configured" end
  local path = cfg.secret_file
  if io.stat and not (os.platform and os.platform() == "WINDOWS") then
    local stat = io.stat(path)
    if not stat then return nil, "secret file is missing" end
    if stat.type and stat.type ~= "file" then return nil, "secret file is not a regular file" end
    if group_or_other_access(stat.mode) then return nil, "secret file is readable by others" end
  end
  local text = read_file(path)
  if not text then return nil, "secret file is missing" end
  if text:sub(-1) == "\n" then text = text:sub(1, -2) end
  if #text < M.MIN_SECRET or text:find("%s") then return nil, "secret is invalid" end
  return text
end

return M
