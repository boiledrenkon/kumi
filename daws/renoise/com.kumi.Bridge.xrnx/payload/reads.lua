-- read: the song as rows (contract §3), paged. A page is cut to the byte bound by dropping rows off its end.

local import = ...
local json = import("json")
local refs = import("refs")
local rows = import("rows")
local wire = import("wire")

local fail, the_song, track_count, integer, array = rows.fail, rows.the_song, rows.track_count, rows.integer, rows.array
local group_parents, song_row, track_row, sequence_row, pattern_row = rows.group_parents, rows.song_row, rows.track_row,
  rows.sequence_row, rows.pattern_row
local instrument_row, sample_row, phrase_row, device_row, param_row, line_row = rows.instrument_row, rows.sample_row,
  rows.phrase_row, rows.device_row, rows.param_row, rows.line_row
local modset_row, chain_row, parameter_row = rows.modset_row, rows.chain_row, rows.parameter_row

local M = {}

M.DEFAULT_LIMIT = 50
M.MAX_LIMIT = 200
-- The most an encoded answer may be (the frame is a little more: its envelope).
M.PAGE_BUDGET = wire.MAX_PAGE_BYTES - 4096

-- Paging

-- A cursor is `<epoch>.<layout>.<position>`: a position in a list means nothing once the epoch moved, or a track was
-- inserted or removed (the rows after it moved with it).
local function cursor_start(params, epoch, layout)
  local cursor = params.cursor
  if cursor == nil then return 1 end
  local cursor_epoch, cursor_layout, position = tostring(cursor):match("^([^.]+)%.(%d+)%.(%d+)$")
  if not cursor_epoch or tonumber(position) < 1 then fail("bad-params", "not a cursor: " .. tostring(cursor)) end
  if cursor_epoch ~= epoch then fail("stale", "that cursor is from an earlier epoch: read again from the start") end
  if tonumber(cursor_layout) ~= layout then
    fail("stale", "tracks were inserted or removed since that cursor was made: read again from the start")
  end
  return tonumber(position)
end

local function cursor_for(epoch, layout, position)
  return epoch .. "." .. layout .. "." .. position
end

local function limit_of(params)
  return integer(params.limit, "limit", 1, M.MAX_LIMIT) or M.DEFAULT_LIMIT
end

