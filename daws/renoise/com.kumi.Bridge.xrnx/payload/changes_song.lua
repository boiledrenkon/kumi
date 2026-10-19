-- Kumi's changes to the song's own settings: tempo, lines per beat, ticks per line (each a Renoise undo step).

local import = ...
local refs = import("refs")
local base = import("changes_base")

local bad = base.bad
local M = {}

-- A song setting is found without a ref (there is one song), and looked at again through the transport (or the song itself,
-- for what `on_song` kinds change).
local function song_prop(spec)
  local function object_of(song) return spec.on_song and song or song.transport end
  spec.locate = function(song, env, input)
    -- (a record's own ref, when there is one, must still be good: the loop range's is a sequence ref, stale once the sequence changed)
    if type(input) == "table" and type(input.ref) == "string" then refs.expect(input.ref, spec.target_kind or "song", 1, env.epoch()) end
    return {
      object = object_of(song), name = "Song", place = {},
      target = { ref = refs.make(env.epoch(), spec.target_kind or "song", 1), kind = spec.target_kind or "song" },
    }
  end
  spec.reread = function(song) return object_of(song) end
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

-- M4 (n-song): the groove, the metronome, the editing settings, the song's name, artist and comments, and the track headroom.
-- Each kind names several settings and a change gives any of them: only those are read, fenced, written and put back, so a
-- setting of the same kind the producer changes meanwhile is left to them. Measured in Renoise 3.5.4 (lab, 2026-10-19): the
-- groove, the metronome's beats per bar, lines per beat and volume, the track headroom, the song's name, artist and comments
-- are undo steps; the metronome's switch and precount, follow player, edit step, octave, record quantize, wrapped and
-- single-track editing, keyboard velocity and the parameter record mode are not.

local function db_of(linear) return 20 * math.log10(linear) end
local function linear_of(db) return 10 ^ (db / 20) end

local flag, text_of = base.flag, base.text_of

local function percent(value) return math.floor(value * 100 + 0.5) .. "%" end

local function settings(spec)
  base.settings(spec)
  song_prop(spec)
end

settings {
  kind = "song-groove", noun = "groove", undoable = true, heading = "Groove",
  fields = {
    {
      name = "on", read = function(t) return t.groove_enabled end, write = function(t, v) t.groove_enabled = v end,
      check = flag, text = function(v) return v and "on" or "off" end,
    },
    {
      name = "amounts",
      read = function(t)
        local out = {}
        for i, v in ipairs(t.groove_amounts) do out[i] = v end
        return out
      end,
      write = function(t, v) t.groove_amounts = { v[1], v[2], v[3], v[4] } end,
      check = function(value)
        if type(value) ~= "table" or #value ~= 4 then bad("amounts: four numbers from 0 to 1, one per line of the groove") end
        local out = {}
        for i = 1, 4 do
          local v = value[i]
          if type(v) ~= "number" or v ~= v or v < 0 or v > 1 then bad("amounts: four numbers from 0 to 1, one per line of the groove") end
          out[i] = v
        end
        return out
      end,
      text = function(v) return percent(v[1]) .. " " .. percent(v[2]) .. " " .. percent(v[3]) .. " " .. percent(v[4]) end,
    },
  },
}

-- The metronome's volume is kept in dB; at or below SILENT_DB it is silent (0).
local SILENT_DB = -96

settings {
  kind = "song-metronome", noun = "metronome", undoable = true, heading = "Metronome",
  fields = {
    {
      name = "beatsPerBar", read = function(t) return t.metronome_beats_per_bar end, write = function(t, v) t.metronome_beats_per_bar = v end,
      check = function(v) return whole(v, "beatsPerBar", 0, 16) end,
      text = function(v) return v == 0 and "beats per bar from the pattern" or (v .. " beats per bar") end,
    },
    {
      name = "linesPerBeat", read = function(t) return t.metronome_lines_per_beat end, write = function(t, v) t.metronome_lines_per_beat = v end,
      check = function(v) return whole(v, "linesPerBeat", 0, 256) end,
      text = function(v) return v == 0 and "lines per beat from the song" or (v .. " lines per beat") end,
    },
    {
      name = "volumeDb",
      read = function(t)
        local v = t.metronome_volume
        if v <= linear_of(SILENT_DB) then return SILENT_DB end
        return db_of(v)
      end,
      write = function(t, v) t.metronome_volume = v <= SILENT_DB and 0 or linear_of(v) end,
      check = function(v)
        if type(v) ~= "number" or v ~= v or v > 6 then bad(string.format("volumeDb must be a number up to 6 (%d or less is silent)", SILENT_DB)) end
        return math.max(v, SILENT_DB)
      end,
      text = function(v) return "volume " .. base.db_text(v > SILENT_DB and v or nil) .. " dB" end,
    },
  },
}

settings {
  kind = "metronome-switch", noun = "metronome", undoable = false, heading = "Metronome",
  fields = {
    {
      name = "on", read = function(t) return t.metronome_enabled end, write = function(t, v) t.metronome_enabled = v end,
      check = flag, text = function(v) return v and "on" or "off" end,
    },
    {
      name = "precount",
      read = function(t) return t.metronome_precount_enabled and t.metronome_precount_bars or false end,
      write = function(t, v)
        if v == false then
          t.metronome_precount_enabled = false
        else
          t.metronome_precount_bars = v
          t.metronome_precount_enabled = true
        end
      end,
      check = function(v)
        if v == false then return false end
        return whole(v, "precount (bars, or false for none)", 1, 4)
      end,
      text = function(v) return v == false and "no precount" or (v == 1 and "1-bar precount" or (v .. "-bar precount")) end,
    },
  },
}

