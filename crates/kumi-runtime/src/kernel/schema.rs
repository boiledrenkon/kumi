//! A tool input checked against the tool's own JSON schema before Kumi runs it, for the tools the model
//! didn't see up front (see `deferred`). It covers what Kumi's tool schemas use: type, properties,
//! required, additionalProperties, items, enum, const, anyOf, the numeric bounds, lengths, item and field
//! counts, and pattern. The first problem comes back in words naming the field, so the model can fix it.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use kumi_common::js::number::to_string as number_text;
use kumi_common::js::string::head;
use regex::Regex;
use serde_json::{Map, Value};

/// The first problem with `value` against `schema`, or None when it fits.
pub fn problem(schema: &Value, value: &Value) -> Option<String> {
    check(schema, value, "").err()
}

/// How a field is named in a problem: the input itself, or its path in it ("steps[2].tool").
fn field(path: &str) -> String {
    if path.is_empty() {
        "The input".into()
    } else {
        format!("`{path}`")
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.into()
    } else {
        format!("{path}.{key}")
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

fn type_words(name: &str) -> &str {
    match name {
        "string" => "a string",
        "number" => "a number",
        "integer" => "a whole number",
        "boolean" => "true or false",
        "object" => "an object",
        "array" => "a list",
        "null" => "null",
        other => other,
    }
}

fn is_type(name: &str, value: &Value) -> bool {
    match name {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|number| number.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => format!("\"{}\"", head(text, 40)),
        other => head(&other.to_string(), 40),
    }
}

/// Patterns compiled once each; one that won't compile checks nothing.
static PATTERNS: LazyLock<Mutex<HashMap<String, Option<Regex>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn matches_pattern(pattern: &str, text: &str) -> bool {
    let mut patterns = PATTERNS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let compiled = patterns.entry(pattern.to_string()).or_insert_with(|| Regex::new(pattern).ok());
    compiled.as_ref().is_none_or(|regex| regex.is_match(text))
}

fn number(schema: &Map<String, Value>, key: &str) -> Option<f64> {
    schema.get(key).and_then(Value::as_f64)
}

fn count(schema: &Map<String, Value>, key: &str) -> Option<usize> {
    schema.get(key).and_then(Value::as_u64).map(|n| n as usize)
}

fn check(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(schema) = schema.as_object() else { return Ok(()) };
    if let Some(forms) = schema.get("anyOf").and_then(Value::as_array) {
        let mut first = None;
        for form in forms {
            match check(form, value, path) {
                Ok(()) => return Ok(()),
                // The form of the value's own type explains best why it doesn't fit.
                Err(why) if form.get("type").and_then(Value::as_str).is_some_and(|name| is_type(name, value)) => return Err(why),
                Err(why) => {
                    first.get_or_insert(why);
                }
            }
        }
        if let Some(why) = first {
            return Err(why);
        }
    }
    match schema.get("type") {
        Some(Value::String(name)) if !is_type(name, value) => {
            return Err(format!("{} must be {}, not {}.", field(path), type_words(name), kind_of(value)));
        }
        Some(Value::Array(names)) if !names.iter().filter_map(Value::as_str).any(|name| is_type(name, value)) => {
            let words: Vec<&str> = names.iter().filter_map(Value::as_str).map(type_words).collect();
            return Err(format!("{} must be {}, not {}.", field(path), words.join(" or "), kind_of(value)));
        }
        _ => {}
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            let listed: Vec<String> = allowed.iter().take(24).map(shown).collect();
            let more = if allowed.len() > 24 { ", …" } else { "" };
            return Err(format!("{} must be one of {}{more}, not {}.", field(path), listed.join(", "), shown(value)));
        }
    }
    if let Some(constant) = schema.get("const") {
        if constant != value {
            return Err(format!("{} must be {}.", field(path), shown(constant)));
        }
    }
    match value {
        Value::Number(_) => {
            let n = value.as_f64().unwrap_or(f64::NAN);
            if let Some(least) = number(schema, "minimum").filter(|least| n < *least) {
                return Err(format!("{} must be at least {}, not {}.", field(path), number_text(least), number_text(n)));
            }
            if let Some(most) = number(schema, "maximum").filter(|most| n > *most) {
                return Err(format!("{} must be at most {}, not {}.", field(path), number_text(most), number_text(n)));
            }
            if let Some(above) = number(schema, "exclusiveMinimum").filter(|above| n <= *above) {
                return Err(format!("{} must be more than {}, not {}.", field(path), number_text(above), number_text(n)));
            }
        }
        Value::String(text) => {
            let length = text.chars().count();
            if let Some(least) = count(schema, "minLength").filter(|least| length < *least) {
                return Err(format!("{} must have at least {least} characters.", field(path)));
            }
            if let Some(most) = count(schema, "maxLength").filter(|most| length > *most) {
                return Err(format!("{} must have at most {most} characters, not {length}.", field(path)));
            }
            if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
                if !matches_pattern(pattern, text) {
                    return Err(format!("{} must match {pattern}, not {}.", field(path), shown(value)));
                }
            }
        }
        Value::Array(items) => {
            if let Some(least) = count(schema, "minItems").filter(|least| items.len() < *least) {
                return Err(format!("{} must have at least {least} items.", field(path)));
            }
            if let Some(most) = count(schema, "maxItems").filter(|most| items.len() > *most) {
                return Err(format!("{} must have at most {most} items, not {}.", field(path), items.len()));
            }
            if let Some(each) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    check(each, item, &format!("{path}[{index}]"))?;
                }
            }
        }
        Value::Object(fields) => {
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                if let Some(missing) = required.iter().filter_map(Value::as_str).find(|key| !fields.contains_key(*key)) {
                    return Err(format!("{} needs `{missing}`.", field(path)));
                }
            }
            if let Some(most) = count(schema, "maxProperties").filter(|most| fields.len() > *most) {
                return Err(format!("{} must have at most {most} fields.", field(path)));
            }
            let properties = schema.get("properties").and_then(Value::as_object);
            for (key, item) in fields {
                match (properties.and_then(|properties| properties.get(key)), schema.get("additionalProperties")) {
                    (Some(own), _) => check(own, item, &join(path, key))?,
                    (None, Some(Value::Bool(false))) => {
                        let known: Vec<&str> =
                            properties.map(|properties| properties.keys().map(String::as_str).collect()).unwrap_or_default();
                        let takes = if known.is_empty() {
                            "no fields".to_string()
                        } else {
                            let more = if known.len() > 24 { ", …" } else { "" };
                            format!("{}{more}", known.iter().take(24).copied().collect::<Vec<_>>().join(", "))
                        };
                        return Err(format!("{} has no field `{key}` (it takes {takes}).", field(path)));
                    }
                    (None, Some(other @ Value::Object(_))) => check(other, item, &join(path, key))?,
                    (None, _) => {}
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::problem;
    use serde_json::json;

    fn schema() -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["track", "beats"],
            "properties": {
                "track": {"type": "string", "minLength": 1, "maxLength": 8, "pattern": "^[a-z]+:\\d+$"},
                "beats": {"type": "number", "exclusiveMinimum": 0, "maximum": 64},
                "bars": {"type": "integer", "minimum": 1},
                "focus": {"type": "string", "enum": ["tone", "section"]},
                "mix": {"const": true},
                "steps": {"type": "array", "maxItems": 2, "items": {"type": "object", "required": ["tool"], "properties": {"tool": {"type": "string"}}}},
                "sample": {"anyOf": [{"type": "string", "minLength": 1}, {"type": "object", "additionalProperties": false, "properties": {"random": {"type": "boolean"}}}]},
                "either": {"type": ["string", "number"]}
            }
        })
    }

    #[test]
    fn an_input_that_fits_has_no_problem() {
        let fits = json!({"track": "track:3", "beats": 8, "bars": 2, "focus": "tone", "mix": true, "steps": [{"tool": "x"}], "sample": {"random": true}, "either": 4});
        assert_eq!(problem(&schema(), &fits), None);
    }

    #[test]
    fn each_problem_names_its_field() {
        let cases = [
            (json!({"beats": 8}), "The input needs `track`."),
            (json!({"track": "track:3", "beats": "8"}), "`beats` must be a number, not a string."),
            (json!({"track": "track:3", "beats": 0}), "`beats` must be more than 0, not 0."),
            (json!({"track": "track:3", "beats": 65}), "`beats` must be at most 64, not 65."),
            (json!({"track": "track:3", "beats": 8, "bars": 1.5}), "`bars` must be a whole number, not a number."),
            (json!({"track": "track:3", "beats": 8, "bars": 0}), "`bars` must be at least 1, not 0."),
            (json!({"track": "track:3", "beats": 8, "focus": "loud"}), "`focus` must be one of \"tone\", \"section\", not \"loud\"."),
            (json!({"track": "track:3", "beats": 8, "mix": false}), "`mix` must be true."),
            (json!({"track": "trk", "beats": 8}), "`track` must match ^[a-z]+:\\d+$, not \"trk\"."),
            (json!({"track": "track:123", "beats": 8}), "`track` must have at most 8 characters, not 9."),
            (
                json!({"track": "track:3", "beats": 8, "loud": true}),
                "The input has no field `loud` (it takes track, beats, bars, focus, mix, steps, sample, either).",
            ),
            (json!({"track": "track:3", "beats": 8, "steps": [{}, {}]}), "`steps[0]` needs `tool`."),
            (json!({"track": "track:3", "beats": 8, "steps": [{"tool": 1}]}), "`steps[0].tool` must be a string, not a number."),
            (
                json!({"track": "track:3", "beats": 8, "steps": [{"tool": "a"}, {"tool": "b"}, {"tool": "c"}]}),
                "`steps` must have at most 2 items, not 3.",
            ),
            (json!({"track": "track:3", "beats": 8, "sample": ""}), "`sample` must have at least 1 characters."),
            (json!({"track": "track:3", "beats": 8, "sample": {"words": []}}), "`sample` has no field `words` (it takes random)."),
            (json!({"track": "track:3", "beats": 8, "either": true}), "`either` must be a string or a number, not true or false."),
            (json!(["track"]), "The input must be an object, not a list."),
        ];
        for (input, want) in cases {
            assert_eq!(problem(&schema(), &input).as_deref(), Some(want), "{input}");
        }
    }

    #[test]
    fn a_pattern_that_wont_compile_checks_nothing() {
        let schema = json!({"type": "string", "pattern": "(unclosed"});
        assert_eq!(problem(&schema, &json!("anything")), None);
    }
}
