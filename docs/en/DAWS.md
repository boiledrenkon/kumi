# What Kumi does in Live, REAPER and Renoise

English · [简体中文](../zh-CN/DAWS.md) · [日本語](../ja/DAWS.md)

Kumi works in Ableton Live, REAPER and Renoise, in each DAW's own words. This
page says what works where. Connecting is in the
[guide](KUMI_GUIDE.md#connect-to-reaper-or-renoise), every change tool in
[how Kumi changes your Set](KUMI_CHANGES.md), and the Live bridge's detail in
the [capability matrix](CAPABILITY_MATRIX.md).

## At a glance

✓ works. "Live only" is something only Live has. "Not yet" names what's
missing.

| Ability | Live | REAPER | Renoise |
| --- | --- | --- | --- |
| **Changing the project** | | | |
| Changes in HISTORY with Kumi's own undo | ✓ | ✓ | ✓ |
| Instant phrases, made with no model call | ✓ | ✓ | ✓ |
| Code run inside the DAW | ✓ Python | ✓ Lua (ReaScript), REAPER's API and actions | ✓ Lua |
| Stop everything at once (`/stop`) | ✓ | ✓ | ✓ |
| Knows whether the DAW is recording | ✓ | ✓ | ✓ |
| A line in the DAW's status bar | ✓ | no: REAPER's API has no status bar (a notification instead) | ✓ |
| Opens a saved project | ✓ | not yet: setup's restart doesn't open it again | not yet: setup's restart doesn't open it again |
| Catches up on what changed while Kumi was closed | ✓ | ✓ | ✓ |
| Conversations follow the saved project | ✓ | ✓ | ✓ |
| **Listening** | | | |
| Hears tracks or the mix | ✓ | ✓ | ✓ |
| Renders offline to listen, then plays it out loud | hears as Live plays | ✓ | ✓ |
| Auditions candidates against a reference, and goals (`/goal`) on a rig kept open | ✓ | ✓ | ✓ |
| Judges its changes, `/loop`, matches references | ✓ | ✓ | ✓ |
| The audio file behind a clip, item or sample | ✓ | ✓ | ✓ |
| Splits a track into stems | ✓ Live's own splitter | not yet: no splitter Kumi can use | not yet: no splitter Kumi can use |
| Turns audio into MIDI | ✓ drums, melody and harmony | not yet: only a pitched part in a file (Basic Pitch) | not yet: only a pitched part in a file (Basic Pitch) |
| **FOCUS** | | | |
| FOCUS and the beat light follow the DAW as it changes | ✓ | ✓ | ✓ |
| A track's devices as a tree | ✓ | ✓ FX chains and containers | ✓ Track DSPs, Doofers and Splitters |
| The notes in the note editor | ✓ | ✓ the MIDI editor | the Pattern and Phrase Editors' lines instead |
| The timeline at a glance | ✓ Arrangement | ✓ arrange view | the Pattern Matrix instead |
| A launcher grid | ✓ Session View | Live only | Live only |
| Pointing at something in the DAW (**Ask Kumi about this**) | ✓ | Live only | Live only |
| **Library and tutorials** | | | |
| Watches video tutorials | ✓ | ✓ | ✓ |
| Finds and loads the DAW's presets | ✓ | ✓ | ✓ |
| Lists your own projects (`my_sets`) | ✓ | not yet: `.rpp` files aren't read | not yet: `.xrns` files aren't read |
| Searches the DAW's manual; loads Browser items by id | ✓ | Live only | Live only |
| Plug-in parameters shown once configured | ✓ | Live only: every parameter is there | Live only: every parameter is there |
| **Live's own** | | | |
| Max for Live devices, Kumi Ears, Kumi's hands on Live's window | ✓ | Live only | Live only |
| Live's Extensions SDK, Willington | ✓ | Live only | Live only |

## Live

- Kumi's bridge is a Remote Script, with Kumi's Live extension and Willington
  beside it ([capability matrix](CAPABILITY_MATRIX.md)).
- A whole plan is one Live undo (Cmd-Z, or Ctrl-Z on Windows).
- Live's own menus do what its scripting can't: freezing, bouncing, stems,
  audio to MIDI ([Live's own commands](KUMI_GUIDE.md#lives-own-commands)).

## REAPER

Through Kumi's extension (`reaper_kumi`). Tested on REAPER 7.82 on macOS.

### Your setup

Each time Kumi looks, it reads how you set REAPER up and works with it, not
with REAPER's factory defaults:

- new items' fades (or none), loop source for new items, a new track's
  volume, the project's timebase;
- which of SWS, ReaPack, ReaImGui and js_ReaScriptAPI are installed (it uses
  one only when it's there);
- the render settings in use (source, bounds, format, sample rate, file
  pattern), which it tells you about before changing.

A file Kumi imports loops its source only when your imported media does.

### What Kumi can change

Each change is one step of REAPER's undo, and a whole plan is one step. Every
tool is in [KUMI_CHANGES](KUMI_CHANGES.md#what-kumi-can-change-in-reaper).

- **Tracks** (`track_add`, `track_move`, `track_delete` and the other
  `track_…`): added (also as a folder), moved, deleted, renamed, coloured,
  shown or hidden, folders opened or collapsed, locked; groups, templates
  (`track_template_insert`), fixed lanes and comping (`lane_plays`,
  `lane_name`, `lane_comp`), razor areas (`razor_set`).
- **Mixer** (`track_volume`, `track_pan` and the like): volume, pan and pan
  mode, width, mute, solo, polarity, channels, the send to the parent or
  master, the FX chain on or off; arming, record input, monitoring.
- **Routing:** sends and receives (`send_…`, `receive_add`), sidechains on
  3/4, hardware outputs (`hw_out_…`), MIDI to a hardware output.
- **FX** (`fx_…`): added by the name REAPER's FX browser shows, deleted,
  moved, copied, bypassed, taken offline; wet, pins, presets, whole states, FX
  chain files; containers and parallel FX; parameters (up to 256 in one step),
  ReaEQ bands; parameter modulation (LFO, audio control signal, links, learn).
- **Envelopes and automation** (`envelope_…`, `automation_item_…`,
  `automation_override`): shown and armed, points, automation items,
  automation modes.
- **Items and takes** (`item_…`, `items_glue`, `items_render`, `take_…`,
  `stretch_markers_…`): added, a file imported, moved, split, trimmed, faded,
  grouped, deleted, duplicated; glued or rendered only when you ask; takes,
  stretch markers, take markers.
- **MIDI** (`notes_…`, `midi_events_…`): notes added, changed, deleted,
  quantized, humanized; CC, pitch bend, program, pressure, text, SysEx.
- **Markers, regions and the loop** (`marker_…`, `region_add`,
  `region_render`, `ruler_lane`, `time_selection`, `loop_range`): ruler
  lanes, the region render matrix.
- **Tempo and project** (`tempo_set`, `tempo_marker_set`, `metronome_switch`,
  `project_…`): tempo markers, time signatures and ramps, the metronome, grid,
  timebase, notes, title, author.
- **Kits and goals** (`drum_kit`, `scratch_keep`).
- **Scripts** (`run_lua`, `api_call`, `run_action`).

It also plays, stops, moves the edit cursor, and saves projects (or REAPER's
next version), track templates and FX chains.

### How it listens

- Renders the part offline (faster than it plays), hears that, then plays it
  out loud for you, up to a minute. A track alone is its pre-fader stem.
- While REAPER plays, it can hear the master output as it plays (8 seconds by
  default). A render stops REAPER's playback.
- Judged changes keep their level with the last ReaLimit, else a JS: Volume
  Adjustment it adds. Goals try candidates on scratch copies ("Kumi · try …");
  the best stays on "Kumi · Goal best".

### Workflows

- **Drum kits** from sound files: a ReaSamplOmatic5000 per pad, as a folder of
  pad tracks or one container, hi-hats choking.
- **Sounds from tutorials and references:** `watch_video`, then ReaEQ,
  ReaComp, ReaSynth or your plug-ins, judged by ear.
- **Presets:** FX chains, track and project templates, JSFX and FX presets
  found in REAPER's resource folder and loaded.

### Limits

- **Only REAPER's undo** takes back `run_lua`, `api_call` and `run_action`.
  `run_action` never runs an action that opens a dialog, quits, opens, closes
  or saves projects, or renders.
- **Only Kumi's undo** takes back what REAPER's undo skips: repeat, the
  metronome switch, a ReaEQ band's type or switch, the automation override,
  the project's notes, title, author, grid and timebase, and parameter
  modulation with nothing on.
- Kumi's undo of parameter modulation turns its switches off but leaves the
  settings REAPER keeps once made; the HISTORY row says so.
- A project whose tracks are all replaced at once reads as another project:
  Kumi's references go stale and it reads again.
- `track_midi_out` needs a MIDI output device in REAPER's preferences.
- While a dialog or a render holds REAPER, changes are refused at once.

## Renoise

Through Kumi's tool (`com.kumi.Bridge.xrnx`). Tested on Renoise 3.5.4 on
macOS.

### Your setup

Each time Kumi looks, it reads how the song is set up and follows it: BPM
(a 2× or 4× convention stays), LPB, TPL, edit step, the length most patterns
run, the groups and their colours, and the tools installed (Paketti among
them).

### What Kumi can change

Each change is its own step of Renoise's undo. Every tool is in
[KUMI_CHANGES](KUMI_CHANGES.md#what-kumi-can-change-in-renoise).

- **Song** (`song_bpm`, `song_lpb`, `song_tpl`, `song_groove` and the other
  `song_…`, `metronome_switch`, `edit_settings`): BPM, LPB, TPL, groove, the
  metronome, editing settings (follow, edit step, octave, record quantize),
  name, artist, comments, headroom.
- **Tracks** (`track_…`): sequencer, send and group tracks added, moved,
  grouped, deleted, renamed, coloured, collapsed; note and effect columns;
  volume, panning, width, mute, solo, output routing and delay.
- **Pattern sequence and matrix** (`slot_…`, `loop_range`, `loop_pattern`):
  slots inserted, deleted, moved, cloned, made unique or given another
  pattern; sections, matrix mutes and colours; loops.
- **Patterns and lines** (`notes_write`, `notes_clear`, `notes_transpose`,
  `notes_humanize`, `lines_copy`, `pattern_…`, `automation_write`): notes,
  instrument, volume, panning, delay and effect columns in Renoise's notation;
  pattern names, lengths and aliases; automation.
- **Track DSPs** (`device_…`, `doofer_…`, `splitter_set`, `notepad_write`,
  `meta_destination`): native and plug-in devices, parameters, presets, chain
  files (.xrnt); Doofers and their macros, Splitters, Notepads, where a meta
  device points.
- **Instruments and samples** (`instrument_…`, `sample_…`, `keyzone_…`):
  added, duplicated, moved, loaded (.xrni, .sfz, a sample), made from sample
  files; macros; sample settings, slices, keyzones, drum-kit layouts.
- **Modulation and sample FX** (`modset_…`, `moddevice_…`, `chain_…`,
  `chaindevice_…`).
- **Phrases** (`phrase_…`): added, mapped to keys, their playback and lines;
  phrase scripts written and rendered; .xrnz files.
- **Plug-ins and MIDI** (`plugin_…`, `midi_set`): plug-in instruments, their
  parameters and programs; a MIDI instrument's output.
- **Goals and scripts** (`scratch_keep`, `run_lua`).

It also plays, stops, plays a note out loud, shows something in Renoise, and
saves the song, instruments and device presets.

### How it listens

- Renders the part offline, hears that, then plays it out loud for you, up to
  a minute. A track alone is Renoise's track solo.
- Renoise's scripts can't hear what it plays, and a render stops playback:
  while it plays, Kumi renders only a part you name.
- Judged changes keep their level with the last Maximizer, else a Gainer it
  adds.
- Renoise's demo can't render, so Kumi can't listen there; `kumi doctor` says
  so.

### Workflows

- **Instruments from sample files:** a drum kit with mute groups, or keys with
  velocity layers, in one instrument.
- **Phrases:** written in Renoise's notation, or as phrase scripts rendered to
  lines.
- **Doofers** from devices, with named macros.
- **Sounds from tutorials and references:** `watch_video`, then native devices
  or your plug-ins, judged by ear.

### Limits

- **Only Renoise's undo** takes back `run_lua`.
- **Only Kumi's undo** takes back what Renoise's undo skips: mute, solo, the
  loop, the pattern loop, the metronome switch, editing settings and a
  plug-in's auto-suspend.
- After Renoise's own undo and redo of line edits (`notes_write`, `lines_copy`,
  `phrase_write` and the like), Kumi's undo of them is refused: it can't tell
  Renoise's redo from your own identical lines. Renoise's undo still can.
- In long sessions Renoise's undo history has sometimes emptied after AU
  instruments were loaded and unloaded (cause unknown).
- FOCUS's Mixer has no meters: Renoise's API has none.
- A render of a dense song can be refused as busy for a moment; ask again.
