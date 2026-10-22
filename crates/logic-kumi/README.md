# logic-kumi

Kumi's Logic Pro bridge. Logic has no plug-in API, so this bridge is not a file
Logic loads but a process beside Kumi: `logic-kumi` reaches Logic through
`kumi-logic-hands`, a helper it runs as a child that holds the macOS channels
(Accessibility, key events, MIDI), and serves Kumi's bridge wire, `kumi-daw/1`
from `daw-bridge`, on loopback exactly as the REAPER extension and the Renoise
tool do from inside their DAWs. Kumi's host (`daw-mcp-server`) finds it through
its discovery file and speaks to it as to any other bridge.

The contract (identity, status, rows, events, the helper protocol) is
`docs/en/LOGIC_CONTRACT.md`; what was measured on a Mac before any of it was
designed is `docs/evidence/logic-lab-2026-10-18.md`.

## What it does (M1)

Started, the bridge asks the helper for the running apps with Logic's bundle
identifiers (`com.apple.logic10`, `com.apple.mobilelogic`) and attaches to the
newest one, or waits for one to launch. Attached, it listens on the first free
port of Logic's range, writes its discovery file, and every 500 ms asks the
helper whether Accessibility is granted and what Logic's windows are:

- the main window's document gives a saved project's path, and so its id (the
  first 32 hex of the SHA-256 of `logic-project:<path>`); the title gives an
  unsaved project's identity (the title without its ` - Tracks` suffix);
- `busy` is `false` when Logic can take work, `modal` when the main or
  focused window is a dialog or a sheet is open, `away` when Logic reports no
  window at all (its Space isn't the current one), `loading` while the main
  window has no title;
- `permission` is `accessibility` while the helper isn't trusted; `status`
  still answers then, and nothing counts as busy.

It answers `ping` and `status` (`{project, busy, permission}`, flat as the
REAPER bridge's; the identity itself is in the welcome, where the host takes it),
sends a `project` event when the project's id or identity changes (rewriting
the discovery file first), and when Logic terminates, or is no longer among
the running apps, removes the discovery file, tells its hosts `bye` with
reason `quit`, ends the helper and exits. A signal (SIGINT, SIGTERM, SIGHUP)
does the same with reason `shutdown`.

## What it does (M2)

The bridge reads the project from Logic's window through Accessibility, in the
shapes of `docs/en/LOGIC_CONTRACT.md` and of the host's simulated Logic
(`crates/daw-mcp-server/src/simulator/logic.rs`), and serves `read`,
`snapshot`, `focus` and `focus_body` beside `ping` and `status`. Nothing in M2
changes the project.

- `src/ax/` is the Accessibility model: a walker that asks the helper for
  subtrees and attributes by root and path (`mod.rs`), Logic's elements found
  by role and label rather than by index (`paths.rs`), and the labels in the
  language Logic runs in, loaded from the `.lproj` tables of the installed
  bundle and keyed by the English string (`strings.rs`), so a moved element is
  a re-match and a translated one still matches. The contract's rules are
  built in: never a pop-up button's children, the project window addressed as
  `AXMainWindow` (or by its title when one of Logic's own windows, the Control
  Surface Setup say, is main), and nothing read while Logic's windows are away.
- The rows (`src/rows.rs`, `src/project.rs`): `project` from the control bar's
  LCD and the window's document (the sample rate from the package's
  `MetaData.plist` when the package can be read, else null); `track` from the
  track headers, with the kind, output and inputs from the Mixer's strips;
  `region` from the Tracks area's items, whose help text names the start and
  end; `note` from the Event List of the region, which the bridge selects when
  it isn't; `plugin` and `send` from the strips' slots; `marker` from the
  Marker List; `tempo` from the Tempo and Signature lists. A read opens the
  pane it needs (the Mixer, the List Editors) when it is closed and closes it
  after; the Mixer's strips are kept for five seconds, the lists for two.
- Refs are positions under an epoch, `<epoch>:<kind>:<n>` (`src/refs.rs`):
  tracks by their place in the Tracks area, regions by their place through the
  project, markers by their place in the list, notes under their region
  (`1:note:1.3`), plug-ins and sends under their track (`1:plugin:1.1`). The
  epoch moves on every track or region change seen between polls (a header
  appeared, vanished, was renamed or moved; a region added, removed or moved),
  and a ref, parent, around or cursor from an earlier epoch is refused as
  `stale`. Paging is the simulator's: `limit`, `cursor` `<epoch>.<n>`,
  `nextCursor`, `total` and `epoch` in every answer (`src/read.rs`).
