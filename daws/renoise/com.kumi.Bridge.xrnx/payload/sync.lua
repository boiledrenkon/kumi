-- Observers for one song: attached on a new document, detached on release.
--
-- A notifier does the least that keeps the tool true, at once, because a request can arrive between it and the next tick:
--   watch            marks a key dirty (selection, transport, project): the tick sends an event if the value moved.
--   watch_tracks     hands each insert, remove or swap of the track list to `hooks.tracks`, which moves the layout (or the
--                    epoch) and tells Kumi before any later answer.
--   watch_structural hands a change refs index into (instruments, patterns, the sequence, devices, samples, phrases) to
--                    `hooks.structural`, which moves the epoch at once.
-- Everything counts toward `changed`, the counter Kumi polls to know it should read again (line notifiers and plain
-- property observers only count). What changes the shape of the song moves `shape` too: a read in progress stops when it
-- moves.

local M = {}
M.__index = M

-- guard(where, fn) wraps a notifier so it never raises. state: a table that outlives the song (the counters).
-- hooks: { structural = function(reason, kind, op, owner), tracks = function(op) }.
function M.new(guard, state, hooks)
  return setmetatable({ entries = {}, dirty = {}, last = {}, guard = guard, state = state or {}, hooks = hooks or {}, dead = 0 }, M)
end

local function count(self)
  self.state.changed = (self.state.changed or 0) + 1
end

local function reshape(self)
  self.state.shape = (self.state.shape or 0) + 1
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

-- The track list: its operations ({type = "insert" | "remove" | "swap", index, index2}).
function M:watch_tracks(observable)
  add(self, observable, self.guard("tracks notifier", function(op)
    count(self)
    reshape(self)
    if self.hooks.tracks then
      self.hooks.tracks({ type = op and op.type, index = op and op.index, index2 = op and op.index2 })
    end
  end))
end

-- Refs shift or vanish when this changes. `kind` names the list; `op` is the list operation, when the observable is a list;
-- `owner` is the object that holds the list (a track, an instrument), when it has one.
function M:watch_structural(observable, reason, kind, owner)
  add(self, observable, self.guard("notifier " .. reason, function(op)
    count(self)
    reshape(self)
    if self.hooks.structural then
      self.hooks.structural(reason, kind, op and { type = op.type, index = op.index, index2 = op.index2 } or nil, owner)
    end
  end))
end

-- A pattern's lines: any change counts (the handler only counts; clearing a pattern fires hundreds).
function M:watch_lines(pattern)
  local fn = self.guard("line notifier", function() count(self) end)
  pattern:add_line_notifier(fn)
  self.entries[#self.entries + 1] = { remove = function() pattern:remove_line_notifier(fn) end }
end

-- A change that moves things inside what a read steps through (a pattern's number of lines).
function M:watch_layout(observable)
  add(self, observable, self.guard("notifier", function()
    count(self)
    reshape(self)
  end))
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
  self.dead = 0
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
