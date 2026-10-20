//! MCP over JSON-RPC: initialize, ping, tools/list and tools/call for the host's tools.
use crate::host::{Host, Pick};
use crate::tools::{kinds, DEFAULT_LIMIT, MAX_LIMIT};
use daw_bridge::Daw;
use serde_json::{json, Value};

pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The tools, described for the model.
pub fn tools(host: &Host) -> Value {
    let daw = host.daw();
    let kinds = kinds(daw);
    let resource = match daw {
        Daw::Logic => "The Logic Pro to use, by its bundle id (com.apple.logic10) from discover, when several run.".to_string(),
        daw => format!("The {daw} resource or preferences folder to use, from discover, when several run."),
    };
    let pick = json!({
        "pid": {"type": "integer", "minimum": 1, "description": format!("The {daw} process to use, from discover, when several run.")},
        "resource": {"type": "string", "description": resource},
    });
    let mut list = json!([
        {
            "name": "status",
            "title": format!("{daw} status"),
            "description": format!("Whether Kumi's bridge in {daw} is connected: the {daw} and bridge versions, the open project, and whether {daw} is busy (a dialog, a render). With several {daw} instances running, pass pid or resource from discover."),
            "inputSchema": {"type": "object", "properties": pick, "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "discover",
            "title": format!("Find running {daw}"),
            "description": format!("Every running {daw} with Kumi's bridge: process id, port, resource folder, versions, open project, and whether it answers. Use it when status reports none, or to choose among several."),
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "read",
            "title": format!("Read {daw}"),
            "description": format!("Rows of one kind from the open {daw} project, a page at a time: {{items, nextCursor?, epoch}}. Pass parent (a row's ref) to read what is inside it, such as a track's items or an FX's parameters; pass the previous page's nextCursor as cursor for the next page. Or pass ref alone for the one object it names. Refs hold until the epoch changes. With several {daw} instances running, pass pid or resource from discover."),
            "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                "kind": {"type": "string", "enum": kinds.read, "description": format!("What to read in {daw}.")},
                "parent": {"type": "string", "description": "The ref of what the rows are in: a track for its items or sends, an FX for its parameters."},
                "ref": {"type": "string", "description": "The ref of one object to read instead of a page of rows."},
                "cursor": {"type": "string", "description": "The nextCursor an earlier page gave, unchanged."},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": format!("Rows in a page ({DEFAULT_LIMIT} by default).")},
            })), "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "snapshot",
            "title": format!("{daw} snapshot"),
            "description": format!("The open {daw} project on one screen: {{epoch, project, transport, tracks, selection, markers?}}. What is too big to include is named under truncated, to read with `read`."),
            "inputSchema": {"type": "object", "properties": pick, "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "focus",
            "title": format!("{daw} focus"),
            "description": format!("What the producer is looking at in {daw}: {{epoch, view, selection, transport, changed}}. Cheap enough to poll about every 150 ms; read again only when changed moves."),
            "inputSchema": {"type": "object", "properties": pick, "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "focus_body",
            "title": format!("{daw} view"),
            "description": format!("What {daw} shows in one of its views, ready to draw: pass kind, and ref (the take, track or pattern it is about) or window (the range to show) where the kind takes them. Long ones come in pages: pass the nextCursor as cursor."),
            "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                "kind": {"type": "string", "enum": kinds.bodies, "description": "Which view."},
                "ref": {"type": "string", "description": "The ref of what the view is about."},
                "around": {"type": "string", "description": "The ref of a track to centre the first page of tracks on (the arrange view): the page starts half a page above it, and says where in `from`."},
                "window": {"type": "object", "properties": {"from": {"type": "number"}, "to": {"type": "number"}}, "required": ["from", "to"], "additionalProperties": false, "description": "The range to show, in the view's own units."},
                "cursor": {"type": "string", "description": "The nextCursor an earlier page gave, unchanged."},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": "Rows in a page."},
            })), "required": ["kind"], "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "change_preview",
            "title": format!("Preview a {daw} change"),
            "description": format!("What one change would do in {daw}, without making it: {{transactionId, confirmation, prior, display, target, epoch}}. Apply it with change_apply."),
            "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                "kind": {"type": "string", "description": "The change kind."},
                "input": {"type": "object", "description": "What the kind takes."},
                "epoch": {"type": "string", "description": "The epoch the refs in input came from."},
            })), "required": ["kind", "input"], "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "change_apply",
            "title": format!("Apply a {daw} change"),
            "description": format!("Makes a previewed change in {daw}, as one named step of {daw}'s undo. The same idempotencyKey again returns the first answer and changes nothing. state is applied, or uncertain when {daw} didn't answer."),
            "inputSchema": {"type": "object", "properties": {
                "transactionId": {"type": "string"},
                "confirmation": {"type": "string", "description": "The confirmation change_preview gave."},
                "idempotencyKey": {"type": "string"},
                "title": {"type": "string", "description": format!("The change's name in {daw}'s undo history.")},
            }, "required": ["transactionId", "confirmation", "idempotencyKey"], "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "change_undo",
            "title": format!("Undo a {daw} change"),
            "description": format!("Takes a change back as a new named step in {daw}: exactly what it changed is put back. Refused (code changed) if those values changed since, unless discard."),
            "inputSchema": {"type": "object", "properties": {
                "transactionId": {"type": "string"},
                "confirmation": {"const": "undo"},
                "idempotencyKey": {"type": "string"},
                "discard": {"type": "boolean", "description": "Put the values back even if they changed since."},
            }, "required": ["transactionId", "confirmation", "idempotencyKey"], "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "transport_stop",
            "title": format!("Stop {daw}"),
            "description": format!("Stops {daw}'s playback and recording at once."),
            "inputSchema": {"type": "object", "properties": pick, "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "transport_play",
            "title": format!("Play {daw}"),
            "description": format!("Starts {daw}'s playback from where it is, or (Renoise) from sequence (a slot, from 1) and line."),
            "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                "sequence": {"type": "integer", "minimum": 1},
                "line": {"type": "integer", "minimum": 1},
                "epoch": {"type": "string"},
            })), "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "cursor_set",
            "title": format!("Move {daw}'s cursor"),
            "description": format!("Moves {daw}'s edit cursor to positionBeats (quarter notes from the start) or position (seconds); the play position follows while it plays."),
            "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                "positionBeats": {"type": "number", "minimum": 0},
                "position": {"type": "number", "minimum": 0},
                "epoch": {"type": "string"},
            })), "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "undo_step_begin",
            "title": format!("Begin one {daw} undo step"),
            "description": format!("Opens one step of {daw}'s undo around the changes that follow, where {daw} gives one: {{stepId}}. Close it with undo_step_end."),
            "inputSchema": {"type": "object", "properties": {"label": {"type": "string"}}, "required": ["label"], "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        },
        {
            "name": "undo_step_end",
            "title": format!("End the {daw} undo step"),
            "description": format!("Closes the step undo_step_begin opened; label names it in {daw}'s undo by what it did."),
            "inputSchema": {"type": "object", "properties": {"stepId": {"type": "string"}, "label": {"type": "string", "maxLength": 200}}, "required": ["stepId"], "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
        {
            "name": "change_release",
            "title": "Forget changes",
            "description": "Changes that won't be undone any more: the host forgets them (at most 64 at a time).",
            "inputSchema": {"type": "object", "properties": {
                "transactionIds": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 64},
            }, "required": ["transactionIds"], "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        },
    ]);
    // (r-tracks) Saving the project, in either DAW: a file written, nothing in the project changed, so no change and no
    // undo. The bridge's `project_save`; Renoise and Logic refuse `version`.
    let (save_title, save) = match daw {
        Daw::Reaper => ("Save the REAPER project", "Saves the open REAPER project where it is, or as its next version (version: true). A project never saved is saved only under path, a .rpp inside the folder REAPER saves projects in, and never over a file."),
        Daw::Logic => ("Save the Logic Pro project", "Saves the open Logic Pro project where it is (File > Save), or as path (a whole path to a .logicx package that isn't there yet; the project is that file from then on). A project never saved is saved only under path, and never over a file. (version is REAPER's: Logic Pro refuses it.)"),
        _ => ("Save the Renoise song", "Saves the open Renoise song where it is, or as path (a whole path to a .xrns file that isn't there yet; the song is that file from then on). A song never saved is saved only under path, and never over a file. (version is REAPER's: Renoise refuses it.)"),
    };
    if let Some(tools) = list.as_array_mut() {
        tools.push(json!({
            "name": "project_save",
            "title": save_title,
            "description": save,
            "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                "version": {"type": "boolean"},
                "path": {"type": "string"},
                "epoch": {"type": "string", "description": "The epoch of the answer the project was read in: if the project open now is another, the save is refused as stale."},
            })), "required": ["epoch"], "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        }));
    }
    if daw == Daw::Reaper {
        // REAPER's refs are GUIDs, which a copy of the project in another tab shares: the epoch says which project they came from.
        let epoch = json!({
            "type": "string",
            "description": "The epoch of the answer the ref, parent or cursor came from. If the project in the tab has changed since, the call is refused as stale instead of reading the other project.",
        });
        list[2]["inputSchema"]["properties"]["epoch"] = epoch.clone();
        list[5]["inputSchema"]["properties"]["epoch"] = epoch.clone();
        // (r-tracks) Saving tracks as a template: a file written, nothing in the project changed, so no change and no undo.
        if let Some(tools) = list.as_array_mut() {
            tools.push(json!({
                "name": "track_template_save",
                "title": "Save REAPER tracks as a template",
                "description": "Writes tracks as a track template (.RTrackTemplate) in REAPER's TrackTemplates folder (name) or at path; never over a file unless replace.",
                "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                    "trackRefs": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                    "name": {"type": "string"},
                    "path": {"type": "string"},
                    "items": {"type": "boolean"},
                    "envelopes": {"type": "boolean"},
                    "replace": {"type": "boolean"},
                    "epoch": epoch,
                })), "required": ["trackRefs", "epoch"], "additionalProperties": false},
                "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false},
            }));
        }
    }
    if daw == Daw::Renoise {
        // A pattern's lines are read one track at a time.
        list[2]["inputSchema"]["properties"]["track"] = json!({
            "type": ["integer", "string"],
            "description": "For kind line: the track (its number, or its ref) whose lines of the parent pattern to read.",
        });
        // Track refs are positions: a request names the layout its refs were made in, and older ones are carried
        // through the tracks inserted, removed or moved since.
        let layout = json!({"type": "integer", "minimum": 0, "description": "The layout the refs were read in (each answer and tracks event says it): refs from an older one are carried through the track shifts since."});
        list[2]["inputSchema"]["properties"]["layout"] = layout.clone();
        list[5]["inputSchema"]["properties"]["layout"] = layout.clone();
        list[6]["inputSchema"]["properties"]["layout"] = layout;
        // Notes in columns the track doesn't show are listed under `hidden` for a line with nothing in the shown ones; for every
        // line when asked (about six times the reading).
        let hidden = json!({"type": "boolean", "description": "Also list data in the note and effect columns the track doesn't show, on every line (under `hidden`). Without it only a line with nothing in the shown columns is looked through."});
        list[2]["inputSchema"]["properties"]["hidden"] = hidden.clone();
        list[5]["inputSchema"]["properties"]["hidden"] = hidden;
        // (M4 n-devices) A device read by ref says what a Doofer, Splitter or Notepad holds; its preset data on ask.
        list[2]["inputSchema"]["properties"]["presetData"] = json!({"type": "boolean", "description": "With ref of a device: also its preset data, its settings as Renoise's XML (for device_preset_data). A Doofer's, Splitter's or Notepad's contents come by themselves."});
        // (M4 n-devices) Actions on a device, no step of Renoise's undo: its own editor window, its settings saved as a preset.
        let own = |more: Value| {
            merged(
                &pick,
                merged(
                    &json!({"ref": {"type": "string", "description": "A device on a track"}, "epoch": {"type": "string"}, "layout": {"type": "integer", "minimum": 0}}),
                    more,
                ),
            )
        };
        if let Some(tools) = list.as_array_mut() {
            tools.push(json!({
                "name": "device_editor",
                "title": "Show a device's editor",
                "description": "Shows a plug-in's own editor window (or a *LFO's), or hides it with show false. Only when the producer asks to see it.",
                "inputSchema": {"type": "object", "properties": own(json!({"show": {"type": "boolean"}})), "required": ["ref"], "additionalProperties": false},
                "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
            }));
            tools.push(json!({
                "name": "preset_save",
                "title": "Save a device's preset",
                "description": "Saves a native device's settings as a preset file named name in Renoise's user library (Effect Presets, in the device's folder): {file, listed}. Renoise lists it in the device's presets from its next start. A preset of that name is replaced only with replace; plug-ins are refused (Renoise lists only the presets it saves for them).",
                "inputSchema": {"type": "object", "properties": own(json!({"name": {"type": "string", "maxLength": 128}, "replace": {"type": "boolean"}})), "required": ["ref", "name"], "additionalProperties": false},
                "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
            }));
        }
        // Renoise's actions (M4): a note auditioned, things selected or shown, an instrument saved to a file (n-instruments).
        // None is a change. (Saving the song is the neutral project_save, listed for both DAWs.)
        let actions = json!([
            {
                "name": "audition_note",
                "title": "Audition a note in Renoise",
                "description": "Plays a note (or notes, at once) on an instrument, out loud, for durationMs, on track (the selected one by default); sample plays that sample alone. stop: true ends what is playing.",
                "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                    "instrument": {"type": "string", "description": "The instrument's ref."},
                    "instrumentRef": {"type": "string", "description": "The instrument's ref (as instrument)."},
                    "track": {"type": "string", "description": "The track's ref to play on."},
                    "trackRef": {"type": "string", "description": "The track's ref (as track)."},
                    "note": {"type": ["string", "integer"], "description": "C-4, C#5 or 0 to 119 (48 is C-4)."},
                    "notes": {"type": "array", "items": {"type": ["string", "integer"]}, "minItems": 1, "maxItems": 16},
                    "volume": {"type": "number", "minimum": 0, "maximum": 1},
                    "durationMs": {"type": "number", "minimum": 1, "maximum": 10000},
                    "sample": {"type": "string", "description": "A sample's ref, to play it alone."},
                    "sampleRef": {"type": "string", "description": "A sample's ref (as sample)."},
                    "stop": {"type": "boolean"},
                    "epoch": {"type": "string"},
                    "layout": {"type": "integer", "minimum": 0},
                })), "additionalProperties": false},
                "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
            },
            {
                "name": "select",
                "title": "Select or show in Renoise",
                "description": "Selects a track (or its device), an instrument (or its sample or phrase), a sequence slot (or the first slot that plays a pattern) and a line; shows a middle frame, the lower frame or the pattern matrix. Not an undo step.",
                "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                    "track": {"type": "string"}, "device": {"type": "string"}, "instrument": {"type": "string"},
                    "sample": {"type": "string"}, "phrase": {"type": "string"}, "sequence": {"type": "string"}, "pattern": {"type": "string"},
                    "trackRef": {"type": "string"}, "deviceRef": {"type": "string"}, "instrumentRef": {"type": "string"},
                    "sampleRef": {"type": "string"}, "phraseRef": {"type": "string"}, "sequenceRef": {"type": "string"}, "patternRef": {"type": "string"},
                    "line": {"type": "integer", "minimum": 1},
                    "frame": {"type": "string", "enum": ["pattern", "mixer", "phrase", "keyzones", "sample", "modulation", "effects", "plugin", "midi"]},
                    "lower": {"type": "string", "enum": ["dsp", "automation", "hidden"]},
                    "matrix": {"type": "boolean"},
                    "epoch": {"type": "string"},
                    "layout": {"type": "integer", "minimum": 0},
                })), "additionalProperties": false},
                "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
            },
            {
                "name": "instrument_save",
                "title": "Save a Renoise instrument",
                "description": "Saves an instrument to an .xrni file at path (absolute; the extension is added when missing), never over a file already there unless path names that file exactly and overwrite: true. Not a change: no undo.",
                "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                    "instrumentRef": {"type": "string", "description": "The instrument."},
                    "ref": {"type": "string", "description": "The instrument (as instrumentRef)."},
                    "path": {"type": "string"},
                    "overwrite": {"type": "boolean"},
                    "epoch": {"type": "string"},
                    "layout": {"type": "integer", "minimum": 0},
                })), "required": ["path"], "additionalProperties": false},
                "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
            },
        ]);
        if let (Some(tools), Value::Array(more)) = (list.as_array_mut(), actions) {
            tools.extend(more);
        }
    }
    if daw == Daw::Reaper {
        // m4-r-fxenv: a chain saved as an FX chain file.
        list.as_array_mut().expect("a list").push(json!({
            "name": "fx_chain_save",
            "title": "Save an FX chain file",
            "description": "Saves a chain's FX (fxRefs: some of them, in the chain's order) as an FX chain file (.RfxChain) under name in REAPER's FXChains folder: no FX ids, no automation. A file of that name is replaced only with overwrite. A file, not the project: no step of REAPER's undo.",
            "inputSchema": {"type": "object", "properties": merged(&pick, json!({
                "ref": {"type": "string", "description": "The track, take, item or container whose chain to save."},
                "input": {"type": "boolean", "description": "The track's input FX chain."},
                "fxRefs": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 256},
                "name": {"type": "string", "minLength": 1, "maxLength": 256, "description": "The file's name in FXChains; sub-folders as \"Vocals/Lead\"."},
                "overwrite": {"type": "boolean"},
                "epoch": {"type": "string"},
            })), "required": ["ref", "name"], "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false},
        }));
    }
    if daw == Daw::Logic {
        // Logic's refs are positions with an epoch (`1:track:2`), as Renoise's: a track or region change moves the epoch,
        // and refs from before are refused as stale. What each kind sits in, and what each view is about.
        property(
            &mut list,
            "read",
            "parent",
            json!({
                "type": "string",
                "description": "The ref of what the rows are in: a track for its regions, plug-ins or sends; a region for its notes (required for notes).",
            }),
        );
        property(
            &mut list,
            "focus_body",
            "ref",
            json!({
                "type": "string",
                "description": "The ref of what the view is about: a track for tree and routing, a region for notes (the selected one when left out).",
            }),
        );
        property(
            &mut list,
            "focus_body",
            "around",
            json!({
                "type": "string",
                "description": "The ref of a track to centre the first page of the timeline's lanes on: the page starts half a page above it, and says where in `from`.",
            }),
        );
        property(
            &mut list,
            "focus_body",
            "window",
            json!({
                "type": "object",
                "properties": {"from": {"type": "number"}, "to": {"type": "number"}},
                "required": ["from", "to"],
                "additionalProperties": false,
                "description": "The range of the timeline to show, in beats from the project's start.",
            }),
        );
    }
    list
}

