//! Exports as Kumi writes them: redacted by `kumi report`'s redactor, pictures drawn for the page, files kept private and
//! never written over another.
use std::cell::RefCell;
use std::fs;

use chrono::FixedOffset;
use kumi::export::{export_command, import_command, render, svg_of, write_new, ExportRequest};
use kumi::report::redactor;
use kumi::tui::tty::TtyOutput;
use kumi_runtime::core::contracts::{ChangePicture, ConversationStore, KernelCheckpoint, SavedConversation, TranscriptRole};
use kumi_runtime::core::export::{Format, Source};
use kumi_runtime::kernel::budget::transcript_of;
use kumi_runtime::system::Env;
use kumi_runtime::{create_conversation_store, IMPORTED};
use serde_json::{json, Value};

const HOME: &str = "/Users/janedoe";
const USER: &str = "janedoe";
const KEY: &str = "sk-proj-Q2xheXRvbkVxdWFsaXplcjEyMzQ1Njc4OTA";
const BEARER: &str = "abc123def456ghi789";
const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
/// A sign-in Kumi keeps: known to be secret, whatever it looks like.
const SIGNED_IN: &str = "refresh-token-value-0123";
const NOW: i64 = 1_760_400_000_000;

fn zone() -> FixedOffset {
    FixedOffset::east_opt(9 * 3600).unwrap()
}

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
            let out = render(format, &source, full, Some(redact), zone(), NOW);
            for secret in [KEY, BEARER, JWT, HOME, USER, SIGNED_IN] {
                assert!(!out.contains(secret), "{format:?} full={full}: {secret} survived:\n{out}");
            }
            assert!(out.contains("~/Music/Song.als"), "{format:?} full={full}: the home folder wasn't shortened:\n{out}");
            assert!(out.contains("Bearer [secret]"), "{format:?} full={full}:\n{out}");
            assert!(out.contains("Louder kick, please."), "{format:?} full={full}: the producer's words are missing");
        }
    }
    let raw = render(Format::Markdown, &source, false, None, zone(), NOW);
    assert!(raw.contains(KEY), "--raw leaves the file as it is");
    assert!(raw.contains("Nothing was taken out: this file may hold keys, tokens and paths."));
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

#[derive(Default)]
struct Out(RefCell<String>);
impl TtyOutput for Out {
    fn is_tty(&self) -> bool {
        false
    }
    fn columns(&self) -> Option<i32> {
        None
    }
    fn rows(&self) -> Option<i32> {
        None
    }
    fn write(&self, s: &str) {
        self.0.borrow_mut().push_str(s)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn kumi_export_saves_a_kept_conversation_and_kumi_import_keeps_it_to_carry_on() {
    let home = tempfile::tempdir().unwrap();
    let kumi = |name: &str| Env::from([("KUMI_HOME".to_string(), home.path().join(name).display().to_string())]);
    let here = kumi("here");
    let store = create_conversation_store(home.path().join("here/projects"));
    store.save("0123456789abcdef0123456789abcdef", "k3x9a1b2c4", &conversation()).await.unwrap();
    let folder = home.path().join("files");
    fs::create_dir(&folder).unwrap();
    let (out, err) = (Out::default(), Out::default());
    let ask = |conversation: Option<&str>| ExportRequest {
        conversation: conversation.map(str::to_string),
        format: Format::Json,
        out: Some(folder.display().to_string()),
        full: false,
        raw: false,
    };
    assert_eq!(export_command(ask(Some("zzzz9")), &here, &out, &err, "kumi").await.unwrap(), 1);
    assert!(err.0.borrow().contains("No conversation zzzz9. kumi -r lists them."), "{}", err.0.borrow());
    assert_eq!(export_command(ask(Some("k3x9")), &here, &out, &err, "kumi").await.unwrap(), 0);
    assert_eq!(export_command(ask(None), &here, &out, &err, "kumi").await.unwrap(), 0);
    assert!(out.0.borrow().contains("Your latest conversation: Louder kick, please. · k3x9a1b2c4"), "{}", out.0.borrow());
    let mut files: Vec<_> = fs::read_dir(&folder).unwrap().map(|entry| entry.unwrap().path()).collect();
    files.sort();
    assert_eq!(
        files,
        [folder.join("kumi-louder-kick-please-k3x9a1b2c4-2.json"), folder.join("kumi-louder-kick-please-k3x9a1b2c4.json")],
        "the second export goes beside the first"
    );
    for file in &files {
        let text = fs::read_to_string(file).unwrap();
        for secret in [KEY, BEARER, JWT] {
            assert!(!text.contains(secret), "{secret} survived in {}", file.display());
        }
        assert!(out.0.borrow().contains(&format!("Saved for Kumi: {}", file.display())));
    }
    // Another Kumi keeps it, with its id while that's free, to carry on with kumi -r.
    let there = kumi("there");
    assert_eq!(import_command(&files[1].display().to_string(), &there, &out, &err, "kumi").await.unwrap(), 0);
    assert!(out.0.borrow().contains("as k3x9a1b2c4. Carry it on: kumi -r k3x9a1b2c4"), "{}", out.0.borrow());
    let kept = create_conversation_store(home.path().join("there/projects"));
    let rows = kept.all().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].place.as_str(), rows[0].id.as_str()), (IMPORTED, "k3x9a1b2c4"));
    assert_eq!(rows[0].title(), "Louder kick, please. (imported)");
    let saved = kept.load(IMPORTED, "k3x9a1b2c4").await.unwrap().unwrap();
    let said: Vec<String> = transcript_of(&saved.checkpoint.messages)
        .into_iter()
        .filter(|line| line.role == TranscriptRole::User)
        .map(|line| line.text)
        .collect();
    assert_eq!(said.len(), 1);
    assert!(said[0].starts_with("Louder kick, please. [secret] Bearer [secret] [secret]"), "{}", said[0]);
    assert_eq!(import_command(&files[0].display().to_string(), &there, &out, &err, "kumi").await.unwrap(), 0);
    let ids: Vec<String> = kept.all().await.unwrap().into_iter().map(|row| row.id).collect();
    assert_eq!(ids.len(), 2, "an id that's taken gets another: {ids:?}");
    let not_kumis = home.path().join("other.json");
    fs::write(&not_kumis, "{}").unwrap();
    assert_eq!(import_command(&not_kumis.display().to_string(), &there, &out, &err, "kumi").await.unwrap(), 1);
    assert!(err.0.borrow().contains("That file isn't a Kumi conversation."));
}
