//! Exports as Kumi writes them: redacted by `kumi report`'s redactor, pictures drawn for the page, files kept private and
//! never written over another.
use std::fs;

use kumi::export::{render, svg_of, write_new};
use kumi::report::redactor;
use kumi_runtime::core::contracts::{ChangePicture, KernelCheckpoint, SavedConversation};
use kumi_runtime::core::export::{Format, Source};
use serde_json::{json, Value};

const HOME: &str = "/Users/janedoe";
const USER: &str = "janedoe";
const KEY: &str = "sk-proj-Q2xheXRvbkVxdWFsaXplcjEyMzQ1Njc4OTA";
const BEARER: &str = "abc123def456ghi789";
const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
/// A sign-in Kumi keeps: known to be secret, whatever it looks like.
const SIGNED_IN: &str = "refresh-token-value-0123";
const NOW: i64 = 1_760_400_000_000;

/// Every secret planted in `text`'s kind of place.
fn planted(text: &str) -> String {
    format!("{text} {KEY} Bearer {BEARER} {JWT} {HOME}/Music/Song.als {USER}'s mix {SIGNED_IN}")
}

fn conversation() -> SavedConversation {
    let messages = vec![
        json!({"role": "user", "content": [{"type": "text", "text": planted("Louder kick, please.")}]}),
        json!({"role": "assistant", "content": [
            {"type": "text", "text": planted("Looking.")},
            {"type": "tool-call", "toolCallId": "toolu_1", "toolName": "set_mixer", "input": {"track": planted("Kick")}}
        ]}),
        json!({"role": "tool", "content": [
            {"type": "tool-result", "toolCallId": "toolu_1", "toolName": "set_mixer", "output": {"type": "text", "value": planted("Done")}}
        ]}),
        json!({"role": "assistant", "content": [{"type": "text", "text": planted("Raised it.")}]}),
    ];
    let changes: Value = json!([{
        "id": "c1",
        "family": "mixer",
        "title": planted("Volume -6 → -3 dB"),
        "track": {"name": planted("Kick")},
        "state": "applied",
        "at": NOW - 1000,
        "picture": {"kind": "values", "values": [{"name": planted("Volume"), "fromText": "-6 dB", "toText": "-3 dB"}]}
    }]);
    SavedConversation {
        saved_at: NOW - 100,
        checkpoint: KernelCheckpoint { version: 1, messages, origin: Some("openai/gpt-test".into()), tools: None },
        changes: Some(serde_json::from_value(changes).unwrap()),
        first: Some("Louder kick, please.".into()),
        turns: Some(1),
    }
}

#[test]
fn no_planted_secret_survives_any_export() {
    let conversation = conversation();
    let title = planted("Louder kick");
    let place = planted("Song");
    let source = Source {
        id: "k3x9a1b2",
        conversation: &conversation,
        title: &title,
        place: Some(&place),
        words: ("Live", "Set"),
        created: NOW - 5000,
    };
    let secrets = vec![SIGNED_IN.to_string(), String::new()];
    let redact: &dyn Fn(&str) -> String = &redactor(&secrets, HOME, USER);
    for format in [Format::Markdown, Format::Html, Format::Json] {
        for full in [false, true] {
            let out = render(format, &source, full, Some(redact), NOW);
            for secret in [KEY, BEARER, JWT, HOME, USER, SIGNED_IN] {
                assert!(!out.contains(secret), "{format:?} full={full}: {secret} survived:\n{out}");
            }
            assert!(out.contains("~/Music/Song.als"), "{format:?} full={full}: the home folder wasn't shortened:\n{out}");
            assert!(out.contains("Bearer [secret]"), "{format:?} full={full}:\n{out}");
            assert!(out.contains("Louder kick, please."), "{format:?} full={full}: the producer's words are missing");
        }
    }
    let raw = render(Format::Markdown, &source, false, None, NOW);
    assert!(raw.contains(KEY), "--raw leaves the file as it is");
    assert!(raw.contains("Nothing was taken out: this file may hold keys and paths."));
}

#[test]
fn files_go_beside_others_never_over_them_and_only_the_producer_reads_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kumi-louder-kick-k3x9a1b2.md");
    let written: Vec<_> = ["one", "two", "three"].iter().map(|text| write_new(&path, text).unwrap()).collect();
    assert_eq!(
        written,
        [path.clone(), dir.path().join("kumi-louder-kick-k3x9a1b2-2.md"), dir.path().join("kumi-louder-kick-k3x9a1b2-3.md")]
    );
    assert_eq!(fs::read_to_string(&written[0]).unwrap(), "one", "a file was written over");
    assert_eq!(fs::read_to_string(&written[2]).unwrap(), "three");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in &written {
            assert_eq!(fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600, "{}", file.display());
        }
    }
    let missing = dir.path().join("no such folder").join("kumi.md");
    assert!(write_new(&missing, "x").is_err(), "a folder that isn't there is an error, not a guess");
}

#[test]
fn pictures_draw_in_the_pages_own_colours_and_load_nothing() {
    let eq: ChangePicture = serde_json::from_value(json!({
        "kind": "eq",
        "before": [{"shape": "bell", "freq": 1000, "gainDb": 0}],
        "after": [{"shape": "bell", "freq": 1000, "gainDb": 4}]
    }))
    .unwrap();
    let svg = svg_of(&eq).unwrap();
    assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 400 160\""), "{svg}");
    for role in ["class=\"now\"", "class=\"ghost\"", "class=\"guide\""] {
        assert!(svg.contains(role), "{role} is missing: {svg}");
    }
    for outside in ["<script", "href", "<image", "url(", "style=", "<foreignObject"] {
        assert!(!svg.contains(outside), "{outside}: {svg}");
    }
    let notes: ChangePicture = serde_json::from_value(json!({
        "kind": "notes",
        "length": 4,
        "before": [[0, 60, 1, 100], [1, 62, 1, 100]],
        "after": [[0, 60, 1, 100], [2, 64, 1, 100]]
    }))
    .unwrap();
    let svg = svg_of(&notes).unwrap();
    assert!(svg.contains("<rect class=\"dim\"") && svg.contains("<rect class=\"now\"") && svg.contains("<rect class=\"ghost\""), "{svg}");
    let values: ChangePicture =
        serde_json::from_value(json!({"kind": "values", "values": [{"name": "Drive", "from": 1, "to": 2}]})).unwrap();
    assert_eq!(svg_of(&values), None, "values read as words on the page");
}
