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
    let pick = json!({
        "pid": {"type": "integer", "minimum": 1, "description": format!("The {daw} process to use, from discover, when several run.")},
        "resource": {"type": "string", "description": format!("The {daw} resource or preferences folder to use, from discover, when several run.")},
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
    ]);
    if daw == Daw::Reaper {
        // REAPER's refs are GUIDs, which a copy of the project in another tab shares: the epoch says which project they came from.
        let epoch = json!({
            "type": "string",
            "description": "The epoch of the answer the ref, parent or cursor came from. If the project in the tab has changed since, the call is refused as stale instead of reading the other project.",
        });
        list[2]["inputSchema"]["properties"]["epoch"] = epoch.clone();
        list[5]["inputSchema"]["properties"]["epoch"] = epoch;
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
        list[5]["inputSchema"]["properties"]["layout"] = layout;
        // Notes in columns the track doesn't show are listed under `hidden` for a line with nothing in the shown ones; for every
        // line when asked (about six times the reading).
        let hidden = json!({"type": "boolean", "description": "Also list data in the note and effect columns the track doesn't show, on every line (under `hidden`). Without it only a line with nothing in the shown columns is looked through."});
        list[2]["inputSchema"]["properties"]["hidden"] = hidden.clone();
        list[5]["inputSchema"]["properties"]["hidden"] = hidden;
    }
    list
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
                _ => error(id, -32602, &format!("Unknown tool: {name}")),
            }
        }
        _ => error(id, -32601, "Method not found"),
    })
}
