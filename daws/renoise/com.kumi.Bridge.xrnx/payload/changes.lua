-- Kumi's changes to a Renoise song (M3): preview, apply and restore, by kind. The kinds are in changes_tracks.lua, and the
-- modules after it; how a change goes, and the journal of what the producer's undo may take back, are in changes_base.lua.

local import = ...
local base = import("changes_base")
import("changes_tracks")
import("changes_song")
import("changes_devices")
import("changes_notes")
import("changes_layout")
import("changes_slots")
import("changes_lua")
import("changes_instruments")
-- (M4 n-devices: presets, names, and the Doofer, Splitter and Notepad edits Kumi's host composes)
local presets = import("changes_presets")

local M = {}

local bad = base.bad

local function kind_of(params)
  local name = base.need_text(params, "kind")
  local spec = base.KINDS[name]
  if not spec then bad("Renoise has no change kind " .. string.format("%q", name)) end
  return spec
end

-- The song, if the request's epoch is the song's and Renoise isn't busy (for the requests that are no change of the song).
M.context = base.context

function M.preview(params, env)
  local song = base.context(params, env)
  local spec = kind_of(params)
  local input = base.need_object(params, "input")
  local preview
  if spec.custom then
    preview = spec.preview(song, env, input)
  else
    preview = base.prop_preview(song, env, spec, input)
  end
  return {
    epoch = env.epoch(), layout = env.layout(), target = preview.target, prior = preview.prior, fence = preview.fence,
    display = preview.display,
  }
end

function M.apply(params, env)
  local song = base.context(params, env)
  local spec = kind_of(params)
  local input, fence = base.need_object(params, "input"), base.need_object(params, "fence")
  local desc = base.need_text(params, "desc")
  local done
  if spec.custom then
    done = spec.apply(song, env, input, fence, desc)
  else
    done = base.prop_apply(song, env, spec, input, fence, desc)
  end
  local answer = {
    epoch = env.epoch(), layout = env.layout(), applied = done.applied, display = done.display, undo = done.undo,
    target = done.target,
  }
  if done.material then answer.material = done.material end
  if done.created then answer.created = done.created end
  if done.undo.entry then
    base.remember(env.journal, {
      desc = desc, kind = spec.kind, place = done.place, spec = spec, prior = done.prior, applied = done.journal_applied or done.applied,
      state = "applied", shape = done.shape or spec.shape, follows = done.follows, uses_patterns = spec.uses_patterns,
    })
  end
  return answer
end

function M.restore(params, env)
  local song = base.context(params, env)
  local spec = kind_of(params)
  local desc = base.need_text(params, "desc")
  local restore = {
    target = base.need_object(params, "target"), prior = params.prior, applied = params.applied, material = params.material,
    created = params.created, discard = params.discard == true, layout = params.layout,
  }
  local done
  if spec.custom then
    done = spec.restore(song, env, restore, desc)
  else
    if type(restore.prior) ~= "table" or type(restore.applied) ~= "table" then bad("prior and applied: the change's values") end
    done = base.prop_restore(song, env, spec, restore, desc)
    base.note_restored(env.journal, spec.kind, done.place, done.applied, spec)
    done.place, done.applied = nil, nil
  end
  if done.kumi_job then return done end
  -- (a restore that found it back already wrote nothing: said, so Kumi doesn't tell it as an undo it made)
  if done.unwritten and type(done.display) == "table" and not done.display.note then
    done.display.note = "It was back already: nothing was written."
  end
  done.unwritten = nil
  done.epoch, done.layout = env.epoch(), env.layout()
  return done
end

-- The track delete's restore needs its place carried by hand: the deleted track's own ref is gone by design.
function M.carry_restore(params, shifts, since)
  local refs = import("refs")
  local material = params.material
  if type(material) == "table" and type(material.index) == "number" then
    local moved = refs.shift(material.index, shifts, since)
    local copy = {}
    for k, v in pairs(params) do copy[k] = v end
    copy.material = {}
    for k, v in pairs(material) do copy.material[k] = v end
    -- a place that was the deleted track's own is still its place (the track after it moved up into it); one before it moved
    copy.material.index = moved or material.index
    copy.material.indexThen = material.index
    return copy
  end
  return params
end

-- A slot delete's restore: the tracks muted in the slot are carried through the shifts since; one deleted meanwhile is left
-- out (and counted, for the answer to say) rather than refusing the slot.
function M.carry_slot_restore(params, shifts, since, now)
  local refs = import("refs")
  local material = params.material
  if type(material) ~= "table" or type(material.mutes) ~= "table" then
    local moved, err = refs.carry(params, now, shifts, since)
    if not moved then error(err, 0) end
    return moved
  end
  local copy = {}
  for k, v in pairs(params) do copy[k] = v end
  copy.material = {}
  for k, v in pairs(material) do copy.material[k] = v end
  copy.material.mutes = nil
  local moved, err = refs.carry(copy, now, shifts, since)
  if not moved then error(err, 0) end
  local mutes, lost = {}, 0
  for _, mute in ipairs(material.mutes) do
    local t = type(mute) == "table" and type(mute.track) == "number" and refs.shift(mute.track, shifts, since) or nil
    if t then mutes[#mutes + 1] = { track = t } else lost = lost + 1 end
  end
  moved.material.mutes, moved.material.mutesLost = mutes, lost
  return moved
end

-- The restores that carry their params by hand, by kind.
M.carry_for = { ["track-delete"] = M.carry_restore, ["slot-delete"] = M.carry_slot_restore }

-- (M4 n-devices) Whether a kind finds what it changed by places alone (its material): a restore of it isn't held back by
-- refs of an earlier epoch in its target or `created`.
function M.follows_places(kind)
  local spec = base.KINDS[kind]
  return spec ~= nil and spec.places == true
end

-- (M4 n-devices) A device's preset data, for Kumi's host to edit (`preset_read`).
function M.preset_read(params, env) return presets.read(params, env) end

-- The journal, for the bridge's notifiers.
M.journal_new, M.journal_reset, M.journal_take, M.journal_active = base.journal_new, base.journal_reset, base.journal_take, base.journal_active
M.journal_track_event, M.journal_check, M.journal_device_event = base.journal_track_event, base.journal_check, base.journal_device_event
M.journal_pattern_event, M.journal_sequence_event = base.journal_pattern_event, base.journal_sequence_event

return M
