-- The discovery file Kumi reads to find this bridge: written when the server listens, rewritten
-- when the song changes, removed when the tool unloads.

local import = ...
local json = import("json")

local M = {}

local function ensure_dir(path)
  if io.exists(path) then return true end
  local parent = path:match("^(.*)[/\\][^/\\]+[/\\]?$")
  if parent and parent ~= "" and not io.exists(parent) then ensure_dir(parent) end
  return os.mkdir(path)
end

-- Writes value as JSON to path through a temporary file, so a reader never sees half of it.
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
  if os.platform and os.platform() == "WINDOWS" then os.remove(path) end
  local ok, rename_err = os.rename(temp, path)
  if not ok then
    os.remove(temp)
    return nil, rename_err
  end
  return true
end

function M.remove(path)
  if path then os.remove(path) end
end

return M
