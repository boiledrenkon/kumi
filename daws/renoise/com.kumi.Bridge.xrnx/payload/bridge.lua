-- The payload's entry: setup, the server, requests, song observers and events.

local import = ...
local json = import("json")
local wire = import("wire")
local server = import("server")
local sync = import("sync")
local jobs_module = import("jobs")
local reads = import("reads")
local focus = import("focus")
local rows = import("rows")
local changes = import("changes")
local refs = import("refs")
local places = import("places")
local status = import("status")
local config = import("config")
local discovery = import("discovery")
local log = import("log")
local VERSION = import("version")

local M = {}

M.RETRY_MS = 1000
-- A read in progress that the timers haven't run for this long is run from the next message.
M.STALE_JOBS_MS = 100
-- With the timers stopped nothing but Renoise's dialog is in use, so such a slice may be longer.
M.STALE_SLICE_MS = 25
M.SETUP_RETRY_MS = 5000


-- Renoise replaces os.clock with a high-precision timer in seconds.
function M.now_ms()
  return os.clock() * 1000
end

local host, st, b, conns, observers, jobs

-- The epoch: refs are valid within one. It moves, at once and inside the notifier that saw it, when a song is switched and
-- when something happens to the song that the tool can't map into the refs it handed out (instruments, patterns or the
-- sequence restructured, devices, samples or phrases inserted or removed, tracks swapped). Within an epoch the track
-- layout counts the tracks inserted or removed: those move refs, and the tool carries them to where they went (refs.carry)
-- for a request that says which layout its refs were made in.
local function epoch()
  return string.format("%x", b.epoch_n)
end

-- The epoch moves when refs stop meaning what they meant. `tracks_kept`: the journal's places (tracks) still hold. `broken`: the
-- lists whose numbers moved (a set of CATEGORIES); a ref of an earlier epoch is still the same thing when none of the categories
-- its kind is numbered by moved since (b.snap keeps the count of each, per epoch).
local CATEGORIES = { "document", "tracks", "devices", "samples", "phrases", "patterns", "instruments", "sequence" }
local DEPENDS = {
  song = { "document" }, track = { "document", "tracks" }, device = { "document", "tracks", "devices" },
  param = { "document", "tracks", "devices" }, ptrack = { "document", "tracks", "patterns" },
  slot = { "document", "tracks", "sequence" }, seq = { "document", "sequence" }, pattern = { "document", "patterns" },
  instrument = { "document", "instruments" }, sample = { "document", "instruments", "samples" },
  phrase = { "document", "instruments", "phrases" },
}
-- The lists a notifier's change moves the numbers of.
local BREAKS = {
  devices = { "devices" }, samples = { "samples" }, phrases = { "phrases" }, patterns = { "patterns" },
  instruments = { "instruments" }, sequence = { "sequence" },
}
local EVERYTHING = { "document", "tracks", "devices", "samples", "phrases", "patterns", "instruments", "sequence" }

