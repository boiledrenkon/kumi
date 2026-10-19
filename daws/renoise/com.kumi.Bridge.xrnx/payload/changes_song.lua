-- Kumi's changes to the song's own settings: tempo, lines per beat, ticks per line (each a Renoise undo step).

local import = ...
local refs = import("refs")
local base = import("changes_base")

local bad = base.bad
local M = {}

-- A song setting is found without a ref (there is one song), and looked at again through the transport.
local function song_prop(spec)
  spec.locate = function(song, env, input)
    -- (a record's own ref, when there is one, must still be good: the loop range's is a sequence ref, stale once the sequence changed)
    if type(input) == "table" and type(input.ref) == "string" then refs.expect(input.ref, spec.target_kind or "song", 1, env.epoch()) end
    return {
      object = song.transport, name = "Song", place = {},
      target = { ref = refs.make(env.epoch(), spec.target_kind or "song", 1), kind = spec.target_kind or "song" },
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

-- The loop is not an undo step, and Renoise applies it a moment after it is written (a read at once shows the old one).

song_prop {
  kind = "loop-range", noun = "loop range", undoable = false, eventual = true,
  -- (its numbers are sequence slots: a record that names the sequence is refused `stale` once the sequence has changed)
  target_kind = "seq",
  read = function(transport)
    local range = transport.loop_sequence_range
    if #range == 0 then return { first = 0, last = 0 } end
    return { first = range[1], last = range[2] }
  end,
  wanted = function(_, input)
    local first, last = input.first or 0, input.last or 0
    local song = renoise.song()
    local slots = song and #song.sequencer.pattern_sequence or 1
    if first == 0 and last == 0 then return { first = 0, last = 0 } end
    if type(first) ~= "number" or type(last) ~= "number" or first ~= math.floor(first) or last ~= math.floor(last)
      or first < 1 or last < first or last > slots then
      bad(string.format("first and last are sequence slots with first up to last, from 1 to %d (both 0 or left out clears the loop)", slots))
    end
    return { first = first, last = last }
  end,
  write = function(transport, state)
    if state.first == 0 then transport.loop_sequence_range = {} else transport.loop_sequence_range = { state.first, state.last } end
  end,
  same = function(a, b) return a.first == b.first and a.last == b.last end,
  title = function(_, _, to)
    if to.first == 0 then return "Loop range cleared" end
    if to.first == to.last then return "Loop slot " .. to.first end
    return string.format("Loop slots %d to %d", to.first, to.last)
  end,
}

song_prop {
  kind = "loop-pattern", noun = "pattern loop", undoable = false, eventual = true,
  read = function(transport) return { on = transport.loop_pattern } end,
  wanted = function(_, input)
    if type(input.on) ~= "boolean" then bad("on must be true or false") end
    return { on = input.on }
  end,
  write = function(transport, state) transport.loop_pattern = state.on end,
  same = function(a, b) return a.on == b.on end,
  title = function(_, _, to) return to.on and "Pattern loop on" or "Pattern loop off" end,
}

return M