/// Sets one property of a tool's input schema, the tool found by its name.
fn property(list: &mut Value, tool: &str, name: &str, schema: Value) {
    let tools = list.as_array_mut().expect("a list of tools");
    let tool = tools.iter_mut().find(|candidate| candidate["name"] == tool).unwrap_or_else(|| panic!("no tool {tool}"));
    tool["inputSchema"]["properties"][name] = schema;
}

/// The properties of a tool: the instance pick, then its own.
fn merged(pick: &Value, own: Value) -> Value {
    let mut all = pick.as_object().cloned().unwrap_or_default();
    all.extend(own.as_object().cloned().unwrap_or_default());
    Value::Object(all)
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn valid_id(id: &Value) -> bool {
    match id {
        Value::String(text) => !text.is_empty() && text.len() <= 256,
        Value::Number(n) => n.is_i64() || n.is_u64(),
        _ => false,
    }
}

fn tool_result(value: Value, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": serde_json::to_string(&value).expect("JSON serializes")}],
        "structuredContent": value,
        "isError": is_error,
    })
}

/// The answer to one message, or None for a notification.
pub async fn handle(host: &Host, message: &Value) -> Option<Value> {
    let Some(envelope) = message.as_object() else { return Some(error(Value::Null, -32600, "Invalid Request")) };
    let method = envelope.get("method").and_then(Value::as_str);
    let id = envelope.get("id").cloned();
    if envelope.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(error(id.filter(valid_id).unwrap_or(Value::Null), -32600, "Invalid Request"));
    }
    let Some(method) = method else {
        // A response to a request this server never sends, or junk: nothing to answer.
        return id.is_none().then(|| error(Value::Null, -32600, "Invalid Request"));
    };
    let id = id?;
    if !valid_id(&id) {
        return Some(error(Value::Null, -32600, "Invalid Request"));
    }
    let params = envelope.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return Some(error(id, -32602, "Invalid params"));
    }
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(LATEST_PROTOCOL_VERSION);
            let version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&asked) { asked } else { LATEST_PROTOCOL_VERSION };
            result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "daw-mcp-server", "title": format!("Kumi's {} bridge", host.daw()), "version": env!("CARGO_PKG_VERSION")},
                }),
            )
        }
        "ping" => result(id, json!({})),
        "tools/list" => result(id, json!({"tools": tools(host)})),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else { return Some(error(id, -32602, "Invalid params")) };
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            if !arguments.is_object() {
                return Some(error(id, -32602, "Invalid params"));
            }
            match name {
                "status" => match Pick::from_arguments(&arguments) {
                    Ok(pick) => result(id, tool_result(host.status(&pick).await, false)),
                    Err(why) => result(id, tool_result(json!({"error": why}), true)),
                },
                "discover" => result(id, tool_result(host.discover().await, false)),
                "read" | "snapshot" | "focus" | "focus_body" => {
                    let (answer, is_error) = match name {
                        "read" => host.read(&arguments).await,
                        "snapshot" => host.snapshot(&arguments).await,
                        "focus" => host.focus(&arguments).await,
                        _ => host.focus_body(&arguments).await,
                    };
                    result(id, tool_result(answer, is_error))
                }
                // (M4 n-devices) Renoise's actions on a device.
                "device_editor" | "preset_save" if host.daw() == Daw::Renoise => {
                    let (answer, is_error) = host.renoise_action(name, &arguments).await;
                    result(id, tool_result(answer, is_error))
                }
                "change_preview" | "change_apply" | "change_undo" | "change_release" | "transport_stop" | "transport_play"
                | "cursor_set" | "undo_step_begin" | "undo_step_end" | "audition_note" | "select" | "instrument_save" => {
                    let (answer, is_error) = match name {
                        "audition_note" | "select" | "instrument_save" => host.action(name, &arguments).await,
                        "change_preview" => host.change_preview(&arguments).await,
                        "change_apply" => host.change_apply(&arguments).await,
                        "change_undo" => host.change_undo(&arguments).await,
                        "transport_stop" => host.transport_stop(&arguments).await,
                        "transport_play" | "cursor_set" => host.transport(name, &arguments).await,
                        "undo_step_begin" | "undo_step_end" => host.undo_step(name, &arguments).await,
                        _ => host.change_release(&arguments),
                    };
                    result(id, tool_result(answer, is_error))
                }
                // m4-r-fxenv
                "fx_chain_save" if host.daw() == Daw::Reaper => {
                    let (answer, is_error) = host.fx_chain_save(&arguments).await;
                    result(id, tool_result(answer, is_error))
                }
                // (r-tracks) Saving: the project in either DAW, tracks as a template in REAPER.
                "project_save" | "track_template_save" if name == "project_save" || host.daw() == Daw::Reaper => {
                    let (answer, is_error) = host.passthrough(name, &arguments).await;
                    result(id, tool_result(answer, is_error))
                }
                _ => error(id, -32602, &format!("Unknown tool: {name}")),
            }
        }
        _ => error(id, -32601, "Method not found"),
    })
}
