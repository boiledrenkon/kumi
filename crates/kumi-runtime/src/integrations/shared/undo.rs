//! undo_change: every DAW's way back for one of Kumi's changes, through its HISTORY.
use crate::core::contracts::{JsonObject, ToolResult};
use crate::core::errors::RuntimeError;
use crate::core::history::{ChangeHost, History};
use kumi_common::abort::Signal;
use serde_json::{json, Value};

/// undo_change's answer: HISTORY's undo of the change asked for (the last when none is named), with the producer's
/// line when it ends the answer. `discard` (a DAW whose undo_change takes it) puts the values back even though the
/// producer changed them since.
pub async fn undo_change<H: ChangeHost>(history: &History<H>, input: &JsonObject, signal: Signal) -> Result<ToolResult, RuntimeError> {
    let discard = input.get("discard") == Some(&json!(true));
    let outcome = history.undo(input.get("change").and_then(Value::as_str).unwrap_or("last"), signal, discard).await?;
    let reply = if input.get("final") == Some(&json!(true)) && !outcome.is_error {
        outcome.record.as_ref().filter(|r| !r.title.is_empty()).map(|r| format!("Undone: {}.", r.title))
    } else {
        None
    };
    Ok(ToolResult { text: outcome.for_model(), is_error: outcome.is_error, reply, ..Default::default() })
}
