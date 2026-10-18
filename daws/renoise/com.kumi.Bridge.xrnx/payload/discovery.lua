-- The discovery file Kumi reads to find this bridge: <dir>/renoise-<pid>-<key>.json, written once the
-- server listens, rewritten when the song changes, removed when the tool unloads.

local import = ...
local json = import("json")
local sha256 = import("sha256")

local M = {}

M.SCHEMA = "kumi-daw-discovery/1"

local function windows()
  return os.platform ~= nil and os.platform() == "WINDOWS"
end

-- Renoise's io functions take UTF-8 paths, but os.rename and os.remove are stock Lua: on Windows they take the ANSI code
-- page, so a path with other letters (a user folder like "Kümi") is not found by them. os.move is Renoise's own.
local function ansi_unsafe(path)
  return windows() and path:find("[\128-\255]") ~= nil
end

-- The first 16 hex of the SHA-256 of the resource folder's path as given.
function M.key(resource)
  return sha256.hex(resource):sub(1, 16)
end

function M.file_name(pid, resource)
  return "renoise-" .. string.format("%d", pid) .. "-" .. M.key(resource) .. ".json"
end

local function ensure_dir(path)
  if io.exists(path) then return true end
  local parent = path:match("^(.*)[/\\][^/\\]+[/\\]?$")
  if parent and parent ~= "" and not io.exists(parent) then ensure_dir(parent) end
  local ok = os.mkdir(path)
  if ok and not windows() and io.chmod then io.chmod(path, 700) end
  return ok
end

-- Writes value as JSON through a temporary file, owner-only, so a reader never sees half of it.
function M.write(path, value)
  local text, err = json.encode(value)
  if not text then return nil, err end
  local dir = path:match("^(.*)[/\\][^/\\]+$")
  if dir and not ensure_dir(dir) then return nil, "could not create " .. dir end
  local temp = path .. ".tmp"
  local f, open_err = io.open(temp, "wb")
  if not f then return nil, open_err end
  f:write(text, "\n")
  f:close()
  if not windows() and io.chmod then io.chmod(temp, 600) end
  local unsafe = ansi_unsafe(path)
  if windows() and not unsafe then os.remove(path) end -- (Windows' rename doesn't replace)
  local ok, rename_err
  if unsafe and os.move then
    ok, rename_err = os.move(temp, path)
  else
    ok, rename_err = os.rename(temp, path)
  end
  if not ok and unsafe then
    -- No UTF-8 way to replace or delete a file here: the file is written where it goes (a reader that finds it half written
    -- reads again), and the temporary one emptied.
    local inplace = io.open(path, "wb")
    if inplace then
      inplace:write(text, "\n")
      inplace:close()
      ok = true
    end
    local empty = io.open(temp, "wb")
    if empty then empty:close() end
  elseif not ok then
    os.remove(temp)
  end
  if not ok then return nil, rename_err end
  return true
end

-- Removes the file. On Windows a path with letters outside ASCII can't be deleted from here: the file stays, naming a process
-- that is gone, and Kumi sweeps such files.
function M.remove(path)
  if path then os.remove(path) end
end

return M
