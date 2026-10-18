-- The payload's entry: setup, the server, requests, song observers and events.

local import = ...
local json = import("json")
local wire = import("wire")
local server = import("server")
local sync = import("sync")
local jobs_module = import("jobs")
local reads = import("reads")
local focus = import("focus")
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

-- The epoch: refs are valid within one. It moves when a song is switched, and when something happens to the song that
-- the tool can't map into the refs it handed out (instruments, patterns or the sequence restructured, devices, samples or
-- phrases inserted or removed, tracks swapped).
local function epoch()
  return string.format("%x", b.epoch_n)
end

local function bump_epoch(reason)
  b.epoch_n = b.epoch_n + 1
  log.write("info", "epoch " .. epoch() .. ": " .. reason)
end

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
  local resource = status.resource(host.bundle_path)
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

local function tracks(song)
  return { count = song.sequencer_track_count + song.send_track_count + 1 }
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

-- Everything the focus pane and the reads depend on is watched: events for what Kumi is told as it happens, a counter
-- for the rest (`changed`), and structural marks for what moves the refs.
local function attach(song)
  observers:detach()
  if not song then return end
  for _, name in ipairs(SELECTION_OBSERVABLES) do observers:watch(song[name], "selection") end
  for _, name in ipairs(TRANSPORT_OBSERVABLES) do observers:watch(song.transport[name], "transport") end
  observers:watch(song.name_observable, "project")
  observers:watch_list(song.tracks_observable, "tracks")
  observers:watch_structural(song.instruments_observable, "the instruments changed")
  observers:watch_structural(song.patterns_observable, "the patterns changed")
  local sequencer = song.sequencer
  observers:watch_structural(sequencer.pattern_sequence_observable, "the sequence changed")
  observers:watch_changes(sequencer.pattern_assignments_observable)
  observers:watch_changes(sequencer.pattern_slot_mutes_observable)
  for p = 1, #song.patterns do
    local pattern = song:pattern(p)
    observers:watch_lines(pattern)
    observers:watch_changes(pattern.name_observable)
    observers:watch_layout(pattern.number_of_lines_observable)
  end
  for t = 1, song.sequencer_track_count + 1 + song.send_track_count do
    local track = song:track(t)
    observers:watch_changes(track.name_observable)
    observers:watch_changes(track.color_observable)
    observers:watch_changes(track.mute_state_observable)
    observers:watch_changes(track.solo_state_observable)
    observers:watch_changes(track.visible_note_columns_observable)
    observers:watch_changes(track.visible_effect_columns_observable)
    observers:watch_structural(track.devices_observable, "a track's devices changed")
  end
  for i = 1, #song.instruments do
    local instrument = song:instrument(i)
    observers:watch_changes(instrument.name_observable)
    observers:watch_structural(instrument.samples_observable, "an instrument's samples changed")
    observers:watch_structural(instrument.phrases_observable, "an instrument's phrases changed")
  end
  observers:changed("tracks", tracks(song))
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

local function flush_events()
  if not observers:attached() then return end
  local song = renoise.song()
  if not song then return end
  local dirty = observers:take_dirty()
  -- A new track has none of the watching the others have: the song is watched anew once this is told.
  local reattach = false
  if dirty.tracks then
    reattach = true
    local count = tracks(song).count
    local known = observers.last.tracks and observers.last.tracks.count or count
    -- Each insert or removal is told as it happened, so Kumi can shift its refs; anything else moves the epoch.
    local told = false
    for _, op in ipairs(observers:take_ops("tracks")) do
      if op.type == "insert" then
        known = known + 1
        conns:event("tracks", { count = known, made = json.array({ op.index }), gone = json.array() })
        told = true
      elseif op.type == "remove" then
        known = known - 1
        conns:event("tracks", { count = known, made = json.array(), gone = json.array({ op.index }) })
        told = true
      else
        observers.structural = observers.structural or "tracks were swapped"
      end
    end
    if not told and count ~= known then conns:event("tracks", { count = count, made = json.array(), gone = json.array() }) end
  end
  if dirty.selection then
    local value = selection(song)
    if observers:changed("selection", value) then conns:event("selection", value) end
  end
  if dirty.transport then
    local value = transport(song)
    if observers:changed("transport", value) then conns:event("transport", value) end
  end
  if dirty.project and observers:changed("project", status.project(b.memory)) then send_project() end
  local reason = observers:take_structural()
  if reason then
    bump_epoch(reason)
    reattach = true
  end
  if reattach then attach(song) end
end

-- Requests

local handlers = {}

function handlers.ping()
  return json.object()
end

function handlers.status()
  return status.status(b.memory, host.bundle_path)
end

-- The reads. A long one is a job, run a slice at a time from the timers (the fast one while any is pending).
local function as_job(work)
  return { kumi_job = work }
end

function handlers.read(params)
  return as_job(function(job) return reads.read(job, params, job.epoch) end)
end

function handlers.snapshot()
  return as_job(function(job) return focus.snapshot(job, job.epoch, b.changed or 0) end)
end

function handlers.focus()
  return focus.focus(epoch(), b.changed or 0)
end

function handlers.focus_body(params)
  return as_job(function(job) return focus.focus_body(job, params, job.epoch) end)
end

handlers["renoise.reload"] = function()
  host.request_reload()
  return { reloading = true }
end

-- (Exposed for the tests, which add methods of their own.)
M.handlers = handlers

function M.handle(_, request)
  local handler = handlers[request.method]
  if not handler then error({ code = "unknown-method", message = "no method " .. request.method }) end
  return handler(request.params)
end

-- Lifecycle, called by main.lua

function M.start(h, reloading)
  host, st = h, h.state
  st.bridge = st.bridge or { memory = {}, retry_ms = 0, started_ms = 0 }
  b = st.bridge
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
  b.changed = b.changed or 0
  observers = sync.new(guard, b)
  jobs = jobs_module.new({
    now_ms = M.now_ms,
    epoch = epoch,
    layout = function() return b.layout or 0 end,
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
  conns:receive(client, data)
  jobs:run_if_stale(M.STALE_JOBS_MS, M.STALE_SLICE_MS)
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
  if jobs and jobs:pending() > 0 then jobs:run() end
end

function M.on_new_document()
  b.quitting = false
  bump_epoch("a song was opened")
  attach(renoise.song())
  send_project()
end

function M.on_release_document()
  b.quitting = true
  observers:detach()
  b.memory.song_id = nil
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
