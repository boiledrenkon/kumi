-- Lua run in Renoise for Kumi (run-lua): a chunk run in one callback named by the change's description, so Renoise's own
-- undo takes back whatever it did, whole. Kumi can't take it back: it doesn't know what the chunk changed.
--
-- A chunk runs with the whole of Renoise's API (`renoise`, and `song` for renoise.song()), `print` collected for the answer,
-- for at most `timeoutMs` (500 by default, 800 at most: Renoise's watchdog speaks up after about 10 s, and a callback should
-- stay well under a second). A chunk that runs past it is stopped where it is: what it changed so far stays. The limit is a
-- count hook that stops only the chunk's own code (the tool's notifiers run whole: see `guard` in bridge.lua), and the JIT is
-- off while the chunk runs, so code it loads itself is counted too. A long call into C (a string pattern that backtracks) is not.

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

-- What a chunk may write, read from its text (its comments and strings blanked first): "none" when it sets no field but
-- of tables it made itself and calls only methods that read; "unkept" when all it sets or calls is what Renoise's undo
-- doesn't keep (mute, solo, selection, transport); "any" otherwise. Only "none" is taken as a chunk that changed nothing.
local READS = {}
for name in ([[track pattern instrument sample phrase line lines_in_range note_column effect_column device parameter
  column_is_muted column_name sequence_is_start_of_section sequence_section_name track_sequence_slot_is_muted find_automation
  sample_modulation_set sample_device_chain can_undo can_redo is_undo_redoing has_line_notifier has_notifier
  format sub match gmatch find lower upper len rep gsub byte reverse]]):gmatch("%S+") do READS[name] = true end
local UNKEPT_FIELDS = {}
for name in ([[mute_state solo_state selected_track_index selected_instrument_index selected_line_index selected_sequence_index
  selected_sample_index selected_phrase_index selected_track_device_index selected_note_column_index selected_effect_column_index
  follow_player edit_mode edit_step octave loop_pattern loop_block_enabled loop_range loop_sequence_range metronome_enabled
  metronome_precount_enabled metronome_precount_bars record_quantize_enabled record_quantize_lines wrapped_pattern_edit
  single_track_edit_mode keyboard_velocity_enabled keyboard_velocity record_parameter_mode]]):gmatch("%S+") do UNKEPT_FIELDS[name] = true end
local UNKEPT_METHODS = { start = true, stop = true, start_at = true, panic = true, show_status = true }