-- The rows from `row_at(i)`, i from the cursor to `total`, up to the limit (nil rows are skipped, not counted), as
-- {items, nextCursor?, epoch, ...extra}, cut to the byte budget by dropping rows off the end.
local function page(job, epoch, params, total, row_at, sparse, extra)
  local from = cursor_start(params, epoch, job.layout)
  local limit = limit_of(params)
  local items, after = {}, {}
  local i = from
  while i <= total and #items < limit do
    job.slice()
    local row = row_at(i)
    if row ~= nil then
      items[#items + 1] = row
      after[#items] = i + 1
    end
    i = i + 1
  end
  -- Where rows are few among empty places (lines), the next page starts at the next row, and there is none if no row is left.
  if sparse then
    while i <= total do
      job.slice()
      if row_at(i) ~= nil then break end
      i = i + 1
    end
  end
  local resume = i <= total and i or nil
  local keep = #items
  while true do
    local next_at = keep < #items and after[keep] or resume
    local shown = {}
    for k = 1, keep do shown[k] = items[k] end
    local result = { items = array(shown), epoch = epoch }
    for key, value in pairs(extra or {}) do result[key] = value end
    if next_at then result.nextCursor = cursor_for(epoch, job.layout, next_at) end
    if keep <= 1 or #json.encode(result) <= M.PAGE_BUDGET then return result end
    keep = math.max(1, math.floor(keep * 0.6))
  end
end

-- Shrinks the named lists of an answer that has no pages (a snapshot, an instrument, a chain), each from its end by about a
-- third at a time, until it fits a page; `truncated` says it was cut.
function M.bound(result, keys)
  while #json.encode(result) > M.PAGE_BUDGET do
    local cut = false
    for _, key in ipairs(keys) do
      local list = result[key]
      if list and #list > 1 then
        local shorter = {}
        for i = 1, math.max(1, math.floor(#list * 0.7)) do shorter[i] = list[i] end
        result[key] = array(shorter)
        cut = true
      end
    end
    if not cut then break end
    result.truncated = true
  end
  return result
end

---@return integer
local function track_param(value, epoch, song)
  ---@type integer?
  local number
  if type(value) == "number" then
    number = integer(value, "track", 1, 10000)
  elseif type(value) == "string" then
    number = refs.expect(value, "track", 1, epoch)[1]
  end
  if not number then fail("bad-params", "line needs a track: a number or a track ref") end
  ---@cast number integer
  if number > track_count(song) then fail("bad-params", "no track " .. number) end
  return number
end

local function need_parent(params, kind, count, epoch)
  if params.parent == nil then fail("bad-params", "read of " .. params.kind .. " needs a parent (" .. kind .. " ref)") end
  return refs.expect(params.parent, kind, count, epoch)
end

-- How many numbers each kind of ref holds.
local REF_COUNTS = { track = 1, seq = 1, pattern = 1, instrument = 1, sample = 2, phrase = 2, device = 2, param = 3,
  -- n-instruments
  modset = 2, moddevice = 3, chain = 2, chaindevice = 3, chainparam = 4, pluginparam = 2 }

-- n-instruments: the objects inside an instrument a ref names (nil when it isn't there).
local function instrument_object(song, kind, n)
  if n[1] > #song.instruments then return nil end
  local instrument = song:instrument(n[1])
  if kind == "modset" or kind == "moddevice" then
    if n[2] > #instrument.sample_modulation_sets then return nil end
    local set = instrument:sample_modulation_set(n[2])
    if kind == "modset" then return set end
    return n[3] <= #set.devices and set:device(n[3]) or nil
  elseif kind == "chain" or kind == "chaindevice" or kind == "chainparam" then
    if n[2] > #instrument.sample_device_chains then return nil end
    local chain = instrument:sample_device_chain(n[2])
    if kind == "chain" then return chain end
    if n[3] > #chain.devices then return nil end
    local device = chain:device(n[3])
    if kind == "chaindevice" then return device end
    return n[4] <= #device.parameters and device:parameter(n[4]) or nil
  elseif kind == "pluginparam" then
    local plugin = instrument.plugin_properties
    if not plugin.plugin_loaded or n[2] > #plugin.plugin_device.parameters then return nil end
    return plugin.plugin_device:parameter(n[2])
  end
  return nil
end

-- read {ref}: the one object a ref names.
local function one(params, epoch)
  local parsed = refs.parse(params.ref)
  local count = parsed and REF_COUNTS[parsed.kind]
  if not count then fail("bad-params", "not a ref this tool reads: " .. tostring(params.ref)) end
  local kind = parsed.kind
  local n = refs.expect(params.ref, kind, count, epoch)
  local song = the_song()
  local function need(exists)
    if not exists then fail("bad-params", "nothing is at " .. params.ref) end
  end
  local row
  if kind == "track" then
    need(n[1] <= track_count(song))
    row = track_row(song, epoch, n[1], group_parents(song))
  elseif kind == "seq" then
    need(n[1] <= #song.sequencer.pattern_sequence)
    row = sequence_row(song, epoch, n[1], track_count(song))
  elseif kind == "pattern" then
    need(n[1] <= #song.patterns)
    row = pattern_row(song, epoch, n[1], track_count(song))
  elseif kind == "instrument" then
    need(n[1] <= #song.instruments)
    row = instrument_row(song, epoch, n[1])
  elseif kind == "sample" then
    need(n[1] <= #song.instruments and n[2] <= #song:instrument(n[1]).samples)
    row = sample_row(song, epoch, n[1], n[2])
  elseif kind == "phrase" then
    need(n[1] <= #song.instruments and n[2] <= #song:instrument(n[1]).phrases)
    row = phrase_row(song, epoch, n[1], n[2])
  elseif kind == "device" then
    need(n[1] <= track_count(song) and n[2] <= #song:track(n[1]).devices)
    row = device_row(song, epoch, n[1], n[2])
  elseif kind == "modset" then
    need(instrument_object(song, kind, n) ~= nil)
    row = modset_row(song, epoch, n[1], n[2])
  elseif kind == "moddevice" then
    need(instrument_object(song, kind, n) ~= nil)
    -- (a modulation device is shown in its set's row)
    local set = modset_row(song, epoch, n[1], n[2])
    row = set.devices[n[3]]
  elseif kind == "chain" then
    need(instrument_object(song, kind, n) ~= nil)
    row = chain_row(song, epoch, n[1], n[2])
  elseif kind == "chaindevice" then
    need(instrument_object(song, kind, n) ~= nil)
    row = chain_row(song, epoch, n[1], n[2]).devices[n[3]]
  elseif kind == "chainparam" or kind == "pluginparam" then
    local parameter = instrument_object(song, kind, n)
    need(parameter ~= nil)
    row = parameter_row(params.ref, n[#n], parameter)
  else
    need(n[1] <= track_count(song) and n[2] <= #song:track(n[1]).devices
      and n[3] <= #song:track(n[1]):device(n[2]).parameters)
    row = param_row(song, epoch, n[1], n[2], n[3])
  end
  return { items = array({ row }), epoch = epoch }
end

-- read {kind, parent?, cursor?, limit?, track?} or {ref}
function M.read(job, params, epoch)
  if type(params) ~= "table" then fail("bad-params", "read needs a kind") end
  if params.ref ~= nil then return one(params, epoch) end
  local kind = params.kind
  local song = the_song()
  if kind == "song" then
    return { items = array({ song_row(song) }), epoch = epoch }
  elseif kind == "track" then
    local count = track_count(song)
    local parents = group_parents(song)
    return page(job, epoch, params, count, function(i) return track_row(the_song(), epoch, i, parents) end)
  elseif kind == "sequence" then
    local tracks = track_count(song)
    return page(job, epoch, params, #song.sequencer.pattern_sequence, function(s) return sequence_row(the_song(), epoch, s, tracks) end)
  elseif kind == "pattern" then
    local tracks = track_count(song)
    return page(job, epoch, params, #song.patterns, function(p) return pattern_row(the_song(), epoch, p, tracks) end)
  elseif kind == "line" then
    local pattern_at = need_parent(params, "pattern", 1, epoch)[1]
    if pattern_at > #song.patterns then fail("bad-params", "no pattern " .. pattern_at) end
    local track_at = track_param(params.track, epoch, song)
    local track = song:track(track_at)
    local note_columns, effect_columns = track.visible_note_columns, track.visible_effect_columns
    local total = song:pattern(pattern_at).number_of_lines
    -- An alias shows the lines of another pattern: they are that pattern's, and a write to them changes it.
    local alias = song:pattern(pattern_at):track(track_at).alias_pattern_index
    local extra = alias > 0 and { aliasOf = alias, aliasRef = refs.make(epoch, "pattern", alias) } or {}
    extra.mutedColumns = rows.column_state(track)
    return page(job, epoch, params, total, function(i)
      return line_row(the_song():pattern(pattern_at):track(track_at):line(i), i, note_columns, effect_columns, params.hidden == true)
    end, true, extra)
  elseif kind == "instrument" then
    return page(job, epoch, params, #song.instruments, function(i) return instrument_row(the_song(), epoch, i) end)
  elseif kind == "sample" then
    local at = need_parent(params, "instrument", 1, epoch)[1]
    if at > #song.instruments then fail("bad-params", "no instrument " .. at) end
    return page(job, epoch, params, #song:instrument(at).samples, function(s) return sample_row(the_song(), epoch, at, s) end)
  elseif kind == "phrase" then
    local at = need_parent(params, "instrument", 1, epoch)[1]
    if at > #song.instruments then fail("bad-params", "no instrument " .. at) end
    return page(job, epoch, params, #song:instrument(at).phrases, function(p) return phrase_row(the_song(), epoch, at, p) end)
  elseif kind == "device" then
    local at = need_parent(params, "track", 1, epoch)[1]
    if at > track_count(song) then fail("bad-params", "no track " .. at) end
    return page(job, epoch, params, #song:track(at).devices, function(d) return device_row(the_song(), epoch, at, d) end)
  elseif kind == "param" then
    -- (n-instruments: a sample FX device's parameters, or a plug-in instrument's)
    local parent = refs.parse(params.parent)
    if parent and parent.kind == "chaindevice" then
      local at = refs.expect(params.parent, "chaindevice", 3, epoch)
      local device = instrument_object(song, "chaindevice", at)
      if not device then return fail("bad-params", "no such device") end
      return page(job, epoch, params, #device.parameters, function(k)
        local now = instrument_object(the_song(), "chainparam", { at[1], at[2], at[3], k })
        if not now then fail("stale", "the device changed while it was read") end
        return parameter_row(refs.make(epoch, "chainparam", at[1], at[2], at[3], k), k, now)
      end)
    elseif parent and parent.kind == "instrument" then
      local i = refs.expect(params.parent, "instrument", 1, epoch)[1]
      if i > #song.instruments or not song:instrument(i).plugin_properties.plugin_loaded then fail("bad-params", "that instrument has no plug-in") end
      return page(job, epoch, params, #song:instrument(i).plugin_properties.plugin_device.parameters, function(k)
        local device = the_song():instrument(i).plugin_properties.plugin_device
        return parameter_row(refs.make(epoch, "pluginparam", i, k), k, device:parameter(k))
      end)
    end
    local at = need_parent(params, "device", 2, epoch)
    local t, d = at[1], at[2]
    if t > track_count(song) or d > #song:track(t).devices then fail("bad-params", "no such device") end
    return page(job, epoch, params, #song:track(t):device(d).parameters, function(k) return param_row(the_song(), epoch, t, d, k) end)
  elseif kind == "modset" or kind == "chain" then
    -- n-instruments: an instrument's modulation sets or sample FX chains
    local at = need_parent(params, "instrument", 1, epoch)[1]
    if at > #song.instruments then fail("bad-params", "no instrument " .. at) end
    local count = kind == "modset" and #song:instrument(at).sample_modulation_sets or #song:instrument(at).sample_device_chains
    return page(job, epoch, params, count, function(k)
      if kind == "modset" then return modset_row(the_song(), epoch, at, k) end
      return chain_row(the_song(), epoch, at, k)
    end)
  end
  fail("bad-params", "unknown kind " .. tostring(kind) .. " (song, track, sequence, pattern, line, instrument, sample, phrase, device, param, modset, chain)")
end

return M
