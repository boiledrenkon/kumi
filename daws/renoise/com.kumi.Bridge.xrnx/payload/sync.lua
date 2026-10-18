-- Observers for one song: attached on a new document, detached on release. A notifier only marks
-- what changed; the timer reads the current values, so a storm of notifications is one event.
--
-- Three kinds of watching:
--   watch          marks a key dirty (selection, transport, project): the flush sends an event if the value moved.
--   watch_list     keeps each insert, remove or swap of a list in order (the tracks), for events that say what shifted.
--   watch_structural  marks that refs stopped meaning what they meant (instruments, patterns, sequence, devices,
--                  samples, phrases): the flush bumps the epoch and watches the song anew.
-- Everything counts toward `changed`, the counter Kumi polls to know it should read again (line notifiers and
-- plain property observers only count). What changes the shape of the song moves `layout` too, at once: a read in
-- progress stops when it moves.

local M = {}
M.__index = M

-- guard(where, fn) wraps a notifier so it never raises. state: a table that outlives the song (the counter).
function M.new(guard, state)
  return setmetatable({ entries = {}, dirty = {}, last = {}, ops = {}, structural = nil, guard = guard, state = state or {} }, M)
end

local function count(self)
  self.state.changed = (self.state.changed or 0) + 1
end

local function reshape(self)
  self.state.layout = (self.state.layout or 0) + 1
end

local function add(self, observable, fn)
  observable:add_notifier(fn)
  self.entries[#self.entries + 1] = { observable = observable, fn = fn }
end

function M:watch(observable, key)
  add(self, observable, self.guard("notifier " .. key, function()
    self.dirty[key] = true
    count(self)
  end))
end

-- Only counts: a name, a colour, a mute.
function M:watch_changes(observable)
  add(self, observable, self.guard("notifier", function() count(self) end))
end

-- A list observable: its operations ({type = "insert" | "remove" | "swap", index, index2}) in the order they happened.
function M:watch_list(observable, key)
  add(self, observable, self.guard("list notifier " .. key, function(op)
    local ops = self.ops[key] or {}
    self.ops[key] = ops
    ops[#ops + 1] = { type = op and op.type, index = op and op.index, index2 = op and op.index2 }
    self.dirty[key] = true
    count(self)
    reshape(self)
  end))
end

-- Refs shift or vanish when this changes.
function M:watch_structural(observable, reason)
  add(self, observable, self.guard("notifier " .. reason, function()
    self.structural = self.structural or reason
    count(self)
    reshape(self)
  end))
end

-- A change that moves things inside what a read steps through (a pattern's number of lines).
function M:watch_layout(observable)
  add(self, observable, self.guard("notifier", function()
    count(self)
    reshape(self)
  end))
end

-- A pattern's lines: any change counts (the handler only counts; clearing a pattern fires hundreds).
function M:watch_lines(pattern)
  local fn = self.guard("line notifier", function() count(self) end)
  pattern:add_line_notifier(fn)
  self.entries[#self.entries + 1] = { remove = function() pattern:remove_line_notifier(fn) end }
end

function M:detach()
  for _, entry in ipairs(self.entries) do
    pcall(function()
      if entry.remove then
        entry.remove()
      elseif entry.observable:has_notifier(entry.fn) then
        entry.observable:remove_notifier(entry.fn)
      end
    end)
  end
  self.entries = {}
  self.dirty = {}
  self.last = {}
  self.ops = {}
  self.structural = nil
end

function M:attached()
  return #self.entries > 0
end

-- The marks since the last call.
function M:take_dirty()
  local dirty = self.dirty
  self.dirty = {}
  return dirty
end

-- The operations on a list since the last call, in order.
function M:take_ops(key)
  local ops = self.ops[key] or {}
  self.ops[key] = nil
  return ops
end

-- Why refs shifted since the last call, or nil.
function M:take_structural()
  local reason = self.structural
  self.structural = nil
  return reason
end

local function same(a, b)
  if type(a) ~= "table" or type(b) ~= "table" then return a == b end
  for k, v in pairs(a) do
    if b[k] ~= v then return false end
  end
  for k in pairs(b) do
    if a[k] == nil then return false end
  end
  return true
end

-- Whether value (a flat table) differs from the one last reported under key; remembers it.
function M:changed(key, value)
  if same(self.last[key], value) then return false end
  self.last[key] = value
  return true
end

return M
