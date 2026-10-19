# kumi-logic-hands

The macOS helper behind Kumi's Logic Pro bridge, `logic-kumi`. It is one Swift file with no dependencies
beyond Cocoa, ApplicationServices and CoreMIDI. The bridge runs it as a child and speaks JSON lines with
it: one request a line on stdin, one reply a line on stdout, events in between. It knows macOS, not Logic:
it reads and writes Accessibility elements, presses menu items, posts key events to a process and opens
virtual MIDI ports, each exactly as asked. The protocol is the "Helper protocol" section of
`docs/en/LOGIC_CONTRACT.md`; this page is how the helper implements it.

The helper stays dumb on purpose: no element paths, no key command tables, nothing that changes with a
Logic version. macOS ties the Accessibility grant to the helper's content, so a helper that rarely changes
keeps the grant.

## Build

```sh
python3 scripts/build-logic-hands.py
```

writes `target/logic-hands/kumi-logic-hands-<digest>`: a universal arm64 and x86_64 binary for macOS 13
and later, ad-hoc signed, named after the first 12 hex digits of the source's SHA-256. The folder is left
holding only the helper of the current source: builds of older sources are removed, and any other file
there stops the build, as the release script's staging expects. The script is importable; `build(reuse=True)`
keeps an existing build of the same source.

A quick build for this Mac only:

```sh
xcrun swiftc -swift-version 5 -o kumi-logic-hands crates/logic-kumi/helper/KumiLogicHands.swift
```

## Run

```sh
target/logic-hands/kumi-logic-hands-<digest>
```

then one request a line:

```
{"id": 1, "op": "trusted"}
{"id":1,"ms":0,"ok":true,"trusted":true}
{"id": 2, "op": "apps", "bundles": ["com.apple.logic10"]}
{"apps":[{"active":false,"bundle":"com.apple.logic10","hidden":false,"name":"Logic Pro","pid":812}],"id":2,"ms":1,"ok":true}
{"id": 3, "op": "windows", "pid": 812}
{"id":3,"listed":["KumiLab.logicx - Tracks"],"ms":22,"ok":true,"windows":[{"document":"file:///…/KumiLab.logicx","focused":true,"fullscreen":false,"main":true,"minimized":false,"modal":false,"role":"AXWindow","subrole":"AXStandardWindow","title":"KumiLab.logicx - Tracks"}]}
```

A reply is `{"id": n, "ok": true, …}` or `{"id": n, "ok": false, "error": "…", …}`, with the request's
id as given (`null` when the request had none) and `ms`, how long the helper took. An event is
`{"event": "…", …}` with no id. Replies and events come whole, one a line, flushed.

The rules: one request at a time; a reply within 2 s, else an error (`timeout` when the work did not
finish in time, `busy` when the request before it still held the helper); logging goes to stderr only; no
app is activated unless `activate` asks; a pop-up button's children are never read unless `popups` is
true, since reading them opens its menu; EOF on stdin or `quit` ends the helper, and its MIDI ports with it.

## Ops

