-- Kumi's changes to a Renoise song (M3): preview, apply and restore, by kind. The kinds are in changes_tracks.lua, and the
-- modules after it; how a change goes, and the journal of what the producer's undo may take back, are in changes_base.lua.

local import = ...
local base = import("changes_base")
import("changes_tracks")
import("changes_song")
import("changes_devices")

local M = {}

local bad = base.bad

local function kind_of(params)
  local name = base.need_text(params, "kind")
  local spec = base.KINDS[name]
  if not spec then bad("Renoise has no change kind " .. string.format("%q", name)) end
  return spec
end

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
  }
  if done.material then answer.material = done.material end
  if done.created then answer.created = done.created end
  if done.undo.entry then
    base.remember(env.journal, {
      desc = desc, kind = spec.kind, place = done.place, spec = spec, prior = done.prior, applied = done.applied,
      state = "applied", shape = done.shape or spec.shape,
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
    created = params.created, discard = params.discard == true,
  }
  local done
  if spec.custom then
    done = spec.restore(song, env, restore, desc)
    if done.kumi_job then return done end
  else
    if type(restore.prior) ~= "table" or type(restore.applied) ~= "table" then bad("prior and applied: the change's values") end
    done = base.prop_restore(song, env, spec, restore, desc)
    base.note_restored(env.journal, spec.kind, done.place, restore.applied, spec)
    done.place = nil
  end
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

-- The journal, for the bridge's notifiers.
M.journal_new, M.journal_reset, M.journal_take, M.journal_active = base.journal_new, base.journal_reset, base.journal_take, base.journal_active
M.journal_track_event, M.journal_check, M.journal_device_event = base.journal_track_event, base.journal_check, base.journal_device_event

return M
