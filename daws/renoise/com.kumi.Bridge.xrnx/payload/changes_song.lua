-- Kumi's changes to the song's own settings: tempo, lines per beat, ticks per line (each a Renoise undo step).

local import = ...
local refs = import("refs")
local base = import("changes_base")

local bad = base.bad
local M = {}

-- A song setting is found without a ref (there is one song), and looked at again through the transport.
local function song_prop(spec)
  spec.locate = function(song, env)
    return {
      object = song.transport, name = "Song", place = {},
      target = { ref = refs.make(env.epoch(), "song", 1), kind = "song" },
    }
  end
  spec.reread = function(song) return song.transport end
  base.register(spec)
end

local function whole(value, name, low, high)
  if type(value) ~= "number" or value ~= math.floor(value) or value < low or value > high then
    bad(string.format("%s must be a whole number from %d to %d", name, low, high))
  end
  return value
end

song_prop {
  kind = "song-bpm", noun = "tempo", undoable = true,
  read = function(transport) return { bpm = transport.bpm } end,
  wanted = function(_, input)
    local bpm = input.bpm
    if type(bpm) ~= "number" or bpm ~= bpm or bpm < 32 or bpm > 999 then bad("bpm must be a number from 32 to 999") end
    return { bpm = bpm }
  end,
  write = function(transport, state) transport.bpm = state.bpm end,
  same = function(a, b) return base.close(a.bpm, b.bpm) end,
  title = function(_, _, to) return "Tempo " .. string.format("%g", to.bpm) .. " BPM" end,
}

song_prop {
  kind = "song-lpb", noun = "lines per beat", undoable = true,
  read = function(transport) return { lpb = transport.lpb } end,
  wanted = function(_, input) return { lpb = whole(input.lpb, "lpb", 1, 256) } end,
  write = function(transport, state) transport.lpb = state.lpb end,
  same = function(a, b) return a.lpb == b.lpb end,
  title = function(_, _, to) return "Lines per beat " .. to.lpb end,
}

song_prop {
  kind = "song-tpl", noun = "ticks per line", undoable = true,
  read = function(transport) return { tpl = transport.tpl } end,
  wanted = function(_, input) return { tpl = whole(input.tpl, "tpl", 1, 16) } end,
  write = function(transport, state) transport.tpl = state.tpl end,
  same = function(a, b) return a.tpl == b.tpl end,
  title = function(_, _, to) return "Ticks per line " .. to.tpl end,
}

return M