local function bump_epoch(reason, tracks_kept, broken, keep_shifts)
  b.epoch_n = b.epoch_n + 1
  broken = broken or EVERYTHING
  -- The layout goes on counting across epochs (no event says when an epoch moves for a device or a pattern, so a layout
  -- that started over would look older than the one Kumi holds). The shifts start over when track numbers can't be followed.
  local cleared = false
  for _, category in ipairs(broken) do
    b.cat[category] = (b.cat[category] or 0) + 1
    if (category == "tracks" or category == "document") and not keep_shifts then cleared = true end
  end
  if cleared then
    -- (the layout moves past what the shifts cover: a track numbered in an earlier layout has no place that can be told now,
    -- where the layout kept would have said it is where it was. Only a layout something was told of needs it.)
    if b.layout_told or #b.shifts > 0 then b.layout = b.layout + 1 end
    b.shifts, b.layout_told = {}, false
  end
  -- Another song: the pool's and the instruments' places are its own.
  for _, category in ipairs(broken) do
    if category == "document" and b.plog then
      places.gap(b.plog)
      places.gap(b.ilog)
    end
  end
  local snapshot = {}
  for _, category in ipairs(CATEGORIES) do snapshot[category] = b.cat[category] or 0 end
  b.snap[b.epoch_n] = snapshot
  b.snap[b.epoch_n - 512] = nil
  -- (the journal's places are tracks: a change to devices, patterns or instruments leaves them where they were)
  if b.journal and not tracks_kept then changes.journal_reset(b.journal) end
  log.write("info", "epoch " .. epoch() .. ": " .. reason)
end

-- Whether a ref of `kind` made in epoch `from` (hex) names the same thing in epoch `to` (default: now).
local function survives(from, kind, to)
  local a, c = tonumber(from, 16), to and tonumber(to, 16) or b.epoch_n
  local before, after = b.snap[a], b.snap[c]
  local depends = DEPENDS[kind]
  if not before or not after or not depends then return false end
  for _, category in ipairs(depends) do
    if before[category] ~= after[category] then return false end
  end
  return true
end

-- The layout, as it is handed out (it then needs moving when the shifts are cleared).
local function layout_now()
  b.layout_told = true
  return b.layout
end

-- How many shifts of the track list the tool keeps to carry refs through.
-- (a restore that puts a track back into a big group makes two swaps per member itself: 1024 covers groups of 500)
M.SHIFT_HISTORY = 1024
-- Watchers left behind by deleted objects, past which the song is watched anew.
M.MAX_DEAD = 256

function M.log_error(where, err)
  log.write("error", where .. ": " .. tostring(err))
end

local function guard(where, fn)
  return function(...)
    local ok, result = xpcall(fn, debug.traceback, ...)
    if not ok then
      M.log_error(where, result)
      return nil
    end
    return result
  end
end

local function show_status(message)
  pcall(function() renoise.app():show_status(message) end)
end

-- Unix milliseconds: os.time's second, refined by the timer since that second was seen to start.
local function unix_ms()
  local now, seconds = M.now_ms(), os.time()
  if seconds ~= b.anchor_seconds then
    b.anchor_seconds, b.anchor_ms = seconds, now
  end
  return seconds * 1000 + math.floor(math.min(now - b.anchor_ms, 999))
end

local function identity()
  return status.identity(b.memory, host.bundle_path)
end

local function write_discovery()
  if not (b.cfg and b.port) then return end
  local pid = status.pid() or 0
  -- (the file's name is made from the resource as written in it, cut or not: that is what a host hashes)
  local resource = status.resource_text(host.bundle_path)
  local path = b.cfg.discovery_dir .. "/" .. discovery.file_name(pid, resource)
  local ok, err = discovery.write(path, {
    schema = discovery.SCHEMA,
    daw = "renoise",
    pid = pid,
    host = "127.0.0.1",
    port = b.port,
    protocol = wire.PROTOCOL,
    version = VERSION,
    dawVersion = renoise.RENOISE_VERSION,
    resource = resource,
    project = status.project(b.memory),
    startedMs = b.started_ms,
  })
  if ok then
    if b.discovery_path and b.discovery_path ~= path then discovery.remove(b.discovery_path) end
    b.discovery_path = path
  else
    log.write("error", "could not write the discovery file: " .. tostring(err))
  end
end

-- Each Renoise has a log of its own (named by its pid, or by when it started where the pid is unknown).
local function log_file(dir)
  local id = status.pid() and ("pid-" .. status.pid()) or ("started-" .. math.floor(b.started_ms / 1000))
  return dir .. "/renoise-bridge-" .. id .. ".log"
end

local function configure_log(dir)
  local path = log_file(dir)
  log.configure(path)
  log.prune(dir, path)
end

local function configure()
  local cfg, err = config.load(host.bundle_path)
  if not cfg then
    if b.setup_error ~= err then
      b.setup_error = err
      log.write("error", "not set up: " .. err)
      show_status("Kumi: the bridge isn't set up yet; run kumi setup")
    end
    b.retry_ms = M.now_ms() + M.SETUP_RETRY_MS
    return false
  end
  b.cfg, b.setup_error = cfg, nil
  if cfg.log_dir then configure_log(cfg.log_dir) end
  return true
end

local function server_running()
  local ok, running = pcall(function() return st.server ~= nil and st.server.is_running end)
  return ok and running
end

local function listen()
  local last_err
  for _, port in ipairs(config.candidate_ports(b.cfg.port)) do
    local socket, err = host.listen(port)
    if socket then
      b.port, b.listen_error = port, nil
      log.write("info", "listening on 127.0.0.1:" .. port)
      write_discovery()
      return true
    end
    last_err = err
  end
  if b.listen_error ~= last_err then
    b.listen_error = last_err
    log.write("error", "could not listen: " .. tostring(last_err))
    show_status("Kumi: the bridge could not open a port")
  end
  b.retry_ms = M.now_ms() + M.RETRY_MS
  return false
end

local function selection(song)
  return {
    track = song.selected_track_index,
    instrument = song.selected_instrument_index,
    pattern = song.selected_pattern_index,
    sequence = song.selected_sequence_index,
  }
end

local function track_total(song)
  return song.sequencer_track_count + song.send_track_count + 1
end

local function transport(song)
  local t = song.transport
  return { playing = t.playing, bpm = t.bpm, lpb = t.lpb }
end

local TRANSPORT_OBSERVABLES = {
  "playing_observable", "bpm_observable", "lpb_observable", "tpl_observable", "edit_mode_observable",
  "follow_player_observable", "loop_pattern_observable",
}
local SELECTION_OBSERVABLES = {
  "selected_track_index_observable", "selected_instrument_index_observable", "selected_pattern_index_observable",
  "selected_sequence_index_observable", "selected_sample_observable", "selected_phrase_observable",
  "selected_track_device_observable",
}

-- Each object is watched in a group of its own (see sync.lua), kept in a list in the order of the song's own, so the operation
-- that deletes or swaps an object (which names its index) can say whose watchers are dead or moved.
local group = sync.group

-- What each kind of object is watched for. A new object gets its watchers where it is made (inside the notifier that saw
-- it), so there is no turn on which it goes unseen.
local function watch_device(device, token, track_token)
  observers:owned(token, track_token, function()
    observers:watch_changes(device.is_active_observable)
    observers:watch_changes(device.display_name_observable)
  end)
end

local function watch_track(song, i)
  local track = song:track(i)
  local token = b.groups.tracks[i]
  token.devices = {}
  -- (the track's devices' log: a new one for every watch, so a place from before the track was watched anew is unknown)
  b.ids = (b.ids or 0) + 1
  token.dlog = places.new("d" .. b.ids)
  observers:owned(token, nil, function()
    observers:watch_changes(track.name_observable)
    observers:watch_changes(track.color_observable)
    observers:watch_changes(track.mute_state_observable)
    observers:watch_changes(track.solo_state_observable)
    observers:watch_changes(track.visible_note_columns_observable)
    observers:watch_changes(track.visible_effect_columns_observable)
    for c = 1, track.max_note_columns do
      observers:watch_changes(track:column_is_muted_observable(c))
      observers:watch_changes(track:column_name_observable(c))
    end
    observers:watch_structural(track.devices_observable, "a track's devices changed", "devices", track, token)
  end)
  for d = 1, #track.devices do
    token.devices[d] = group()
    watch_device(track:device(d), token.devices[d], token)
  end
end

-- What a pattern's track shows is another pattern's when it is an alias. (Its watcher belongs to the pattern and the track.)
local function watch_pattern_track(pattern, t, pattern_token, track_token)
  observers:owned(pattern_token, track_token, function()
    observers:watch_changes(pattern:track(t).alias_pattern_index_observable)
  end)
end

local function watch_pattern(song, p, tracks)
  local pattern = song:pattern(p)
  local token = b.groups.patterns[p]
  observers:owned(token, nil, function()
    observers:watch_lines(pattern)
    observers:watch_changes(pattern.name_observable)
    observers:watch_layout(pattern.number_of_lines_observable)
  end)
  for t = 1, tracks do watch_pattern_track(pattern, t, token, b.groups.tracks[t]) end
end

local function watch_sample(sample, token, instrument_token)
  observers:owned(token, instrument_token, function()
    observers:watch_changes(sample.name_observable)
    local mapping = sample.sample_mapping
    if not mapping then return end
    observers:watch_changes(mapping.note_range_observable)
    observers:watch_changes(mapping.velocity_range_observable)
    observers:watch_changes(mapping.base_note_observable)
    observers:watch_changes(mapping.layer_observable)
    observers:watch_changes(mapping.map_key_to_pitch_observable)
    observers:watch_changes(mapping.map_velocity_to_volume_observable)
  end)
end

local function watch_phrase(phrase, token, instrument_token)
  observers:owned(token, instrument_token, function()
    observers:watch_lines(phrase)
    observers:watch_changes(phrase.name_observable)
    observers:watch_layout(phrase.number_of_lines_observable)
    observers:watch_changes(phrase.lpb_observable)
    observers:watch_changes(phrase.visible_note_columns_observable)
    observers:watch_changes(phrase.visible_effect_columns_observable)
  end)
end

local function watch_instrument(song, i)
  local instrument = song:instrument(i)
  local token = b.groups.instruments[i]
  token.samples, token.phrases = {}, {}
  observers:owned(token, nil, function()
    observers:watch_changes(instrument.name_observable)
    for k = 1, #instrument.macros do
      local macro = instrument:macro(k)
      observers:watch_changes(macro.value_observable)
      observers:watch_changes(macro.name_observable)
    end
    observers:watch_structural(instrument.samples_observable, "an instrument's samples changed", "samples", instrument, token)
    observers:watch_structural(instrument.phrases_observable, "an instrument's phrases changed", "phrases", instrument, token)
  end)
  for s = 1, #instrument.samples do
    token.samples[s] = group()
    watch_sample(instrument:sample(s), token.samples[s], token)
  end
  for ph = 1, #instrument.phrases do
    token.phrases[ph] = group()
    watch_phrase(instrument:phrase(ph), token.phrases[ph], token)
  end
end

-- Everything the focus pane and the reads depend on is watched: events for what Kumi is told as it happens, a counter
-- for the rest (`changed`), and structural marks for what moves the refs.
local function attach(song)
  observers:detach()
  if not song then return end
  local tracks = track_total(song)
  b.groups = { tracks = {}, patterns = {}, instruments = {} }
  for t = 1, tracks do b.groups.tracks[t] = group() end
  for p = 1, #song.patterns do b.groups.patterns[p] = group() end
  for i = 1, #song.instruments do b.groups.instruments[i] = group() end
  for _, name in ipairs(SELECTION_OBSERVABLES) do observers:watch(song[name], "selection") end
  for _, name in ipairs(TRANSPORT_OBSERVABLES) do observers:watch(song.transport[name], "transport") end
  observers:watch(song.name_observable, "project")
  observers:watch_tracks(song.tracks_observable)
  observers:watch_structural(song.instruments_observable, "the instruments changed", "instruments")
  observers:watch_structural(song.patterns_observable, "the patterns changed", "patterns")
  local sequencer = song.sequencer
  observers:watch_structural(sequencer.pattern_sequence_observable, "the sequence changed", "sequence")
  observers:watch_changes(sequencer.pattern_assignments_observable)
  observers:watch_changes(sequencer.pattern_slot_mutes_observable)
  observers:watch_changes(sequencer:sequence_sections_changed_observable())
  for p = 1, #song.patterns do watch_pattern(song, p, tracks) end
  for t = 1, tracks do watch_track(song, t) end
  for i = 1, #song.instruments do watch_instrument(song, i) end
  b.track_count = tracks
  b.reattach = false
  observers:changed("selection", selection(song))
  observers:changed("transport", transport(song))
  observers:changed("project", status.project(b.memory))
end

local function send_project()
  local project = status.project(b.memory)
  observers:changed("project", project)
  conns:event("project", project)
  write_discovery()
end

-- The group of a deleted owner is marked dead (its watchers are dropped when enough have piled up); a swap moves the groups
-- with their objects. Returns false when the list doesn't match the operation, which asks for the song to be watched anew.
local function forget(list, index)
  if type(index) ~= "number" or not list[index] then return false end
  table.remove(list, index).dead = true
  observers.dead = observers.dead + 1
  return true
end

local function exchange(list, a, b_)
  if type(a) ~= "number" or type(b_) ~= "number" or not list[a] or not list[b_] then return false end
  list[a], list[b_] = list[b_], list[a]
  return true
end

-- A group for an object made at `index`. A list that doesn't reach that far (it should) is filled up so the object is watched
-- at once; false says the list was out of step, which asks for the song to be watched anew on the next tick.
local function remember(list, index)
  if type(index) ~= "number" or index < 1 then return false end
  local exact = index <= #list + 1
  while #list < index - 1 do list[#list + 1] = group() end
  table.insert(list, index, group())
  return exact
end

-- A change to the lists refs index into: the epoch moves (so every answer after it is in the new epoch, and a ref from
-- before is refused), and what was made gets its watchers. What was deleted loses its group, and a swap moves the groups
-- with their objects (Renoise keeps an object's notifiers through a swap: measured). Only an operation the lists can't follow
-- asks for the whole song to be watched anew.
local function on_structural(reason, kind, op, owner, token)
  bump_epoch(reason, true, BREAKS[kind])
  -- (what moved, for the places a change keeps: a track's devices, the pool, the instruments; anything not told is a gap)
  if kind == "devices" and token and token.dlog then
    places.note(token.dlog, op)
  elseif kind == "patterns" then
    places.note(b.plog, op)
  elseif kind == "instruments" then
    places.note(b.ilog, op)
  end
  local song = renoise.song()
  if not song then return end
  local lists = { instruments = b.groups.instruments, patterns = b.groups.patterns }
  if token then
    lists.devices, lists.samples, lists.phrases = token.devices, token.samples, token.phrases
  end
  if kind == "sequence" and op then changes.journal_sequence_event(b.journal, song:is_undo_redoing()) end
  local list = lists[kind]
  if not list then return end -- (the sequence: nothing is watched per slot)
  if kind == "devices" and op then
    -- (the journal's places are device places: they move, and a change that made or removed a device is told on the producer's undo)
    for track_index, candidate in ipairs(b.groups.tracks) do
      if candidate == token then
        changes.journal_device_event(b.journal, track_index, op.type, op.index, op.index2, song:is_undo_redoing())
      end
    end
  end
  if kind == "patterns" and op then changes.journal_pattern_event(b.journal, op.type, op.index) end
  local fine = false
  if op and op.type == "insert" then
    fine = remember(list, op.index)
    if type(op.index) == "number" and op.index >= 1 then
      if kind == "instruments" then
        watch_instrument(song, op.index)
      elseif kind == "patterns" then
        watch_pattern(song, op.index, track_total(song))
      elseif kind == "devices" then
        watch_device(owner:device(op.index), list[op.index], token)
      elseif kind == "samples" then
        watch_sample(owner:sample(op.index), list[op.index], token)
      elseif kind == "phrases" then
        watch_phrase(owner:phrase(op.index), list[op.index], token)
      end
    end
  elseif op and op.type == "remove" then
    fine = forget(list, op.index)
  elseif op and op.type == "swap" then
    fine = exchange(list, op.index, op.index2)
  end
  if not fine then b.reattach = true end
end

-- A change to the track list. An insert or removal keeps the epoch and moves the layout: the shift is kept, so a request
-- made in an earlier layout can be carried to the present, and it is told to Kumi at once, before any later answer. Anything
-- else (a swap, notifications that don't add up to the count) moves the epoch.
local function on_tracks(op)
  local song = renoise.song()
  if not song then return end
  local count = track_total(song)
  local expected = b.track_count
  if op.type == "insert" then expected = expected + 1 elseif op.type == "remove" then expected = expected - 1 end
  local swapped = op.type == "swap" and expected == count and exchange(b.groups.tracks, op.index, op.index2)
  if swapped then
    -- (the tracks are the same objects in another order: they keep their watchers and their groups move with them)
    -- (a swap moves the epoch and the layout, and is kept as a shift: a job can follow a track through the swaps of a producer, or its own)
    bump_epoch("tracks were swapped", true, { "tracks" }, true)
    b.layout = b.layout + 1
    b.shifts[#b.shifts + 1] = { layout = b.layout, type = "swap", index = op.index, index2 = op.index2 }
    if #b.shifts > M.SHIFT_HISTORY then table.remove(b.shifts, 1) end
    changes.journal_track_event(b.journal, "swap", op.index, song:is_undo_redoing(), op.index2)
    conns:event("tracks", { count = count, made = json.array(), gone = json.array(), layout = b.layout, epoch = epoch() })
    return
  end
  if (op.type ~= "insert" and op.type ~= "remove") or expected ~= count or type(op.index) ~= "number" then
    bump_epoch(op.type == "swap" and "tracks were swapped" or "the tracks changed in a way the tool can't follow")
    b.track_count = count
    b.reattach = true
    conns:event("tracks", { count = count, made = json.array(), gone = json.array(), layout = b.layout, epoch = epoch() })
    return
  end
  b.track_count = count
  b.layout = b.layout + 1
  b.shifts[#b.shifts + 1] = { layout = b.layout, type = op.type, index = op.index }
  if #b.shifts > M.SHIFT_HISTORY then table.remove(b.shifts, 1) end
  local listed = true
  changes.journal_track_event(b.journal, op.type, op.index, song:is_undo_redoing())
  if op.type == "insert" then
    listed = remember(b.groups.tracks, op.index)
    if type(op.index) == "number" and op.index >= 1 then
      watch_track(song, op.index)
      for p = 1, #song.patterns do
        watch_pattern_track(song:pattern(p), op.index, b.groups.patterns[p], b.groups.tracks[op.index])
      end
    end
  else
    listed = forget(b.groups.tracks, op.index)
  end
  if not listed then b.reattach = true end
  conns:event("tracks", {
    count = count,
    made = json.array(op.type == "insert" and { op.index } or {}),
    gone = json.array(op.type == "remove" and { op.index } or {}),
    layout = b.layout,
    epoch = epoch(),
  })
end

local function flush_events()
  if not observers:attached() then return end
  local song = renoise.song()
  if not song then return end
  local dirty = observers:take_dirty()
  if dirty.selection then
    local value = selection(song)
    if observers:changed("selection", value) then conns:event("selection", value) end
  end
  if dirty.transport then
    local value = transport(song)
    if observers:changed("transport", value) then conns:event("transport", value) end
  end
  if dirty.project and observers:changed("project", status.project(b.memory)) then send_project() end
  if b.undone then
    b.undone = false
    changes.journal_check(b.journal)
  end
  for _, event in ipairs(changes.journal_take(b.journal)) do
    conns:event("undone_in_daw", { descs = json.array(event.descs), how = event.how, epoch = epoch() })
  end
  if b.reattach then
    -- (the lists were out of step: what moved meanwhile can't be told)
    places.gap(b.plog)
    places.gap(b.ilog)
    attach(song)
  elseif observers.dead > M.MAX_DEAD then
    observers:prune() -- (no call to Renoise: the entries of deleted objects are dropped)
  end
end

-- Requests

local handlers = {}

function handlers.ping()
  return json.object()
end

function handlers.status()
  return status.status(b.memory, host.bundle_path)
end

-- The reads. A long one is a job, run a slice at a time from the timers (the fast one while any is pending). Every
-- answer names the epoch and the track layout its refs were made in.
local function as_job(work)
  return { kumi_job = work }
end

local function stamped(result, layout)
  if type(result) == "table" then result.layout = layout end
  b.layout_told = true
  return result
end

function handlers.read(params)
  return as_job(function(job) return stamped(reads.read(job, params, job.epoch), job.layout) end)
end

function handlers.snapshot()
  return as_job(function(job) return stamped(focus.snapshot(job, job.epoch, b.changed or 0), job.layout) end)
end

function handlers.focus()
  return stamped(focus.focus(epoch(), b.changed or 0), b.layout)
end

function handlers.focus_body(params)
  return as_job(function(job) return stamped(focus.focus_body(job, params, job.epoch), job.layout) end)
end

-- The faders and pans of a track Kumi changed are watched for the producer's undo (and for nothing else: an automation moves
-- them all the time, and `changed` is not for that). Once per track.
local function watch_params(index)
  local token = b.groups and b.groups.tracks[index]
  local song = renoise.song()
  if not token or token.params_watched or not song or index > track_total(song) then return end
  token.params_watched = true
  local track = song:track(index)
  observers:owned(token, nil, function()
    for _, parameter in ipairs({ track.prefx_volume, track.prefx_panning, track.postfx_volume, track.postfx_panning }) do
      observers:watch_undo(parameter.value_observable)
    end
  end)
end

-- A parameter's value is watched the same way, for the producer's undo only; once per parameter.
local function watch_param(track_index, device_index, k)
  local token = b.groups and b.groups.tracks[track_index]
  local device_token = token and token.devices and token.devices[device_index]
  local song = renoise.song()
  if not device_token or not song then return end
  device_token.params_watched = device_token.params_watched or {}
  if device_token.params_watched[k] then return end
  device_token.params_watched[k] = true
  local parameter = song:track(track_index):device(device_index):parameter(k)
  observers:owned(device_token, token, function() observers:watch_undo(parameter.value_observable) end)
end

-- Changes (M3): see changes.lua. Every request carries the epoch (and the layout) its refs were made in.
-- A dialog holds Renoise's timers while its sockets keep answering (measured, 3.5.4: no tool timer ran in 3 s behind the
-- "unsaved changes" prompt; with Renoise in the background they run as ever, 57-73 ms apart for 50). So a change that finds
-- the timers silent for this long refuses `busy`, and a sliced restore waits before each slice. A callback of our own that
-- runs long holds the timers too: `held` moves the beat by that time.
M.DIALOG_MS = 400

local function busy()
  local song = renoise.song()
  if song and song.rendering then return "Renoise is rendering the song" end
  -- (What a socket callback does itself before it asks, decoding a big request, is its own time, not a dialog's: from its start.)
  if b.alive_ms and (b.work_ms or M.now_ms()) - b.alive_ms >= M.DIALOG_MS then return "a Renoise dialog is open (its timers aren't running)" end
  return nil
end

-- Track numbers carried from layout `since` to now: the new number, or nil and why ("gone": that track was deleted; "unknown":
-- the shifts since aren't kept).
local function shifted(index, since)
  if type(since) ~= "number" or since > b.layout or b.layout - since > #b.shifts then return nil, "unknown" end
  if since == b.layout then return index end
  local moved = refs.shift(index, b.shifts, since)
  if not moved then return nil, "gone" end
  return moved
end

-- A place among the tracks carried the same way: a track removed there leaves the place (nil and "unknown" as above).
local function place_shifted(index, since)
  if type(since) ~= "number" or since > b.layout or b.layout - since > #b.shifts then return nil, "unknown" end
  return refs.shift_place(index, b.shifts, since)
end

-- The places a change keeps of devices (on the track now at `t`), patterns and instruments: a mark, and where the object is now.
local function device_log(t)
  local token = b.groups and b.groups.tracks[t]
  return token and token.dlog or nil
end

local function unknown_log() return nil, "unknown" end

local place_env = {
  device_mark = function(t)
    local log_ = device_log(t)
    return log_ and places.mark(log_) or nil
  end,
  device_at = function(t, n, mark)
    local log_ = device_log(t)
    if not log_ then return unknown_log() end
    return places.item(log_, n, mark)
  end,
  -- (where a device deleted from place `n` goes back: after the one before it; the mixer device is always first)
  device_after = function(t, n, mark)
    local log_ = device_log(t)
    if not log_ then return unknown_log() end
    return places.after(log_, n, mark, 1)
  end,
  -- (how many times lines changed in pattern `p` of the track now at `t`, with the track's id: a count is of one track's lines)
  touched = function(p, t)
    local log_ = device_log(t)
    if not log_ then return nil end
    local key = p .. ":" .. log_.id
    return key .. "=" .. (b.touched[key] or 0)
  end,
  pattern_mark = function() return places.mark(b.plog) end,
  pattern_at = function(p, mark) return places.item(b.plog, p, mark) end,
  instrument_mark = function() return places.mark(b.ilog) end,
  instrument_at = function(i, mark) return places.item(b.ilog, i, mark) end,
}

local function change_env()
  local env = {
    epoch = epoch, layout = function() b.layout_told = true return b.layout end, journal = b.journal, watch_params = watch_params,
    watch_param = watch_param, busy = busy, now_ms = function() return M.now_ms() end, shifted = shifted, place_shifted = place_shifted,
    survives = function(from, kind) return survives(from, kind) end,
  }
  for name, fn in pairs(place_env) do env[name] = fn end
  return env
end

function handlers.change_preview(params)
  return changes.preview(params, change_env())
end

function handlers.change_apply(params)
  return changes.apply(params, change_env())
end

function handlers.change_restore(params)
  return changes.restore(params, change_env())
end

-- A step of undo around a plan: Renoise has none that spans callbacks, so every change stays its own step (merged: false).
function handlers.undo_step_begin(params)
  local label = type(params) == "table" and params.label or nil
  if type(label) ~= "string" or label == "" then
    error({ code = "bad-params", message = "label must be a string" }, 0)
  end
  b.steps = (b.steps or 0) + 1
  b.open_step = { id = "s" .. b.steps, label = label }
  return { stepId = b.open_step.id, merged = false }
end

function handlers.undo_step_end(params)
  local id = type(params) == "table" and params.stepId or nil
  local open = b.open_step
  if type(id) ~= "string" or not open or open.id ~= id then
    error({ code = "failed", message = "that step isn't open" }, 0)
  end
  b.open_step = nil
  return { desc = open.label, entry = false, merged = false }
end

-- The emergency stop.
function handlers.transport_stop()
  local song = renoise.song()
  if not song then error({ code = "no-song", message = "Renoise has no song open" }, 0) end
  local transport = song.transport
  local running = transport.playing
  local recording = running and transport.edit_mode or false
  if running then transport:stop() end
  return { stopped = running, wasRecording = recording }
end

-- Play: from where the pattern is (no arguments), or from a line of a sequence slot, or of the pattern being edited when only
-- `line` is given. Not an undo step, and nothing to put back. A slot is a place in the sequence, so a request that names one
-- may name the epoch it was read in.
function handlers.transport_play(params)
  local song = rows.the_song()
  if song.rendering then error({ code = "busy", message = "Renoise is rendering the song" }, 0) end
  params = type(params) == "table" and params or {}
  local sequence, line = params.sequence, params.line
  local function bad(message) error({ code = "bad-params", message = message }, 0) end
  local transport = song.transport
  local started = false
  if sequence ~= nil or line ~= nil then
    if params.epoch ~= nil and params.epoch ~= epoch() then
      error({ code = "stale", message = "those slots are from an earlier epoch (" .. tostring(params.epoch) .. "; now " .. epoch() .. "): read again" }, 0)
    end
    local slots = #song.sequencer.pattern_sequence
    if sequence ~= nil and (type(sequence) ~= "number" or sequence ~= math.floor(sequence) or sequence < 1 or sequence > slots) then
      bad(string.format("sequence is a sequence slot from 1 to %d", slots))
    end
    local pattern = sequence and song:pattern(song.sequencer:pattern(sequence)) or song:pattern(song.selected_pattern_index)
    if line == nil then line = 1 end
    if type(line) ~= "number" or line ~= math.floor(line) or line < 1 or line > pattern.number_of_lines then
      bad(string.format("line is a line of that pattern, from 1 to %d", pattern.number_of_lines))
    end
    if sequence ~= nil then transport:start_at(renoise.SongPos(sequence, line)) else transport:start_at(line) end
    started = true
  elseif not transport.playing then
    transport:start(renoise.Transport.PLAYMODE_CONTINUE_PATTERN)
    started = true
  end
  local position = transport.playback_pos
  return {
    epoch = epoch(), layout = layout_now(), started = started, playing = transport.playing,
    sequence = sequence or position.sequence, line = sequence and line or (line or position.line),
  }
end

handlers["renoise.reload"] = function()
  host.request_reload()
  return { reloading = true }
end

-- (Exposed for the tests, which add methods of their own.)
M.handlers = handlers

-- A request may name the track layout its refs were made in (`layout`): the refs that name a track, or something on one,
-- are carried through the shifts since, to where the tracks are now. A track deleted meanwhile answers `gone`; a layout the
-- tool no longer remembers (or does not know) answers `stale`.
local function carried(params, carry)
  local since = type(params) == "table" and params.layout or nil
  if since == nil then return params end
  if type(since) ~= "number" or since ~= math.floor(since) or since < 0 then
    error({ code = "bad-params", message = "layout must be a whole number from 0" }, 0)
  end
  if since == b.layout then return params end
  -- Only refs on a track move with the layout: a request naming none is never refused for it.
  if not refs.names_tracks(params) then return params end
  if since > b.layout then
    error({ code = "stale", message = "that layout (" .. since .. ") is from another epoch (now " .. epoch() .. "): read again" }, 0)
  end
  if b.layout - since > #b.shifts then
    error({ code = "stale", message = "that layout is too old to carry its refs to the present: read again" }, 0)
  end
  if carry then return carry(params, b.shifts, since, epoch()) end
  local moved, err = refs.carry(params, epoch(), b.shifts, since)
  if not moved then error(err, 0) end
  return moved
end

local CARRIED = { read = true, focus_body = true, change_preview = true, change_apply = true, change_restore = true }

-- An answer whose epoch is not the request's says what survives: the kinds of ref the request held that name the same things
-- in the answer's epoch (`tracks` too, for a reader that knows only that).
local ALL_KINDS = { "song", "track", "device", "param", "ptrack", "slot", "seq", "pattern", "instrument", "sample", "phrase" }
local function say_kept(result, from)
  if type(result) ~= "table" or type(from) ~= "string" or type(result.epoch) ~= "string" or result.epoch == from then return result end
  local kinds = {}
  for _, kind in ipairs(ALL_KINDS) do
    if survives(from, kind, result.epoch) then kinds[#kinds + 1] = kind end
  end
  if #kinds > 0 then
    result.kept = { from = from, tracks = survives(from, "track", result.epoch), kinds = kinds }
  end
  return result
end

function M.handle(_, request)
  local handler = handlers[request.method]
  if not handler then error({ code = "unknown-method", message = "no method " .. request.method }) end
  local params = request.params
  local from
  if CARRIED[request.method] then
    from = type(params) == "table" and params.epoch or nil
    -- A ref of an earlier epoch that names the same thing now is made in this one, whatever epoch the request names (Kumi keeps
    -- the names of what a device or a sequence change left alone); so is the request's own epoch, when every ref of it was.
    if type(params) == "table" then
      local left
      params, left = refs.relabel_surviving(params, epoch(), survives)
      if type(from) == "string" and from ~= epoch() and not left[from] and survives(from, "song") then
        local copy = {}
        for k, v in pairs(params) do copy[k] = v end
        copy.epoch = epoch()
        params = copy
      end
    end
    if request.method == "change_restore" and type(params) == "table" and changes.carry_for[params.kind] then
      -- (a deleted track's own ref is gone by design: only its place is carried; a deleted slot's mutes leave out a track gone)
      params = carried(params, changes.carry_for[params.kind])
    else
      params = carried(params)
    end
  end
  local started = M.now_ms()
  local result = handler(params)
  -- (A handler that ran long held the timers itself: that is not a dialog.)
  local held = M.now_ms() - started
  if held >= 50 and b.alive_ms then b.alive_ms = b.alive_ms + held end
  if type(result) == "table" and type(result.kumi_job) == "function" then
    local work = result.kumi_job
    result.kumi_job = function(job) return say_kept(work(job), from) end
    return result
  end
  return say_kept(result, from)
end

-- Lifecycle, called by main.lua

function M.start(h, reloading)
  host, st = h, h.state
  st.bridge = st.bridge or { memory = {}, retry_ms = 0, started_ms = 0 }
  b = st.bridge
  b.journal = b.journal or changes.journal_new()
  b.journal.edits = function() return b.edits or 0 end
  if b.started_ms == 0 then b.started_ms = unix_ms() end
  conns = server.new({
    connections = st.connections,
    now_ms = function() return M.now_ms() end,
    unix_ms = unix_ms,
    secret = function()
      local secret, why = config.secret(b.cfg)
      if not secret then log.write("error", "refusing a connection: " .. tostring(why)) end
      return secret, why
    end,
    identity = identity,
    on_request = M.handle,
    start_job = function(record, request, work) return jobs:add(record, request, work) end,
    cancel_jobs = function(record) jobs:cancel(record) end,
    on_authenticated = function(record)
      log.write("info", "Kumi connected from " .. record.key)
      show_status("Kumi connected")
    end,
    log = log.write,
    on_too_many_open = function(open)
      b.remake = "too many connections held open after being refused (" .. open .. ")"
    end,
  })
  b.epoch_n = b.epoch_n or ((os.time() % 1000000) * 16)
  b.layout, b.shifts, b.shape = b.layout or 0, b.shifts or {}, b.shape or 0
  b.ids = b.ids or 0
  b.touched = b.touched or {}
  b.plog, b.ilog = b.plog or places.new("p"), b.ilog or places.new("i")
  b.cat, b.snap = b.cat or {}, b.snap or {}
  if not b.snap[b.epoch_n] then
    local snapshot = {}
    for _, category in ipairs(CATEGORIES) do snapshot[category] = b.cat[category] or 0 end
    b.snap[b.epoch_n] = snapshot
  end
  b.changed = b.changed or 0
  b.document = b.document or 1
  -- A song's id (the one in the project row) and which opening of a song this is: the same while it stays open.
  rows.song_extra = function(song)
    -- (where the pid is unknown, when this Renoise was started stands in for it: `0:1` would be the same after every restart)
    return { id = status.song_id(b.memory, song), identity = (status.pid() or ("t" .. b.started_ms)) .. ":" .. b.document }
  end
  observers = sync.new(guard, b, {
    structural = on_structural,
    tracks = on_tracks,
    -- (which pattern track a line changed in: a change that cleared lines with nothing around them to tell where they are
    -- knows whether its track was touched since)
    lines = function(pos)
      local token = type(pos) == "table" and type(pos.track) == "number" and b.groups and b.groups.tracks[pos.track]
      if token and token.dlog and type(pos.pattern) == "number" then
        local key = pos.pattern .. ":" .. token.dlog.id
        b.touched[key] = (b.touched[key] or 0) + 1
      end
    end,
    -- (asked of every notifier once Kumi has made a change: was the producer's undo or redo what moved it?)
    undoing = function()
      local song = renoise.song()
      return song ~= nil and changes.journal_active(b.journal) and song:is_undo_redoing()
    end,
  })
  jobs = jobs_module.new({
    now_ms = M.now_ms,
    unix_ms = unix_ms,
    epoch = epoch,
    layout = function() b.layout_told = true return b.layout end,
    shape = function() return b.shape end,
    finish = function(job, ok, value)
      conns:finish_job(job.record, job.request, ok, value)
    end,
    on_active = function(active) if host.set_fast then host.set_fast(active) end end,
  })
  status.cache = b
  if b.cfg and b.cfg.log_dir then configure_log(b.cfg.log_dir) end
  -- Two Renoises write to one log: each line says whose it is.
  log.set_tag(status.pid() and ("pid " .. status.pid()) or ("started " .. math.floor(b.started_ms / 1000)))
  -- The reference is read again on a reload (new log or discovery folders); the server is not remade for it.
  local configured = configure()
  if configured and (not reloading or not server_running()) then
    listen()
  elseif configured then
    -- Kept: the server. A changed discovery folder is followed: the file moves there.
    write_discovery()
  end
  attach(renoise.song())
  log.write("info", (reloading and "reloaded" or "started") .. " bridge " .. VERSION)
  log.flush()
end

function M.stop(_, reloading)
  if observers then observers:detach() end
  if jobs then jobs:abort_all("reloading", "the tool was stopped or reloaded: ask again") end
  if not reloading then
    -- Renoise releases the song just before it quits; a tool disabled or reloaded sees no release.
    if conns then conns:close_all(b.quitting and "quit" or "shutdown") end
    host.close_server()
    discovery.remove(b.discovery_path)
    b.discovery_path = nil
    log.write("info", "stopped")
  end
  log.flush()
end

function M.tick()
  b.alive_ms = M.now_ms()
  unix_ms()
  if not b.cfg then
    if M.now_ms() >= b.retry_ms and configure() then listen() end
  elseif b.remake and M.now_ms() >= b.retry_ms then
    -- Descriptors held by refused connections can only be freed by closing the server (nothing else may close a
    -- client): Kumi gets a bye and reconnects.
    log.write("error", "making the server anew: " .. b.remake)
    b.remake = nil
    conns:close_all("overload")
    b.retry_ms = M.now_ms() + M.RETRY_MS
    listen()
  elseif not server_running() and M.now_ms() >= b.retry_ms then
    -- The only reason to remake the server: it says it isn't running. Remaking it drops every connection.
    log.write("error", "the server stopped running; listening again")
    b.retry_ms = M.now_ms() + M.RETRY_MS
    listen()
  end
  -- One failing step mustn't skip the others: connections still need their tick.
  local ok, err = xpcall(flush_events, debug.traceback)
  if not ok then M.log_error("events", err) end
  jobs:run()
  conns:tick()
  b.ticks = (b.ticks or 0) + 1
  if b.ticks % 20 == 0 then log.flush() end
end

function M.on_accept(client)
  conns:accept(client)
end

-- (Every message also gives the reads in progress a slice if the timers haven't: see jobs.lua.)
function M.on_message(client, data)
  b.work_ms = M.now_ms()
  local ok, err = pcall(function()
    conns:receive(client, data)
    jobs:run_if_stale(M.STALE_JOBS_MS, M.STALE_SLICE_MS)
  end)
  b.work_ms = nil
  if not ok then error(err, 0) end
end

-- Only noted, whatever the error: it may be one client's (a reset, a timeout) and the server serves the others. Closing
-- the server inside Renoise's socket callback can upset it, and remaking it drops every connection, Kumi's included,
-- for anyone who can open a connection. The tick remakes it when it says it isn't running.
function M.on_socket_error(message)
  local text = tostring(message)
  if b.last_socket_error == text then
    b.socket_error_repeats = (b.socket_error_repeats or 0) + 1
    if b.socket_error_repeats % 100 ~= 0 then return end
    text = text .. " (" .. b.socket_error_repeats .. " times)"
  else
    b.last_socket_error, b.socket_error_repeats = text, 1
  end
  log.write("error", "socket error: " .. text .. " (server running: " .. tostring(server_running()) .. ")")
end

-- Called from the fast timer while jobs are pending.
function M.tick_jobs()
  b.alive_ms = M.now_ms()
  if jobs and jobs:pending() > 0 then jobs:run() end
end

-- How many things the song is watched through (the tests count them to see that only what is new gets watched).
function M.watching()
  return observers and #observers.entries or 0
end

function M.on_new_document()
  b.quitting = false
  b.document = b.document + 1
  -- Another song: its id is worked out afresh. (Starting with a file, Renoise opens its empty song and then the file
  -- without releasing the first, so the empty song's id would otherwise stay with the file.)
  b.memory.song_id, b.memory.born_unsaved = nil, nil
  if renoise.song() then status.note_document(b.memory, renoise.song()) end
  bump_epoch("a song was opened")
  attach(renoise.song())
  send_project()
end

function M.on_release_document()
  b.quitting = true
  observers:detach()
  b.memory.song_id, b.memory.born_unsaved = nil, nil
  conns:event("project", json.null)
end

function M.on_will_save()
  local song = renoise.song()
  if song then status.write_song_id(b.memory, song) end
end

function M.on_saved()
  send_project()
end

function M.on_reload_failed(err)
  log.write("error", "reload failed, keeping the running payload: " .. tostring(err))
end

return M
