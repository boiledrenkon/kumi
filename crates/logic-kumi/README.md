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
- `busy` is `modal` when the main or focused window is a dialog or a sheet is
  open, `away` when Logic reports no window at all (its Space isn't the
  current one), `loading` while the main window has no title;
- `permission` is `accessibility` while the helper isn't trusted; `status`
  still answers then, and nothing counts as busy.

It answers `ping` and `status` (the identity with `busy` and `permission`),
sends a `project` event when the project's id or identity changes (rewriting
the discovery file first), and when Logic terminates, or is no longer among
the running apps, removes the discovery file, tells its hosts `bye` with
reason `quit`, ends the helper and exits. A signal (SIGINT, SIGTERM, SIGHUP)
does the same with reason `shutdown`.

Rows, focus, bodies and actions come with M2.

## Arguments

```sh
logic-kumi [--kumi-home <dir>] [--helper <path>] [--reference <path>]
```

- `--kumi-home <dir>`: Kumi's folder; `KUMI_HOME`, else `~/.kumi`.
- `--helper <path>`: the helper binary; else `KUMI_LOGIC_HANDS`, else a
  `kumi-logic-hands-*` file beside `logic-kumi` (the one named for this
  architecture when there are several), else `target/logic-hands/` in the
  checkout. Without one the bridge says so and exits.
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
cargo run -p logic-kumi --example lab_client -- --watch 20
```

The lab client finds the bridge through its discovery file, proves the
secret, prints the welcome and `status`, then the events for the time given:
open or switch a project in Logic to see the `project` event. `--call <method>
<json params>` sends any other request first.

## Tests

`cargo test -p logic-kumi` runs without Logic or the helper: the Logic model
on canned windows, the helper's supervision on a mock and on shell scripts,
the server against daw-bridge's conformance vectors over real sockets, and
the whole bridge on the mock helper (`tests/status.rs`). The isolated run
`sh scripts/test-isolated.sh -p logic-kumi` keeps the tests away from this
machine's Kumi folder.
