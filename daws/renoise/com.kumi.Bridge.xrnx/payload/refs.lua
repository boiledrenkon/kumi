-- Refs: how the tool names things in the song. Renoise has no ids, so a ref is positional:
-- <epoch>:<kind>:<indices>, 1-based as Renoise shows them (E:track:3, E:sample:2.5, E:param:3.2.7).
-- The epoch changes whenever refs stop meaning what they meant (a song switch, a restructure the tool
-- can't map), and a ref from another epoch is refused rather than read as something else.

local M = {}

function M.make(epoch, kind, ...)
  return epoch .. ":" .. kind .. ":" .. table.concat({ ... }, ".")
end

-- {epoch = text, kind = text, n = {numbers}}, or nil and why.
function M.parse(text)
  if type(text) ~= "string" then return nil, "not text" end
  local epoch, kind, rest = text:match("^([^:]+):([^:]+):(%d[%d%.]*)$")
  if not epoch then return nil, "not a ref" end
  local numbers = {}
  for digits in rest:gmatch("%d+") do numbers[#numbers + 1] = tonumber(digits) end
  return { epoch = epoch, kind = kind, n = numbers }
end

-- A ref of `kind` with `count` indices from the current epoch, or an error table for the request to fail with.
---@return integer[]
function M.expect(text, kind, count, epoch)
  local ref = M.parse(text)
  if not ref then error({ code = "bad-params", message = "not a ref: " .. tostring(text) }, 0) end
  if ref.kind ~= kind or #ref.n ~= count then
    error({ code = "bad-params", message = "expected a " .. kind .. " ref, got " .. text }, 0)
  end
  if ref.epoch ~= epoch then
    error({ code = "stale", message = "that ref is from an earlier epoch (" .. ref.epoch .. "; now " .. epoch .. "): read again" }, 0)
  end
  return ref.n
end

return M
