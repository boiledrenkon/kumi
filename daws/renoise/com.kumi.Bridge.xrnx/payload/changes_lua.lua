-- Lua run in Renoise for Kumi (run-lua): a chunk run in one callback named by the change's description, so Renoise's own
-- undo takes back whatever it did, whole. Kumi can't take it back: it doesn't know what the chunk changed.
--
-- A chunk runs with the whole of Renoise's API (`renoise`, and `song` for renoise.song()), `print` collected for the answer,
-- for at most `timeoutMs` (500 by default, 800 at most: Renoise's watchdog speaks up after about 10 s, and a callback should
-- stay well under a second). A chunk that runs past it is stopped where it is: what it changed so far stays.

local import = ...
local refs = import("refs")
local base = import("changes_base")
local json = import("json")

local bad, fail = base.bad, base.fail
local M = {}

M.DEFAULT_MS, M.MOST_MS = 500, 800
local MOST_CODE = 256 * 1024
local MOST_PRINTED = 200
local MOST_RESULT = 256 * 1024

local function first_line(code)
  for line in code:gmatch("[^\n]+") do
    local text = line:gsub("^%s+", ""):gsub("%s+$", "")
    if text ~= "" then
      if #text > 60 then text = text:sub(1, 59) .. "\226\128\166" end
      return text
    end
  end
  return "(nothing)"
end

local function compile(input)
  local code = input.code
  if type(code) ~= "string" or code == "" then bad("code: the Lua to run") end
  if #code > MOST_CODE then bad("code is longer than 256 KB") end
  local chunk, err = loadstring(code, "=kumi")
  if not chunk then bad("the Lua doesn't compile: " .. tostring(err)) end
  local limit = input.timeoutMs
  if limit == nil then limit = M.DEFAULT_MS end
  if type(limit) ~= "number" or limit < 1 or limit > M.MOST_MS then bad(string.format("timeoutMs is from 1 to %d", M.MOST_MS)) end
  return chunk, limit
end

-- A value as JSON can carry it: tables copied (to a depth), Renoise's objects and functions as their names.
local function plain(value, depth)
  local kind = type(value)
  if kind == "nil" or kind == "boolean" or kind == "string" then return value end
  if kind == "number" then return (value == value and value ~= math.huge and value ~= -math.huge) and value or tostring(value) end
  if kind ~= "table" or depth > 6 then return tostring(value) end
  local out, n = {}, 0
  for k, v in pairs(value) do
    n = n + 1
    if n > 1000 then break end
    out[type(k) == "number" and k or tostring(k)] = plain(v, depth + 1)
  end
  return out
end

local NOTE = "Only Renoise's own undo can take back what it did (Edit > Undo); Kumi can't."

local function preview(song, env, input)
  compile(input)
  return {
    target = { ref = refs.make(env.epoch(), "song", 1), kind = "song" }, prior = {}, fence = {},
    display = { title = "Ran Lua: " .. first_line(input.code), note = NOTE },
  }
end

local function apply(song, env, input, fence, desc)
  local chunk, limit = compile(input)
  local printed = {}
  local scope = setmetatable({
    song = song,
    print = function(...)
      if #printed >= MOST_PRINTED then return end
      local parts = {}
      for i = 1, select("#", ...) do parts[i] = tostring((select(i, ...))) end
      printed[#printed + 1] = table.concat(parts, "\t")
    end,
  }, { __index = _G })
  setfenv(chunk, scope)
  -- (a loop the JIT compiles would never see the hook: the chunk runs interpreted)
  if jit and jit.off then jit.off(chunk, true) end
  local before = env.changes and env.changes() or 0
  local deadline = env.now_ms() + limit
  song:describe_undo(desc)
  debug.sethook(function()
    if env.now_ms() > deadline then error(string.format("Kumi stopped it: it ran longer than %d ms", limit), 2) end
  end, "", 1000)
  local ok, result = pcall(chunk)
  debug.sethook()
  local changed = (env.changes and env.changes() or 0) ~= before
  local applied = { changed = changed, printed = printed }
  local display = { title = "Ran Lua: " .. first_line(input.code), note = NOTE }
  if ok then
    local value = plain(result, 0)
    if #json.encode({ v = value }) > MOST_RESULT then value = "(a value of more than 256 KB, not sent)" end
    applied.result = value
  else
    applied.failed = { message = tostring(result) }
    display.note = string.format("It stopped with an error (%s)%s. %s", tostring(result),
      changed and "; what it changed before that stays" or "", NOTE)
  end
  if not changed then display.unchanged = true end
  return {
    applied = applied, display = display, target = { ref = refs.make(env.epoch(), "song", 1), kind = "song" },
    -- (Renoise makes a step only of what changed: when nothing did, there is none for its undo)
    undo = { desc = desc, entry = changed }, prior = {},
  }
end

local function restore()
  fail("daw-only", "Kumi can't take back what a Lua chunk did: Renoise's own undo can (Edit > Undo)")
end

base.register({ kind = "run-lua", custom = true, preview = preview, apply = apply, restore = restore })

return M
