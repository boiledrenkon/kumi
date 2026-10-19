# Logic Pro bridge contract (draft, M2)

What `logic-kumi` answers over `kumi-daw/1`, and which macOS channel each answer comes from. The
wire, handshake, discovery files and lifecycle are daw-bridge's; this page is only Logic's shapes.
Everything here was measured in the lab of 2026-10-18 (`docs/evidence/logic-lab-2026-10-18.md`).

## Channels

| Channel | Used for | Rule |
| --- | --- | --- |
| Accessibility reads | every row, focus and body | poll; never read a pop-up button's children; prefer `AXMainWindow` and the Window menu over `AXWindows` |
| Accessibility writes | values with a text field or slider, buttons, rows | set, then `AXConfirm`; read back before answering |
| Key commands | actions with a binding | activate Logic first; post modifiers as their own events |
| Menu items | actions without a binding | activate Logic first; press by index after matching the title |
| Mackie Control | selection, transport and level events; the LCD names | answer only as a Mackie Control (device 0x14) |
| Panel service | open and save panels | post keys to `com.apple.appkit.xpc.openAndSavePanelService`; set the go-to-folder field by AX |
| Process tap | listening | M5 |

## Identity

- `pid`: Logic's; `resource`: the app bundle path.
- `project.id`: no UUID exists in every project, so the id is the 32-hex SHA-256 prefix of the saved package path; unsaved projects carry an `identity` from the window title.
- `status.busy`: `modal` when `AXMainWindow` is an `AXDialog` or a sheet is open; `away` when Logic's windows are not on the current Space; `loading` while the window title is empty.
- `status.permission`: `accessibility` missing when `AXIsProcessTrusted` is false; reads then answer `failed`, never `busy`.

## Rows (`read`)

| Kind | Fields | Source |
| --- | --- | --- |
| `project` | id, identity, path, tempo, timeSignature, keySignature, sampleRate, trackCount | window document and control bar; `MetaData.plist` for sample rate |
| `track` | ref, index, name, kind (audio, instrument, external, aux, output), mute, solo, armed, monitoring, volumeDb, pan, color?, output, inputs, selected | track headers; the Inspector strip for the selected track |
| `region` | ref, trackRef, name, startBeats, endBeats, kind (audio, midi), selected, looped | the Tracks area layout items and their help text |
| `note` | ref, regionRef, position (bar, beat, division, tick), pitch, velocity, lengthTicks, channel | the Event List of the region |
| `plugin` | ref, trackRef, slot, name, bypassed, kind (instrument, effect) | the Inspector strip's insert groups |
| `send` | ref, trackRef, slot, destination, level | the send slot |
| `marker` | ref, position, name, length, text | the Marker List |
| `tempo` | bpm, signatures | control bar; the Signature List |

Refs are positional with an epoch (`<epoch>:<kind>:<n>`), as Renoise's; the epoch moves on every
track or region change the bridge observes.

## Focus and bodies

- `focus`: the selected track, the selected region, the open editor (Piano Roll, Event List, Mixer), the playhead in bars and beats, the transport (playing, recording, cycle), answered from a cache refreshed every 150 ms.
- `focus_body`: `tree` (the selected track's strip), `notes` (the selected region's notes), `timeline` (tracks as lanes with regions), `routing` (outputs and sends).

## Events

`project` (path or identity changed), `transport` (from the Mackie LEDs and timecode), `tracks`
(a header appeared, vanished or was renamed), `selection` (the Mackie select LEDs or the header's
selected state).
