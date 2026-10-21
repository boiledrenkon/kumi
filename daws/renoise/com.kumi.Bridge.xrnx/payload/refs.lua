-- Refs: how the tool names things in the song. Renoise has no ids, so a ref is positional:
-- <epoch>:<kind>:<indices>, Lua's indices from 1 (E:track:3, E:sample:2.5, E:param:3.2.7). Kumi shows the model a short name
-- for each; the numbers Renoise shows (lines, patterns, instruments and samples from 00) are the rows'.
-- The epoch changes whenever refs stop meaning what they meant (a song switch, a restructure the tool
-- can't map), and a ref from another epoch is refused rather than read as something else.

local M = {}

function M.make(epoch, kind, ...)
  return epoch .. ":" .. kind .. ":" .. table.concat({ ... }, ".")
end

-- {epoch = text, kind = text, n = {numbers}}, or nil and why.
function M.parse(text)
  if type(text) ~= "string" then return nil, "not text" end
  local epoch, kind, rest = text:match("^([^:]+):([^:]+):([%d%.]+)$")
  if not epoch then return nil, "not a ref" end
  -- (indices count from 1, with no zeros in front and nothing between or after the dots: the tool makes no other)
  local numbers = {}
  for digits in (rest .. "."):gmatch("([^.]*)%.") do
    if not digits:match("^[1-9]%d*$") then return nil, "not a ref" end
    numbers[#numbers + 1] = tonumber(digits)
  end
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

-- Refs that name a track, or something on one (its devices and their parameters, a pattern's track): the number that is the track.
local TRACK_KINDS = { track = 1, device = 1, param = 1, ptrack = 2, slot = 1 }

-- Walks `value`, giving each leaf (a string, a number) to `leaf(value, key)`, which answers the value to put there and whether it
-- is another. A table is copied only when something in it changed: JSON's null is a table that must stay the one it is.
local function rebuild(value, key, leaf)
  if type(value) == "table" then
    local copy
    for k, v in pairs(value) do
      local moved, did = rebuild(v, k, leaf)
      if did then
        if not copy then
          copy = {}
          for k2, v2 in pairs(value) do copy[k2] = v2 end
          setmetatable(copy, getmetatable(value))
        end
        copy[k] = moved
      end
    end
    if copy then return copy, true end
    return value, false
  end
  return leaf(value, key)
end

-- `params` with every ref of an earlier epoch than `now` that `survives(epoch, kind)` says still names the same thing made one of
-- `now`'s. (Kumi keeps the names of what an epoch that moved for something else left alone, and so the refs behind them.) Also
-- answers the epochs of the refs left as they were.
function M.relabel_surviving(params, now, survives)
  local left = {}
  local moved = rebuild(params, nil, function(value, key)
    if type(value) ~= "string" or key == "epoch" then return value, false end
    local ref = M.parse(value)
    if not ref or ref.epoch == now then return value, false end
    if survives(ref.epoch, ref.kind) then return M.make(now, ref.kind, unpack(ref.n)), true end
    left[ref.epoch] = true
    return value, false
  end)
  return moved, left
end

-- The shifts of the track list since layout `since`: each { layout, type = "insert" | "remove" | "swap", index, index2? (a swap's
-- other place), back? (an insert that made again the track a remove took: that remove's layout) }, oldest first, one per layout.
-- Returns the new number for the track `n`, or nil when the track is gone. A track removed and made again (`back`) is followed.
function M.shift(n, shifts, since)
  local k = 1
  while k <= #shifts do
    local s = shifts[k]
    if s.layout > since then
      if s.type == "insert" then
        if n >= s.index then n = n + 1 end
      elseif s.type == "swap" then
        if n == s.index then n = s.index2 elseif n == s.index2 then n = s.index end
      elseif n == s.index then
        local again
        for j = k + 1, #shifts do
          if shifts[j].back == s.layout then again = j break end
        end
        if not again then return nil end
        n, k = shifts[again].index, again
      elseif n > s.index then
        n = n - 1
      end
    end
    k = k + 1
  end
  return n
end

-- The track a remove took (at layout `removed`) made again (Renoise's redo): of the inserts since, the one whose track is at `at`
-- now is linked back to it (`back`), so a track number from before the remove follows it. False when none is.
function M.back(shifts, removed, at)
  for j = #shifts, 1, -1 do
    local s = shifts[j]
    if s.layout <= removed then break end
    if s.type == "insert" then
      local n = s.index
      for k = j + 1, #shifts do
        local later = shifts[k]
        if later.type == "insert" then
          if n >= later.index then n = n + 1 end
        elseif later.type == "swap" then
          if n == later.index then n = later.index2 elseif n == later.index2 then n = later.index end
        elseif n == later.index then
          n = nil
          break
        elseif n > later.index then
          n = n - 1
        end
      end
      if n == at then
        s.back = removed
        return true
      end
    end
  end
  return false
end

-- The same for a place among the tracks rather than a track: where a track deleted from there would go back. A track removed
-- at the place leaves it where it is (the next one took it); a swap moves no place.
function M.shift_place(n, shifts, since)
  for _, s in ipairs(shifts) do
    if s.layout > since then
      if s.type == "insert" then
        if n >= s.index then n = n + 1 end
      elseif s.type == "remove" then
        if n > s.index then n = n - 1 end
      end
    end
  end
  return n
end

-- Whether a request names a track, or something on one (a ref of kind track, device or param, of any epoch, or a number
-- under `track`): only those move with the track layout.
function M.names_tracks(params)
  local found = false
  local function walk(value, key)
    if found then return end
    if type(value) == "string" then
      local ref = M.parse(value)
      if ref and TRACK_KINDS[ref.kind] then found = true end
    elseif type(value) == "number" and key == "track" then
      found = true
    elseif type(value) == "table" then
      for k, v in pairs(value) do walk(v, k) end
    end
  end
  walk(params, nil)
  return found
end

-- `params` with every track number carried through the shifts since layout `since`: refs of the current epoch that name a
-- track (kinds track, device, param, ptrack and slot) and a number under `track`. Returns the new params, or nil and an error table
-- (code `gone`) naming the first track that was deleted meanwhile.
function M.carry(params, epoch, shifts, since)
  local gone
  local carried = rebuild(params, nil, function(value, key)
    if type(value) == "string" then
      local ref = M.parse(value)
      if ref and ref.epoch == epoch and TRACK_KINDS[ref.kind] then
        local at = TRACK_KINDS[ref.kind]
        local moved = M.shift(ref.n[at], shifts, since)
        if not moved then
          gone = gone or { code = "gone", message = "track " .. ref.n[at] .. " (" .. value .. ") was deleted since that ref was made" }
          return value, false
        end
        ref.n[at] = moved
        return M.make(ref.epoch, ref.kind, unpack(ref.n)), true
      end
    elseif type(value) == "number" and key == "track" then
      local moved = M.shift(value, shifts, since)
      if not moved then
        gone = gone or { code = "gone", message = "track " .. value .. " was deleted since that number was made" }
        return value, false
      end
      return moved, true
    end
    return value, false
  end)
  if gone then return nil, gone end
  return carried
end

return M
