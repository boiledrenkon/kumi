-- The bridge's own log, in Kumi's state folder. Lines wait in memory until it's configured and are
-- written on the next flush (errors at once). Never logs secrets.

local M = {}

M.MAX_BYTES = 1024 * 1024
M.MAX_PENDING = 500

local path, tag
local pending, count = {}, 0

function M.configure(file)
  path = file
  M.flush()
end

function M.path()
  return path
end

-- Said on every line after the level, so lines of two Renoises sharing a log folder can be told apart.
function M.set_tag(text)
  tag = text
end

M.KEEP_SECONDS = 7 * 24 * 60 * 60

-- Each Renoise writes a log of its own in the folder (two writing one file would interleave and race on rotating it),
-- so logs of other launches pile up: those not touched for a week go. `keep` is this one's path.
function M.prune(dir, keep)
  pcall(function()
    if not (os.filenames and io.stat) then return end
    local now = os.time()
    for _, name in ipairs(os.filenames(dir, { "*.log", "*.log.1" })) do
      local path = dir .. "/" .. name
      if name:find("^renoise%-bridge") and path ~= keep and path ~= keep .. ".1" then
        local stat = io.stat(path)
        if stat and stat.mtime and now - stat.mtime > M.KEEP_SECONDS then os.remove(path) end
      end
    end
  end)
end

function M.flush()
  if not path or count == 0 then return end
  local f = io.open(path, "ab")
  if not f then return end
  local size = f:seek("end")
  if size and size > M.MAX_BYTES then
    f:close()
    local on_windows = os.platform ~= nil and os.platform() == "WINDOWS"
    if on_windows and path:find("[\128-\255]") then
      -- (stock os.rename and os.remove take Windows' ANSI code page, not the UTF-8 of this path: move it with Renoise's own
      -- os.move, or start the log again)
      if not (os.move and os.move(path, path .. ".1")) then
        local fresh = io.open(path, "wb")
        if fresh then fresh:close() end
      end
    else
      os.remove(path .. ".1")
      os.rename(path, path .. ".1")
    end
    f = io.open(path, "ab")
    if not f then return end
  end
  f:write(table.concat(pending, "\n", 1, count), "\n")
  f:close()
  pending, count = {}, 0
end

function M.write(level, message)
  local line = os.date("!%Y-%m-%dT%H:%M:%SZ") .. " " .. level .. (tag and (" [" .. tag .. "] ") or " ") .. tostring(message):gsub("\n", "\n    ")
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