-- The code with its comments blanked and its strings emptied (so nothing in them reads as code).
local function blanked(code)
  local out, i, n = {}, 1, #code
  while i <= n do
    local at = code:find("[%-%[\"']", i)
    if not at then out[#out + 1] = code:sub(i) break end
    out[#out + 1] = code:sub(i, at - 1)
    local c = code:sub(at, at)
    local long = code:match("^%[(=*)%[", c == "-" and at + 2 or at)
    if c == "-" and code:sub(at + 1, at + 1) ~= "-" then
      out[#out + 1], i = "-", at + 1
    elseif c == "-" then
      local close = long and code:find("]" .. long .. "]", at + 4 + #long, true)
      i = long and (close and close + #long + 2 or n + 1) or (code:find("\n", at, true) or n + 1)
      out[#out + 1] = " "
    elseif c == "[" and not long then
      out[#out + 1], i = "[", at + 1
    elseif c == "[" then
      local close = code:find("]" .. long .. "]", at + 2 + #long, true)
      out[#out + 1], i = '""', close and close + #long + 2 or n + 1
    else
      local j = at + 1
      while j <= n do
        local d = code:sub(j, j)
        if d == "\\" then j = j + 2 elseif d == c or d == "\n" then break else j = j + 1 end
      end
      out[#out + 1], i = '""', j + 1
    end
  end
  return table.concat(out)
end

-- The target of an assignment that ends at `e`: its root name ("?" when it is a call's result) and the field it sets, or
-- nil for a plain name or a table constructor's key; and where the text before it ends.
local function target_at(text, e)
  local i, field = e, nil
  while i >= 1 do
    local c = text:sub(i, i)
    if c == "]" then
      local depth = 0
      repeat
        local d = text:sub(i, i)
        if d == "]" then depth = depth + 1 elseif d == "[" then depth = depth - 1 end
        i = i - 1
      until depth == 0 or i < 1
      field = field or "[]"
      while i >= 1 and text:sub(i, i):match("%s") do i = i - 1 end
      if not text:sub(i, i):match("[%w_%]%)]") then return nil, nil, i end
    elseif c:match("[%w_]") then
      local last = i
      while i >= 1 and text:sub(i, i):match("[%w_]") do i = i - 1 end
      local name = text:sub(i + 1, last)
      local j = i
      while j >= 1 and text:sub(j, j):match("%s") do j = j - 1 end
      if text:sub(j, j) == "." and text:sub(j - 1, j - 1) ~= "." then
        field = field or name
        i = j - 1
        while i >= 1 and text:sub(i, i):match("%s") do i = i - 1 end
      else
        if field == nil or name:match("^%d") then return nil, nil, j end
        return name, field, j
      end
    else
      return field and "?" or nil, field, i
    end
  end
  return field and "?" or nil, field, 0
end

local function writes_of(code)
  local text = blanked(code)
  -- (code it loads or runs itself, or tables it reaches round: anything)
  for _, word in ipairs({ "load", "dofile", "require", "rawset", "setmetatable", "setfenv", "debug" }) do
    if text:find(word, 1, true) then return "any" end
  end
  local made, class = {}, "none"
  for name in text:gmatch("local%s+([%a_][%w_]*)%s*=%s*{") do made[name] = true end
  local function saw(unkept)
    if not unkept then class = "any" elseif class == "none" then class = "unkept" end
  end
  for name in text:gmatch(":%s*([%a_][%w_]*)%s*[%(\"{]") do
    if UNKEPT_METHODS[name] then saw(true) elseif not READS[name] then saw(false) end
  end
  for at in text:gmatch("()=") do
    if text:sub(at + 1, at + 1) ~= "=" and not text:sub(at - 1, at - 1):match("[=~<>]") then
      local e = at - 1
      while e >= 1 do
        while e >= 1 and text:sub(e, e):match("%s") do e = e - 1 end
        local root, field, before = target_at(text, e)
        if root and not made[root] then saw(UNKEPT_FIELDS[field] == true) end
        if text:sub(before, before) ~= "," then break end
        e = before - 1
      end
    end
    if class == "any" then break end
  end
  return class
end
M.writes_of = writes_of

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
local UNKEPT_NOTE = "What it changed (mute, solo, selection or transport) is nothing Renoise's undo takes back, and Kumi can't either."

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
  local writes = writes_of(input.code)
  local before = env.changes and env.changes() or 0
  local deadline = env.now_ms() + limit
  song:describe_undo(desc)
  -- (a loop the JIT compiles never sees the hook, the chunk's own or one it loads: the JIT is off while it runs)
  local jitted = jit and jit.status and jit.status()
  if jitted then
    jit.off()
    jit.flush()
  end
  -- (inside the tool's own notifiers the stop waits for the chunk's code: they run whole)
  local in_notifier = env.in_notifier or function() return false end
  debug.sethook(function()
    if env.now_ms() > deadline and not in_notifier() then error(string.format("Kumi stopped it: it ran longer than %d ms", limit), 2) end
  end, "", 1000)
  local ok, result = pcall(chunk)
  debug.sethook()
  if jitted then jit.on() end
  local told = (env.changes and env.changes() or 0) ~= before
  -- (what its text can't write didn't change; what the tool was told of did; the rest may have)
  local changed = writes == "any" or (writes == "unkept" and told) or (writes == "none" and told)
  local unkept = writes == "unkept"
  local applied = { changed = changed, printed = printed, writes = writes }
  local note = unkept and UNKEPT_NOTE or NOTE
  if changed and not unkept and not told then note = "It may have changed the song: Kumi can't see all a chunk does. " .. NOTE end
  local display = { title = "Ran Lua: " .. first_line(input.code), note = note }
  if ok then
    local value = plain(result, 0)
    if #json.encode({ v = value }) > MOST_RESULT then value = "(a value of more than 256 KB, not sent)" end
    applied.result = value
  else
    applied.failed = { message = tostring(result) }
    display.note = string.format("It stopped with an error (%s)%s. %s", tostring(result),
      changed and "; what it changed before that stays" or "", unkept and UNKEPT_NOTE or NOTE)
  end
  if not changed then display.unchanged = true end
  return {
    applied = applied, display = display, target = { ref = refs.make(env.epoch(), "song", 1), kind = "song" },
    -- (Renoise makes a step only of what its undo keeps: none when nothing changed, or only mute, solo, selection or transport)
    undo = { desc = desc, entry = changed and not unkept }, prior = {},
  }
end

local function restore()
  fail("daw-only", "Kumi can't take back what a Lua chunk did: Renoise's own undo can (Edit > Undo)")
end

base.register({ kind = "run-lua", custom = true, preview = preview, apply = apply, restore = restore })

return M