| Op | Parameters | Reply |
| --- | --- | --- |
| `trusted` | `prompt` (default false) | `trusted`: whether this process holds the Accessibility grant; `prompt` true shows the system's prompt |
| `apps` | `bundles` | `apps: [{bundle, pid, name, active, hidden}]` for the running apps with those bundle ids; from then on their launches, exits, activations and deactivations are `app` events |
| `processes` | `contains` | `pids`: every process whose executable path contains the text, ascending; libproc sees the XPC services NSWorkspace does not list, such as `com.apple.appkit.xpc.openAndSavePanelService` |
| `activate` | `pid` | `active`: whether the app is in front after asking, waiting up to 1 s |
| `windows` | `pid`, `menu` (the Window menu's title, default `Window`) | `windows: [{title, role, subrole, main, focused, minimized, fullscreen, modal, document}]` from `AXWindows`, `AXMainWindow` and `AXFocusedWindow` together, deduplicated; `listed`: the titles after the last separator of the app's Window menu, which still names the windows while the app's Space is not current and `windows` is empty |
| `tree` | `pid`, `root`, `path`, `depth` (default 1), `children` (default 100), `attrs`, `popups` | `node`: the element at the path and its descendants, `depth` levels down and `children` children per node; `truncated` when an array value was cut at 256 entries |
| `get` | `pid`, `root`, `path`, `attrs`, `popups` | `attrs: {name: value}` for the attribute names asked, or every attribute for `"all"`; attributes the element lacks are left out |
| `set` | `pid`, `root`, `path`, `attr`, `value`, `settleMs` (default 50) | `result` (the AXError's name), `before` and `after` (the attribute's value around the set, after waiting `settleMs`) |
| `action` | `pid`, `root`, `path`, `name`, `settleMs` | `result`, `before`, `after` (the element's `AXValue` around the action) |
| `menu` | `pid`, `titles` | `result`, `title` (the item's title as shown), `active` (whether the app was in front; menu items act only then); walks the menu bar by titles and presses the last |
| `keys` | `pid`, `key`, `mods` | `posted`, `active`; modifiers go down as their own events before the key and up after it, posted to the process |
| `type` | `pid`, `text` (up to 200 characters) | `posted`, `active`; one key event a character, carrying the character |
| `click` | `pid`, `x`, `y` (a point on the screen, as Accessibility positions are), `mods`, `count` (1 to 3) | `posted`, `active`; the app brought in front, the pointer moved to the point, the left button down and up there at the system's event tap, `count` times for a double click; the one way Logic takes a track selection from outside (mouse events posted to the process alone it ignores) |
| `midi.open` | `name`, `uid` | `opened`, `source`, `destination`: a virtual source and destination of that name with the unique ids `uid` and `uid + 1`; `uid` is a number, or a text hashed to one |
| `midi.send` | `hex` | `sent`: the bytes out of the source |
| `midi.close` | | `closed`; closing closed ports is fine |
| `tap.start`, `tap.stop` | | the error `reserved` until M5 |
| `quit` | | `{"id": n, "ok": true}`, then the helper exits |

`root` is `"app"` (the default), `"main"` (`AXMainWindow`), `"focused"` (`AXFocusedWindow`),
`"menubar"` or `{"title": "substring"}` (the first window whose title contains it); `path` is child
indexes joined by dots from that root, `"0.3.2"`, and the root itself when empty. `get`, `set` and
`action` take `popups` too: a path that passes through a pop-up button is refused without it.

A node is `{i, role, subrole, title, desc, value, valueDesc, help, id, enabled, selected, pos, size, set,
act, c, more}`: `i` the node's path from the root, `role` always, the others when the element has them
and they are not empty, `set` the settable attribute names, `act` the action names, `c` the children
listed, `more` how many children were not (beyond `children`, or all of them at the depth limit). `attrs`
picks the fields: a list of those names (or of their attribute names, `AXValue` for `value`), `"all"` for
every one, and without it every one but `set` and `act`, which cost extra calls per node. Values are
numbers, strings and booleans as they are; points `{x, y}`, sizes `{w, h}`, rectangles `{x, y, w, h}` and
ranges `{loc, len}`; other elements `{"ref": "role title"}`. A `set` takes the same shapes.

Key names are the US layout's characters and `return`, `enter`, `tab`, `space`, `delete`,
`forwarddelete`, `escape`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`,
`f1` to `f20`, `keypad0` to `keypad9`, `keypad+`, `keypad-`, `keypad*`, `keypad/`, `keypad.`,
`keypad=`, `keypadenter`, `clear`, `help`; a number is a virtual key code. Modifiers are `cmd`, `shift`,
`opt` and `ctrl`.

## Errors

`error` is one of: `malformed` (the line is not a JSON object), `unknown-op`, `bad-request` (a parameter
is missing or of the wrong shape; `detail` says which), `untrusted` (no Accessibility grant; every
Accessibility, key and menu op checks first), `no-process`, `no-app` (no running application has the
pid), `no-root`, `no-element` (the path runs out; `path` names where), `popup`, `unknown-attr`,
`no-attributes`, `no-menubar`, `no-menu-item` (`title` names the first title not found), `disabled` (the
menu item is greyed out), `unknown-key`, `unknown-mod`, `event-failed`, `too-long`, `bad-hex`,
`midi-open` (ports are open already), `midi-closed`, `midi-failed` (`status` carries CoreMIDI's), `reserved`,
`timeout`, `busy`, and for a `set`, `action` or `menu` press that the app refused, the AXError's name
(`cannotComplete`, `attributeUnsupported`, `actionUnsupported`, …) with `result`, `before` and `after`
still in the reply.

## Events

- `{"event": "midi", "hex": "90 5E 7F", "ms": n}`: one for each packet the MIDI destination receives,
  `ms` the Unix time in milliseconds. A long SysEx can arrive as several packets.
- `{"event": "app", "state": "launched" | "terminated" | "activated" | "deactivated", "pid": n, "bundle": "…"}`
  for the bundle ids of the last `apps` request.

## Test

```sh
python3 -m unittest discover -s crates/logic-kumi/helper/tests -p 'test_*.py'
```

builds the helper if needed, and `tests/midi_echo.swift`, a small CoreMIDI counterpart that sends to
the helper's destination and listens on its source, then drives the helper over its pipes: ids, errors,
the ops that need no grant, the MIDI ports both ways, EOF and `quit`. It passes on a Mac without Logic and
without the Accessibility grant; the ops that need the grant are checked for a well-formed error there.
The only MIDI byte it sends is Active Sensing, which a DAW listening on every input ignores.

## What the lab found that the helper lives with

- While Logic's Space is not the current one, `AXWindows` is empty and `AXMainWindow` is nil, but the
  Window menu still lists the project window: hence `listed`. Apps put other items after that separator
  too (Logic's "Control Surface Setup"), so the bridge matches titles it knows.
- Key combinations with modifiers and menu items act only while the app is active; `keys` and `menu`
  say `active` so the bridge can tell, and `activate` brings the app forward when asked.
- The sandboxed App Store build's open and save panels belong to
  `com.apple.appkit.xpc.openAndSavePanelService`; `processes` finds its pid, `keys` reaches it.
- Reading a pop-up button's children opens its menu, so nothing reads them unless `popups` is true.
- An app that stops answering makes each Accessibility call wait 1.5 s at most; the 2 s rule then
  answers `timeout` and the next request `busy` until the helper is free again.
