-- Observers for one song: attached on a new document, detached on release. A notifier only marks
-- what changed; the timer reads the current values, so a storm of notifications is one event.

local M = {}
M.__index = M

-- guard(where, fn) wraps a notifier so it never raises.
function M.new(guard)
  return setmetatable({ entries = {}, dirty = {}, last = {}, guard = guard }, M)
end

function M:watch(observable, key)
  local fn = self.guard("notifier " .. key, function() self.dirty[key] = true end)
  observable:add_notifier(fn)
  self.entries[#self.entries + 1] = { observable = observable, fn = fn }
end

function M:detach()
  for _, entry in ipairs(self.entries) do
    pcall(function()
      if entry.observable:has_notifier(entry.fn) then entry.observable:remove_notifier(entry.fn) end
    end)
  end
  self.entries = {}
  self.dirty = {}
  self.last = {}
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
