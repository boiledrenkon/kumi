-- The bridge's own log, in Kumi's state folder. Lines wait in memory until it's configured and are
-- written on the next flush (errors at once). Never logs secrets.

local M = {}

M.MAX_BYTES = 1024 * 1024
M.MAX_PENDING = 500

local path
local pending, count = {}, 0

function M.configure(file)
  path = file
  M.flush()
end

function M.path()
  return path
end

function M.flush()
  if not path or count == 0 then return end
  local f = io.open(path, "ab")
  if not f then return end
  local size = f:seek("end")
  if size and size > M.MAX_BYTES then
    f:close()
    os.remove(path .. ".1")
    os.rename(path, path .. ".1")
    f = io.open(path, "ab")
    if not f then return end
  end
  f:write(table.concat(pending, "\n", 1, count), "\n")
  f:close()
  pending, count = {}, 0
end

function M.write(level, message)
  local line = os.date("!%Y-%m-%dT%H:%M:%SZ") .. " " .. level .. " " .. tostring(message):gsub("\n", "\n    ")
  if count >= M.MAX_PENDING then
    table.remove(pending, 1)
    count = count - 1
  end
  count = count + 1
  pending[count] = line
  if level == "error" then M.flush() end
end

function M.pending()
  return { unpack(pending, 1, count) }
end

return M
