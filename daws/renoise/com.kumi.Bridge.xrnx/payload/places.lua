-- Where an object of one of the song's lists is now, from where it was when a change kept it: a track's devices, the pattern
-- pool, the instruments. Each list keeps a log of what its notifier told (insert, remove, swap), numbered; a change keeps a
-- place as the object's number and a mark (the log's id and number then). The tracks have their own log (the layout's shifts,
-- bridge.lua), and the sequence none: Renoise's sequence notifier names the wrong slot when patterns repeat (measured).
--
-- A log that missed something (a song opened, a notifier that didn't say what moved, the tool watching the song anew, more
-- operations than it keeps) starts again: a place from before that is unknown, and the change that holds it says so and
-- touches nothing of it.

local M = {}

-- The operations a log keeps.
M.KEPT = 256

function M.new(id)
  return { id = tostring(id), serial = 0, ops = {}, from = 0 }
end

-- What the log has seen up to now, for a place to be followed from.
function M.mark(log)
  return log.id .. ":" .. log.serial
end

-- The log missed something: no place from before can be followed.
function M.gap(log)
  log.serial = log.serial + 1
  log.ops, log.from = {}, log.serial
end

-- An operation the notifier told: `op` = {type = "insert" | "remove" | "swap", index, index2}. Anything else is a gap.
function M.note(log, op)
  local fine = type(op) == "table" and type(op.index) == "number"
    and (op.type == "insert" or op.type == "remove" or (op.type == "swap" and type(op.index2) == "number"))
  if not fine then return M.gap(log) end
  log.serial = log.serial + 1
  log.ops[#log.ops + 1] = { serial = log.serial, type = op.type, index = op.index, index2 = op.index2 }
  if #log.ops > M.KEPT then
    table.remove(log.ops, 1)
    log.from = log.ops[1].serial - 1
  end
end

-- The operations since `mark`, oldest first; nil when they aren't all kept (or the mark is another log's).
local function since(log, mark)
  if type(mark) ~= "string" then return nil end
  local id, serial = mark:match("^(.*):(%d+)$")
  serial = tonumber(serial)
  if id ~= log.id or not serial or serial < log.from or serial > log.serial then return nil end
  local out = {}
  for _, op in ipairs(log.ops) do
    if op.serial > serial then out[#out + 1] = op end
  end
  return out
end

-- The object that was at `n` when `mark` was made: its number now, or nil and "gone" (it was removed) or "unknown". An object
-- Kumi removed and put back is followed to where it came back (`back`).
function M.item(log, n, mark)
  local ops = since(log, mark)
  if not ops or type(n) ~= "number" then return nil, "unknown" end
  local j = 1
  while j <= #ops do
    local op = ops[j]
    if op.type == "insert" then
      if n >= op.index then n = n + 1 end
    elseif op.type == "remove" then
      if n == op.index then
        local back
        for k = j + 1, #ops do
          if ops[k].back == op.serial then back = k break end
        end
        if not back then return nil, "gone" end
        n, j = ops[back].index, back
      elseif n > op.index then
        n = n - 1
      end
    elseif n == op.index then
      n = op.index2
    elseif n == op.index2 then
      n = op.index
    end
    j = j + 1
  end
  return n
end

-- n-instruments: Kumi put back an object it had removed (`removed`: the mark taken just after the remove): of the inserts since,
-- the one whose object is at `at` now is that object, so a place from before the remove follows it. False when none is.
function M.back(log, removed, at)
  local ops = since(log, removed)
  local serial = type(removed) == "string" and tonumber(removed:match(":(%d+)$")) or nil
  if not ops or not serial or type(at) ~= "number" then return false end
  for j = #ops, 1, -1 do
    local op = ops[j]
    if op.type == "insert" then
      local n = op.index
      for k = j + 1, #ops do
        local later = ops[k]
        if later.type == "insert" then
          if n >= later.index then n = n + 1 end
        elseif later.type == "remove" then
          if n == later.index then n = nil break end
          if n > later.index then n = n - 1 end
        elseif n == later.index then
          n = later.index2
        elseif n == later.index2 then
          n = later.index
        end
      end
      if n == at then
        op.back = serial
        return true
      end
    end
  end
  return false
end

-- Where an object deleted from place `n` (when `mark` was made, after it went) goes back: just after the object that was
-- before it, wherever that is now; when that one is gone too, after the one before it (`first` is the place of the list's
-- first object, which never moves: a track's mixer device). Nil and "unknown" when the log can't say.
function M.after(log, n, mark, first)
  local ops = since(log, mark)
  if not ops or type(n) ~= "number" then return nil, "unknown" end
  local anchor = n - 1
  for _, op in ipairs(ops) do
    if op.type == "insert" then
      if op.index <= anchor then anchor = anchor + 1 end
    elseif op.type == "remove" then
      if op.index <= anchor then anchor = anchor - 1 end
    elseif anchor == op.index then
      anchor = op.index2
    elseif anchor == op.index2 then
      anchor = op.index
    end
    if first and anchor < first then anchor = first end
  end
  return anchor + 1
end

return M
