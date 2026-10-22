# Implementation status

English · [简体中文](../zh-CN/IMPLEMENTATION_STATUS.md) · [日本語](../ja/IMPLEMENTATION_STATUS.md)

Where things stand: current versions, what has been tested on real Live and
where the records are, and the known limits. What each area of Live supports
is in the [capability matrix](CAPABILITY_MATRIX.md); systems and Live versions
are in [supported platforms](SUPPORT_MATRIX.md).

## Versions

Each release in the [changelog](../../CHANGELOG.md) names the bridge it ships
with; [bridge versions](KUMI_CHANGES.md#bridge-versions) lists them all. The
version numbers themselves live in the crates' `Cargo.toml` files (Kumi, the
bridge), the root `package.json` (Kumi) and `apps/live-extension/manifest.json`
(the extension); REAPER's extension's in `crates/reaper-kumi/Cargo.toml`, and
Renoise's tool's in `daws/renoise/com.kumi.Bridge.xrnx/payload/version.lua` and
its `manifest.xml`. [Connect to REAPER or Renoise](KUMI_GUIDE.md#connect-to-reaper-or-renoise)
says which Kumi ships with which.

| Part | Version |
| --- | --- |
| Live protocol | `ableton-live/v1`; each release's `release-manifest.json` records the registry's hash |
| REAPER and Renoise protocol | `kumi-daw/1`, between Kumi's host and the bridge inside the DAW, on 127.0.0.1 |
| MCP protocol eras | `2025-11-25` and `2026-07-28` |
| Runtime | Native Rust app and standalone bridge; fresh installs require no Node |
| Older installs (Kumi 1.7.5 and earlier) | Node 22/24; retained Node also supports rollback and optional YouTube challenges ([details](SUPPORT_MATRIX.md)) |
| Logic Pro bridge (`logic-kumi`, beside Logic) | 0.1.0, on the `kumi-daw/1` wire; its shapes are in [the Logic contract](LOGIC_CONTRACT.md) |

## What has been tested where

The real-Live records below are from earlier TypeScript releases. Native CI
and migration checks do not replace a new real-Live acceptance run.

- **Every pull request:** native Rust builds and tests, the Remote Script's and
  the Live extension's tests, six target bundles, and installation and
  migration tests; see [CI](TESTING.md#ci).
- **Real Live on macOS** (Apple silicon, Live 12.4.15 beta): every kind of change
  Kumi makes, each undone through Kumi (65 of 65 on bridges 1.0.62 and 1.0.63,
  in 19- and 200-track Sets), playing, bouncing, listening and watching, offline
  renders and the right-click menu through Kumi's extension, and the Willington
  edits.
- **Real Live on Windows** (Windows 10, Live 12.4.15 beta, Kumi 1.6.0 with
  bridge 1.0.71): installing the bridge into a moved User Library, the Remote
  Script loading, and Kumi connecting. There's no record file for this yet.
- **REAPER and Renoise on macOS** (REAPER 7.82 evaluation, portable; Renoise
  3.5.4; 20 October 2026): each change applied through Kumi, undone through
  Kumi, then undone and redone with the DAW's own undo, big projects (REAPER 200
  tracks and 2,000 items, Renoise 64 tracks and 100 patterns), and listening.
  - REAPER, at commit `c80eb1e7`: 140 of its 141 kinds then, one known limit.
    `drum_kit`, `scratch_keep` and `metronome_switch` came later and weren't run.
  - Renoise, at commit `78faa00f`: 121 of 132 cases, 11 known limits.
    `instrument_kit` and `scratch_keep` came later and weren't run.
  - The limits are in [known limits](KUMI_CHANGES.md#known-limits). There's no
    record file for these yet. REAPER and Renoise on Windows and Linux are
    untested.
- **Real Logic Pro on macOS** (Apple silicon, Logic Pro 12.3.1): the lab of what
  macOS gives Kumi to read and change Logic, a go or no-go per channel (M0), and
  the bridge beside Logic attaching through its Accessibility helper and
  answering `ping` and `status`, set up by `kumi --daw logic bridge`, found by
  the doctor and started by Kumi's host from a session (M1). M2, observation:
  the bridge's reads over the real Logic are recorded in
  `crates/daw-mcp-server/tests/vectors/logic-reads.json` (the rows, the refs,
  the focus and FOCUS's bodies), the host replays them, the runtime reads them,
  opens a kept conversation's project again and tells whether Logic records, and
  `crates/kumi/examples/accept_logic.rs` is the opt-in run against the real
  Logic for the gate. M3, changes and undo: the bridge makes sixteen kinds over
  the real Logic (the header's fader, pan, mute, solo and arm, the Inspector's
  name, tracks added below the selected one and deleted, markers made, moved
  and deleted, the tempo, a region's notes as Event List rows, a plug-in's
  bypass), each read back, and names the producer's own undo back; Kumi's
  change set declares exactly those (`logic_changes.rs`), the simulated Logic
  answers their shapes over the recorded project, the host's journal keeps an
  apply or an undo the bridge couldn't confirm uncertain and never sent twice,
  HISTORY over Logic follows a Cmd-Z through `undone_in_daw`, and the
  acceptance run's M3 checks are a plan of a fader, a rename, a track added and
  muted, each undone through Kumi, and a rename taken back with Cmd-Z. Logic
  keeps no undo step for a fader, pan, mute, solo, arm, tempo or bypass, so
  those are put back by value; a deleted track comes back only through Logic's
  own undo while that step is on top. The M3 gate on this Mac is recorded in
  `docs/evidence/logic-m3-gate-2026-10-19.md`. M4, the first slice: Kumi's
  change set declares the kinds the bridge makes over the real Logic beyond
  M3's, with REAPER's tool names where the concept is the same and Logic's words
  throughout: `key_command` (the escape hatch, by the command's English name or
  id; a command Logic keeps no undo step of makes no row, and only Logic's own
  undo takes one back), the regions of the Tracks area moved along their track,
  resized and split on whole beats (the right part's ref comes back), looped,
  renamed and deleted, `notes_quantize`, the cycle range (`loop_range`, no step
  of Logic's undo), the Tempo and Signature Lists (`tempo_marker_set`,
  `tempo_marker_delete`, `signature_set`), many notes at once as a MIDI file
  Kumi writes and Logic imports onto a new track below the one named
  (`midi_import`), and Track Stacks made and flattened; and the action
  `save_project` (the host's `project_save`: a save, or a save as to a `.logicx`
  that isn't there yet; no row). A track moved and a track coloured are refused,
  saying why: the headers can't be dragged from outside (no key command reorders
  tracks, and a header offers no action but its menu), and the Color palette's
  swatches are bare positions to Accessibility while a header shows no colour,
  so a colour could never be read back. The simulated Logic answers every one of
  them but `notes_quantize` statefully over the recorded project, as the
  bridge's recorded scenes answer (their shapes, the titles Logic's Edit menu
  gives each step, the epoch moving where the bridge's moves); the host's tests
  and the wire golden cover them, the runtime's tests take them into HISTORY and
  back out (a save with no row, a key command with and without a row, a split
  and an import undone through Logic's own undo, the cycle, a tempo change, an
  undo Logic answered uncertain), and the acceptance run gains the M4 checks
  (`--files <dir>` names the folder a save as goes under; a MIDI import counts
  when its region spans the notes, since Logic's Event List lists only the rows
  on its screen). The M4 gate on this Mac, every check passed after six runs
  that each found something to fix, is recorded in
  `docs/evidence/logic-m4-gate-2026-10-21.md`.

## Evidence

The records in [`docs/evidence`](../evidence/). Where a record doesn't name its
bridge, the version given is the one in the commit that added it.

| Record | Date | DAW | Bridge | What it shows |
| --- | --- | --- | --- | --- |
| [logic-lab-2026-10-18.md](../evidence/logic-lab-2026-10-18.md) | 2026-10-18 | Logic Pro 12.3.1 | — | The Logic Pro lab (M0): a go or no-go per macOS channel (Accessibility reads and writes, key commands, the open and save panels, Mackie Control, OSC, the project file, the process tap), measured on this Mac |
| [logic-m1-gate-2026-10-19.md](../evidence/logic-m1-gate-2026-10-19.md) | 2026-10-19 | Logic Pro 12.3.1 | `logic-kumi` 0.1.0 | The M1 gate: the bridge attached to the real Logic with its helper, `ping` and `status` over the wire; the install lifecycle, the doctor, and Kumi's host starting the bridge from a session |
| [kumi-poc.md](../evidence/kumi-poc.md) | 2026-09-28 to 09-30 | 12.4.15b4, b5 | 1.0.0 to 1.0.63 | Kumi on real Live: acceptance runs, speed and big Sets, `kumi bridge`, audition and goals, a Max for Live device, reconnecting |
| [live-extension.md](../evidence/live-extension.md) | 2026-09-30 | 12.4.15b5 | 1.0.57 to 1.0.65 | How Live runs Kumi's extension; offline renders, its costs, both channels agreeing, undo, right-click |
| [lom-audit.md](../evidence/lom-audit.md), [JSON](../evidence/lom-audit-12.4.15b5.json) | 2026-09-30 | 12.4.15b5 | 1.0.55 | A census of Live's Python API against what the Remote Script uses |
| [kumi-benchmark.md](../evidence/kumi-benchmark.md) | 2026-09-30 | 12.4 beta | 1.0.52 | Recreating a section of a track by ear: runs and scores |
| [kumi-clip-follow-actions-b5.json](../evidence/kumi-clip-follow-actions-b5.json) | 2026-09-30 | 12.4.15b5 | 1.0.53 | Follow Actions and Legato through Willington, read back and undone |
| [willington-kumi-chat.json](../evidence/willington-kumi-chat.json) | 2026-09-30 | 12.4.15b4 | 1.0.52 | A Kumi conversation using the Willington edits, each undone |
| [rack-zones-b5.json](../evidence/rack-zones-b5.json) | 2026-10-01, 2026-10-02 | 12.4.15b5 | 1.0.66 | Rack chain zones through Willington: read, written, undone, saved and reopened; then signal gating, fades and Max `live.object` writes, read back and restored |
| [capability-manifest.json](../evidence/capability-manifest.json) | as of Kumi 1.7.6 (2026-10-04) | — | 1.0.74 | Every registry operation, executable or reserved, and the registry's hash; no longer regenerated |
| `phase-3` to `phase-9` files | 2026-07-26 to 07-28 | 12.4.5b8 | 0.1.0 | The bridge's first real-Live runs, before Kumi: discovery, audition, transport, clips, Arrangement, mixer, automation, devices, Browser, routing, recording, project files, events, realtime and capture; plus the [FFmpeg loudness oracle](../evidence/phase-8-audio-oracle.json) and the packaged journeys against a fake Live. History: the bridge has changed a great deal since. |

## Known limits

- Live's APIs can't save or export the Set, freeze, create group tracks or edit
  Arrangement automation; see [what Live's APIs don't offer](CAPABILITY_MATRIX.md#what-lives-apis-dont-offer).
- Some changes can't be undone through Kumi, only with Live's own undo; see
  [Live safety](LIVE_SAFETY.md).
- On Windows, Kumi's extension is untested and `kumi update` from 1.6.0 or
  earlier can fail on a tar error; see [Windows](SUPPORT_MATRIX.md#windows).
- Real-Live testing so far is on Live 12.4.15 beta, mostly on Apple silicon
  Macs. Live 12.0 to 12.3, Intel Macs and screen readers are untested.
- The Willington edits are off until `/willington` turns them on, and need a
  Live build Willington has bindings for: macOS ARM64 Live 12.4.15b4 and b5
  (rack chain zones on b5 only) and Windows x64 Live 12.4.15b5. On Windows, the
  bindings were tested through Python in Live, not yet through Kumi's own edits;
  see [Willington](WILLINGTON_INTEGRATION.md).
- Kumi and the bridge are unsigned; see [releases and distribution](DISTRIBUTION_POLICY.md).
- Logic Pro support can't name or rename a marker, insert, remove, move or set a
  plug-in, or make or set a send: a marker's name cell isn't a text field, the
  slots' pop-up menus can't be driven from outside and a plug-in's controls
  aren't reached yet; nor move or colour a track, since the headers can't be
  dragged from outside and the Color palette's swatches are bare positions that
  no header reads back; the bridge refuses each saying why. The first M4 slice
  declares the cycle range, regions, the Tempo and Signature Lists, a MIDI
  import, Track Stacks and a key command, the kinds the bridge makes over the
  real Logic. Logic has no undo block and keeps no undo step for a fader, pan,
  mute, solo, arm, tempo, bypass or the cycle, so Kumi puts those back by value;
  a deleted track or region, a split, an import and a flattened Track Stack come
  back through Logic's own undo only while that step is on top, and what a key
  command did only Logic's own undo takes back. A region moves in time only,
  along its track; its edges, a split and an import's place go on whole beats,
  and an import makes a track of its own. The tempo takes whole BPM. Saving
  keeps no versions (that is REAPER's). Logic Pro 11 and Intel Macs are out of
  scope. See [the Logic contract](LOGIC_CONTRACT.md).

## Open work

- REAPER and Renoise on Windows and Linux, and REAPER for Windows on Arm, with
  the real DAWs.
- Logic Pro: M4's second slice (sends, plug-in insertion, presets and patches
  from the Library, Smart Controls, automation, bounce and export, the
  instructions addendum), and reading a region's notes past the rows the Event
  List shows on its screen.
- Confirming where Live on Windows keeps its Extensions folder.
- The generated capability manifest still lists Browser preview as reserved,
  though the bridge offers it whenever Live's Browser can preview.
- The decisions listed under [open owner decisions](DISTRIBUTION_POLICY.md#open-owner-decisions).