settings {
  kind = "edit-settings", noun = "editing settings", undoable = false,
  fields = {
    {
      name = "followPlayer", read = function(t) return t.follow_player end, write = function(t, v) t.follow_player = v end,
      check = flag, text = function(v) return v and "follow player on" or "follow player off" end,
    },
    {
      name = "editStep", read = function(t) return t.edit_step end, write = function(t, v) t.edit_step = v end,
      check = function(v) return whole(v, "editStep", 0, 64) end, text = function(v) return "edit step " .. v end,
    },
    {
      name = "octave", read = function(t) return t.octave end, write = function(t, v) t.octave = v end,
      check = function(v) return whole(v, "octave", 0, 8) end, text = function(v) return "octave " .. v end,
    },
    {
      name = "quantize",
      read = function(t) return t.record_quantize_enabled and t.record_quantize_lines or false end,
      write = function(t, v)
        if v == false then
          t.record_quantize_enabled = false
        else
          t.record_quantize_lines = v
          t.record_quantize_enabled = true
        end
      end,
      check = function(v)
        if v == false then return false end
        return whole(v, "quantize (lines, or false for none)", 1, 32)
      end,
      text = function(v) return v == false and "record quantize off" or ("record quantize " .. v .. (v == 1 and " line" or " lines")) end,
    },
    {
      name = "wrap", read = function(t) return t.wrapped_pattern_edit end, write = function(t, v) t.wrapped_pattern_edit = v end,
      check = flag, text = function(v) return v and "wrapped editing on" or "wrapped editing off" end,
    },
    {
      name = "singleTrack", read = function(t) return t.single_track_edit_mode end, write = function(t, v) t.single_track_edit_mode = v end,
      check = flag, text = function(v) return v and "single-track editing on" or "single-track editing off" end,
    },
    {
      name = "keyboardVelocity",
      read = function(t) return t.keyboard_velocity_enabled and t.keyboard_velocity or false end,
      write = function(t, v)
        if v == false then
          t.keyboard_velocity_enabled = false
        else
          t.keyboard_velocity_enabled = true
          t.keyboard_velocity = v
        end
      end,
      check = function(v)
        if v == false then return false end
        return whole(v, "keyboardVelocity (0 to 127, or false for full velocity)", 0, 127)
      end,
      text = function(v) return v == false and "keyboard velocity off" or ("keyboard velocity " .. v) end,
    },
    {
      name = "recordTo",
      read = function(t) return t.record_parameter_mode == renoise.Transport.RECORD_PARAMETER_MODE_AUTOMATION and "automation" or "pattern" end,
      write = function(t, v)
        local T = renoise.Transport
        t.record_parameter_mode = v == "automation" and T.RECORD_PARAMETER_MODE_AUTOMATION or T.RECORD_PARAMETER_MODE_PATTERN
      end,
      check = function(v)
        if v ~= "pattern" and v ~= "automation" then bad("recordTo must be \"pattern\" or \"automation\"") end
        return v
      end,
      text = function(v) return "parameters recorded to the " .. v end,
    },
  },
}

-- The song's comments are paragraphs; Kumi takes them as one text (lines apart by "\n") or a list.
local MOST_COMMENTS = 64 * 1024

settings {
  kind = "song-info", noun = "name, artist or comments", undoable = true, heading = "Song", on_song = true,
  fields = {
    {
      name = "name", read = function(s) return s.name end, write = function(s, v) s.name = v end,
      check = function(v) return text_of(v, "name", 256) end, text = function(v) return "named " .. base.q(v) end,
    },
    {
      name = "artist", read = function(s) return s.artist end, write = function(s, v) s.artist = v end,
      check = function(v) return text_of(v, "artist", 256) end, text = function(v) return "by " .. base.q(v) end,
    },
    {
      name = "comments",
      read = function(s)
        local out = {}
        for i, line in ipairs(s.comments) do out[i] = line end
        return out
      end,
      write = function(s, v) s.comments = v end,
      check = function(v)
        local lines = {}
        if type(v) == "string" then
          if #v > MOST_COMMENTS then bad("comments are at most 64 KB") end
          for line in (v .. "\n"):gmatch("([^\n]*)\n") do lines[#lines + 1] = (line:gsub("\r$", "")) end
          if v == "" then lines = {} end
        elseif type(v) == "table" then
          local size = 0
          for i, line in ipairs(v) do
            if type(line) ~= "string" or line:find("\n") then bad("comments: text, or a list of lines") end
            size = size + #line
            lines[i] = line
          end
          if size > MOST_COMMENTS then bad("comments are at most 64 KB") end
        else
          bad("comments: text, or a list of lines")
        end
        return lines
      end,
      text = function(v) return #v == 0 and "comments cleared" or "comments changed" end,
    },
  },
}

settings {
  kind = "song-headroom", noun = "track headroom", undoable = true,
  fields = {
    {
      name = "headroomDb", read = function(t) return db_of(t.track_headroom) end, write = function(t, v) t.track_headroom = linear_of(v) end,
      check = function(v)
        if type(v) ~= "number" or v ~= v or v < -12 or v > 0 then bad("headroomDb must be a number from -12 to 0") end
        return v
      end,
      text = function(v) return "track headroom " .. base.db_text(v) .. " dB" end,
    },
  },
}

return M
