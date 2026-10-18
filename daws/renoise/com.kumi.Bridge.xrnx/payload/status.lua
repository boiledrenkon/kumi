-- What the bridge says about itself and the song: hello/status, and the song's Kumi id.

local import = ...
local json = import("json")
local net = import("net")
local VERSION = import("version")

local M = {}

local cached_pid, pid_looked_up = nil, false

-- Renoise's pid, where Lua can learn it without a console window: /proc on Linux, the parent of a
-- shell on macOS. nil on Windows (the host can find the port's owner).
function M.pid()
  if pid_looked_up then return cached_pid end
  pid_looked_up = true
  local platform = os.platform and os.platform() or ""
  pcall(function()
    if platform == "LINUX" then
      local f = io.open("/proc/self/stat", "rb")
      if f then
        cached_pid = tonumber((f:read("*l") or ""):match("^(%d+)"))
        f:close()
      end
    elseif platform == "MACINTOSH" then
      local p = io.popen("echo $PPID")
      if p then
        cached_pid = tonumber((p:read("*l") or ""):match("^%s*(%d+)"))
        p:close()
      end
    end
  end)
  return cached_pid
end

function M.dawVersion()
  return (renoise.RENOISE_VERSION:gsub("%s*[Dd]emo%s*$", ""))
end

function M.demo()
  return renoise.RENOISE_VERSION:find("[Dd]emo") ~= nil
end

-- The V<x.y.z> preferences folder this tool is installed in.
function M.resource_path(bundle_path)
  return bundle_path:match("^(.*)[/\\]Scripts[/\\]Tools[/\\][^/\\]+[/\\]?$") or ""
end

-- Song ids live in song.tool_data, which Renoise saves with the song for this tool's Id.
local function stored_id(song)
  local data = song.tool_data
  if type(data) ~= "string" or data == "" then return nil end
  local value = json.decode(data)
  if type(value) == "table" and type(value.songId) == "string" and value.songId:match("^%x+$") and #value.songId == 32 then
    return value.songId:lower()
  end
  return nil
end

-- The id for the current song: the stored one, or a new one kept until the song is saved.
function M.song_id(memory, song)
  local id = stored_id(song)
  if id then
    memory.song_id = id
    return id
  end
  memory.song_id = memory.song_id or net.nonce()
  return memory.song_id
end

function M.write_song_id(memory, song)
  local id = M.song_id(memory, song)
  local data = json.decode(song.tool_data or "")
  if type(data) ~= "table" or json.is_array(data) then data = {} end
  data.songId = id
  song.tool_data = json.encode(data)
end

function M.project(memory)
  local song = renoise.song()
  if not song then return json.null end
  local path = song.file_name or ""
  return {
    id = M.song_id(memory, song),
    name = song.name or "",
    path = path,
    unsaved = path == "",
  }
end

function M.hello(memory, bundle_path, port)
  local song = renoise.song()
  return {
    daw = "renoise",
    protocol = net.PROTOCOL,
    bridgeVersion = VERSION,
    dawVersion = M.dawVersion(),
    apiVersion = renoise.API_VERSION,
    demo = M.demo(),
    pid = M.pid() or json.null,
    port = port or json.null,
    resourcePath = M.resource_path(bundle_path),
    project = M.project(memory),
    playing = song ~= nil and song.transport.playing or false,
  }
end

return M