- `focus` answers from a cache the run loop refreshes every 150 ms with four
  small reads (the control bar, the track headers, the regions, the List
  Editors' tabs), never from a walk of the whole window (`src/focus.rs`); the
  four bodies (`tree`, `notes`, `timeline`, `routing`) come from the same rows
  (`src/bodies.rs`).
- Events: `project` as before; `tracks` with the new epoch when it moves;
  `selection` with the selected track and region; `transport` from the control
  bar, and at once from the Mackie Control's LEDs.
- `src/mcu.rs` is the Mackie Control driver: virtual ports named "Kumi Logic"
  through the helper's `midi.*` ops, Logic's device queries answered only as a
  Mackie Control (device 0x14), the transport and select LEDs, the LCD's names
  and the timecode display decoded into the focus and the events. Logic opens
  its Control Surface Setup window when it first adds the surface; the ports
  close when the bridge stops. Selecting a track from outside is a click on
  its header's name field (the helper's `click`, at the system's event tap):
  the headers' selected state isn't settable, and the surface's select button
  adds to a selection rather than replacing it.
- The socket threads queue `read`, `snapshot` and `focus_body` for the run
  loop, which alone talks to the helper (`src/queue.rs`); `ping`, `status` and
  `focus` never wait. Without the Accessibility grant every read fails with
  words, never `busy`, and `status.permission` names the grant.

Fixtures (`tests/fixtures/<scene>/scene.json.gz`: gzipped JSON, read one with
`gunzip -c <file> | jq .`) are every helper reply the readers asked for over
the fixed request set, recorded from the real Logic by
`examples/record_fixtures.rs` and replayed by `fixtures::SceneHands` to the
bridge's own code; `tests/reads.rs` runs the set over the `scratch` scene,
checks the shapes and blesses `crates/daw-mcp-server/tests/vectors/
logic-reads.json` with `KUMI_BLESS=1`. Fixtures are the only Logic-version-
specific data: a new Logic is a re-recording.

## What it does (M3)

The bridge changes the project and undoes, answering `change_preview`,
`change_apply` and `change_restore` (and `transport_play`, `transport_stop`,
`cursor_set`, `undo_step_begin`, `undo_step_end`) over the same wire the REAPER
extension's changes use; the host's journal (`daw-mcp-server/src/changes.rs`)
is the arbiter. `src/changes/` holds it:

- `mod.rs` dispatches each kind through one macOS channel, checks the fence at
  preview and again at apply, reads the value back, and answers `applied` only
  when Logic shows it, else `uncertain`. The kinds: a track's volume, pan,
  mute, solo, arm (its header's slider or a click at its checkbox),
  rename (the Inspector's field), add and delete (the Track menu); markers
  added, moved and deleted (the Marker List); the tempo (the control bar); a
  take's notes added, changed and deleted (the Event List); a plug-in's bypass
  (the Mixer strip). The kinds Logic's Accessibility won't let the bridge make
  from outside (`loop-range`, `fx-add`/`-delete`/`-param`/`-move`,
  `send-add`/`-volume`, `track-move`, `marker-rename`) are refused with why.
- `nudge.rs` moves Logic's sliders and scrubbers: a set steps one, an increment
  ten, so an exact value is reached by tens then ones, read back after each
  step; a value that shows only as text (a fader's dB) is stepped toward until
  the text says it. `front.rs` brings Logic in front for a menu item, a key or
  a click, refusing `busy` when another app keeps the front, and refreshes the
  Edit menu's Undo and Redo titles with a key equivalent Logic doesn't own,
  only while Logic is in front (a key command of Logic's own doesn't make it
  validate them). The bridge presses no menu bar item: one left open took
  every click and key at the M3 gate, so a menu found open is closed with a
  cancel before a write gives up as `busy`.
- Restore is by value where the value can be set, else by Logic's own undo
  (Cmd-Z through the Edit menu) while the change is still Logic's top Undo step.
  A restore's refs are from the change's own epoch, carried to where the track
  or region stands now (`Project::carry_track`), so undoing a fader or a rename
  after a track was added or deleted doesn't answer `stale`.
- `undo.rs` names the producer's own undo back to Kumi: it keeps a stack of
  Kumi's steps with the title Logic gave each at apply time and, for what the
  focus tick reads anyway, a probe (a header's value, the tempo); each tick it
  watches the probes and the Edit menu's titles as they stand (the tick puts
  no input into Logic) and sends `undone_in_daw` with the `desc`s that moved. Logic keeps no undo block, so `plan_steps` is false
  and a plan is several steps of its undo.

The exact shapes, the channels, the fences and the differences from REAPER are
`docs/en/LOGIC_CONTRACT.md`, "Changes". The change scenes
(`tests/fixtures/change-<kind>/scene.json.gz`) are recorded from the real Logic
by `examples/record_changes.rs` and replayed by `tests/changes.rs`, which
compares every answer to what Logic gave and checks the `uncertain`, `busy`,
carried-ref and undo-stack paths; the M2 scenes and `tests/reads.rs` are
untouched.

## Arguments

```sh
logic-kumi [--kumi-home <dir>] [--helper <path>] [--reference <path>]
```

- `--kumi-home <dir>`: Kumi's folder; `KUMI_HOME`, else `~/.kumi`.
- `--helper <path>`: the helper binary; else `KUMI_LOGIC_HANDS`, else a
  `kumi-logic-hands-*` file beside `logic-kumi` (the one named for this
  architecture when there are several), else `target/logic-hands/` in the
  checkout, where `python3 scripts/build-logic-hands.py` builds it. Without
  one the bridge says so and exits.
- `--reference <path>`: the reference file the lifecycle writes
  (`kumi-daw-reference/1`): where the secret is, where to write the discovery
  file, the first port to try, where to log. By default
  `bridge-reference.json` beside the binary is used when it is there; without
  one this is a development run, which uses Kumi's folder directly: the secret
  `bridges/logic/bridge.secret` (made when missing, owner-only), the discovery
  folder `bridges/logic/discovery` and the logs in `bridges/logic/logs`.

`--version` and `--help` print and leave.

## Files and ports

- Ports: 127.0.0.1 only, 9790–9799, the first free one from the reference's
  port; the port is in the discovery file.
- Discovery: `<discovery folder>/logic-<pid>-<key>.json` once attached and
  listening, `pid` being Logic's and `key` the first 16 hex of the SHA-256 of
  the resource, which for Logic is its bundle identifier. Rewritten when the
  project changes, removed on stop.
- Log: `logic-<pid>.log` (the bridge's own pid) in the log folder, one file
  per run, rotated at 1 MB, runs over a week old pruned; every line goes to
  stderr too. Never in the folder the bridge's own files are installed in.
- The helper's stderr is passed into the log line by line.

## The helper

The bridge writes one JSON request per line on the helper's stdin, with an
increasing `id`, and reads replies and events from its stdout: one request at
a time, a reply within 2 s or the request has failed. A helper that exits is
started again after a backoff that doubles from 0.5 s to 10 s and starts over
once a helper has run for 10 s; one that leaves five requests in a row
unanswered is taken for stuck and started again too. Every reply carries `ms`,
how long the helper took; a refusal's `error` is a code (`unknown-op`,
`bad-request`, `untrusted`, `no-app`, `timeout`, `busy`, an AXError's name…)
with whatever else the helper says beside it. The ops the bridge uses in M1:
`apps` (which also subscribes to `app` events for Logic's bundles), `trusted`
(prompting once per run), `windows`, and `quit` on the way out. The helper's
own page is `helper/README.md`.

## Trying it on a Mac

With Logic open and the helper built:

```sh
cargo run -p logic-kumi
cargo run -p logic-kumi --example logic_lab_client -- --watch 20 --call focus '{}' --call read '{"kind":"track"}'
```

The lab client finds the bridge through its discovery file, proves the
secret, prints the welcome and `status`, then the events for the time given:
open or switch a project in Logic to see the `project` event, select a track or
press play to see `selection` and `transport`. `--call <method> <json params>`
sends any other request first and prints how long the bridge took.

To record a scene for the tests (Logic showing what the scene is named for):

```sh
cargo run -p logic-kumi --example record_fixtures -- --out crates/logic-kumi/tests/fixtures/scratch
cargo run -p logic-kumi --example record_fixtures -- --out crates/logic-kumi/tests/fixtures/scratch-keys --select 2
```

`--select <track>` selects the track through the Mackie Control first.

## Tests

`cargo test -p logic-kumi` runs without Logic or the helper: the Logic model
on canned windows, the helper's supervision on a mock and on shell scripts,
the server against daw-bridge's conformance vectors over real sockets, the
whole bridge on the mock helper (`tests/status.rs`), the reads over the
recorded scenes (`tests/reads.rs`) and the changes over the recorded
change scenes (`tests/changes.rs`). The isolated run
`KUMI_PACKAGES="daw-bridge logic-kumi" sh scripts/test-isolated.sh` keeps the
tests away from this machine's Kumi folder.
