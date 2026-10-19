# Logic Pro M1 gate, 2026-10-19

Status: passed for the bridge and its helper; the host and app parts of M1 were still in their
lanes. Scope: the `logic-kumi` bridge (fbcddc5b on `feat/logic`) attached to the real Logic Pro
through the Swift helper and answered a client over `kumi-daw/1`.

| | |
| --- | --- |
| Logic Pro | 12.3.1 (6682), `com.apple.logic10`, pid 48552, the lab's scratch project open |
| macOS | 26.6.1, Apple Silicon |
| Bridge | `target/debug/logic-kumi --kumi-home <scratch home> --helper target/logic-hands/kumi-logic-hands-eaf2506e88a9` |
| Helper | built by `scripts/build-logic-hands.py` in this checkout, universal, ad-hoc signed |
| Client | `cargo run -p logic-kumi --example lab_client -- --kumi-home <scratch home> --watch 8` |
| Kumi home | a scratch folder under the session's scratchpad, never `~/.kumi` |

## What happened

1. First run, with Logic showing its audio-interface notice ("The last selected audio interface
   is not available"): the bridge attached 255 ms after start, wrote
   `bridges/logic/discovery/logic-48552-9a692d42ae0aa40f.json` with `project: null`, and logged
   "busy: Logic Pro is showing a dialog". On SIGTERM it logged "stopped" and removed the file.
2. The notice was dismissed with its OK button through Accessibility.
3. Second run: attached in 23 ms, "Logic Pro can take work", the project read as `KumiLab` with
   the derived id `31a3626e1094a7ebac8e85f7f68c8ffc` and its `.logicx` path; the discovery file
   carried the project. The client proved the secret, got the welcome in 7 ms (`daw logic`,
   `dawVersion 12.3.1`, `pid 48552`, `resource com.apple.logic10`, `methods [ping, status]`,
   `events [project]`), `ping` and `status` in under 1 ms:
   `{"project": {"id": "31a3…8ffc", "name": "KumiLab", "path": "…/KumiLab.logicx"}, "busy": false,
   "permission": null}`. After SIGTERM: "stopped", no `logic-kumi` or helper process left, the
   discovery folder empty.

## Not covered here

The host's spawn of the bridge, `kumi --daw logic`, the doctor, and the install lifecycle: they
belong to the wiring and app lanes. The Accessibility grant was already held by the terminal app
the session runs in, so no prompt was seen.
