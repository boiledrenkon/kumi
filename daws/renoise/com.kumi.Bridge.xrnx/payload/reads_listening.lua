-- The reads listening needs (M5 contract §8): kind `spans` (what plays where, in beats) and the option `grid` of kind `param` (a
-- parameter's text across its range).

local import = ...
local json = import("json")
local refs = import("refs")
local rows = import("rows")
local listening = import("listening")

local fail, integer, array = rows.fail, rows.integer, rows.array

local M = {}

M.DEFAULT_LIMIT = 64

-- Set by the bridge: a pattern's count of changed lines, so the song's timeline is scanned again only where it changed.
M.pattern_mark = nil

-- Whether pattern `p`'s lines on track `t` play anything: a note in a note column that isn't muted, or an effect. An alias plays
-- its source's lines.
local function pattern_plays(song, p, t, muted, any_muted)
  local pattern_track = song:pattern(p):track(t)
  if pattern_track.is_alias then
    local source = pattern_track.alias_pattern_index
    if source < 1 or source > #song.patterns then return false end
    pattern_track = song:pattern(source):track(t)
  end
  if pattern_track.is_empty then return false end
  if not any_muted then return true end
  local columns = song:track(t).max_note_columns
  for l = 1, song:pattern(p).number_of_lines do
    local line = pattern_track:line(l)
    if not line.is_empty then
      for c = 1, 8 do
        if not line:effect_column(c).is_empty then return true end
      end
      for c = 1, columns do
        if not muted[c] and not line:note_column(c).is_empty then return true end
      end
    end
  end
  return false
end

-- Track `t`'s stretches in beats: every slot it plays in (its pattern track has something that plays, the slot not muted for
-- it), merged where they touch.
local function spans_of(song, timeline, t)
  local track = song:track(t)
  local muted, any_muted = {}, false
  for c = 1, track.max_note_columns do
    if track:column_is_muted(c) then muted[c], any_muted = true, true end
  end
  local sequencer = song.sequencer
  local plays, spans = {}, {}
  for s, slot in ipairs(timeline.slots) do
    if not sequencer:track_sequence_slot_is_muted(t, s) then
      local p = slot.pattern
      if plays[p] == nil then plays[p] = pattern_plays(song, p, t, muted, any_muted) end
      if plays[p] then
        local last = spans[#spans]
        if last and math.abs(last[2] - slot.beat) < 1e-9 then
          last[2] = slot.endBeat
        else
          spans[#spans + 1] = { slot.beat, slot.endBeat }
        end
      end
    end
  end
  local out = {}
  for i, span in ipairs(spans) do out[i] = array({ span[1], span[2] }) end
  return array(out)
end

-- read {kind: "spans", limit?, cursor?} -> {items: [{ref, name, spans: [[fromBeat, toBeat]...]}], nextCursor?, epoch}
function M.spans(job, params, epoch, page)
  local song = rows.the_song()
  local timeline = listening.timeline(song, M.pattern_mark)
  local paged = params
  if params.limit == nil then
    paged = {}
    for k, v in pairs(params) do paged[k] = v end
    paged.limit = M.DEFAULT_LIMIT
  end
  return page(job, epoch, paged, rows.track_count(song), function(t)
    local live = rows.the_song()
    local track = live:track(t)
    return { ref = refs.make(epoch, "track", t), name = track.name, spans = spans_of(live, timeline, t) }
  end)
end

-- A sequence row's place in beats (contract §9: an audition candidate given as a pattern is heard where its slot plays): where
-- the slot starts and how long it plays, as Renoise counts beats (its ZL changes included). The timeline is worked out once a read.
function M.slot_beats()
  local timeline
  return function(row, s)
    timeline = timeline or listening.timeline(rows.the_song(), M.pattern_mark)
    local slot = timeline.slots[s]
    if slot then row.positionBeats, row.lengthBeats = slot.beat, slot.endBeat - slot.beat end
    return row
  end
end

-- The option `grid`: Renoise has no text for a value it isn't set to, and setting each point and putting it back is an undo step
-- that marks the song modified (measured, 3.5.4), so a row answers only its current point, and says so.
function M.grid_of(params)
  local grid = params.grid
  if grid == nil or grid == json.null then return nil end
  integer(grid, "grid", 2, 64)
  return function(row)
    row.grid = array({ array({ row.value, row.valueString }) })
    row.gridShort = true
    return row
  end
end

function M.check_kind(kind)
  if kind ~= "param" then fail("bad-params", "grid is an option of kind param") end
end

return M
