//! Kept conversations found by id or name across every Set, with names, pins and copies beside them, an index made
//! again from the files, locks between Kumis, and keeping at least what Kumi 1.16 kept: hermetic, in temporary folders.
use kumi_runtime::core::{
    contracts::{Claim, ConversationMatch, ConversationStore, SavedConversation},
    conversations::{create_conversation_store, create_conversation_store_keeping, fresh_id, title},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const A: &str = "0123456789abcdef0123456789abcdef";
const B: &str = "fedcba9876543210fedcba9876543210";
const DAY: Duration = Duration::from_secs(86_400);

fn said(text: &str, at: i64) -> SavedConversation {
    serde_json::from_value(json!({"savedAt":at,"checkpoint":{"version":1,"messages":[
        {"role":"user","content":text},{"role":"assistant","content":"done"}
    ]}}))
    .unwrap()
}

fn age(file: &Path, ago: Duration) {
    let file = std::fs::OpenOptions::new().write(true).open(file).unwrap();
    file.set_modified(SystemTime::now() - ago).unwrap();
}

/// Kumi 1.16.3's reading of a place's folder, as it was: what a rollback reads.
mod kumi_1_16_3 {
    use super::*;
    pub fn conversation(id: &str) -> bool {
        let bytes = id.as_bytes();
        (6..=24).contains(&bytes.len()) && bytes.iter().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
    }
    pub fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .map(|dir| dir.flatten().filter_map(|entry| entry.file_name().to_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        names.retain(|name| name.strip_suffix(".json").is_some_and(conversation));
        names.sort();
        names
    }
    /// Its reader: anything but these fields is dropped, and a file without them isn't a conversation.
    pub fn read(file: &Path) -> Option<Value> {
        let value: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
        if !value["savedAt"].is_number()
            || value["checkpoint"]["version"].as_f64() != Some(1.0)
            || !value["checkpoint"]["messages"].is_array()
        {
            return None;
        }
        Some(value)
    }
    fn current(place: &Path) -> Option<String> {
        let text = std::fs::read_to_string(place.join("current")).ok()?;
        let id = text.trim().to_owned();
        conversation(&id).then_some(id)
    }
    fn modified(file: &Path) -> f64 {
        std::fs::metadata(file)
            .ok()
            .and_then(|s| s.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0)
    }
    /// Its trim: the current one and the newest others, 20 in all.
    pub fn trim(place: &Path) {
        let kept = place.join("conversations");
        let current = format!("{}.json", current(place).as_deref().unwrap_or("undefined"));
        let names = names(&kept);
        let mut others: Vec<(&String, f64)> = names.iter().filter(|n| **n != current).map(|n| (n, modified(&kept.join(n)))).collect();
        others.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (name, _) in others.into_iter().skip(20 - usize::from(names.contains(&current))) {
            std::fs::remove_file(kept.join(name)).unwrap();
        }
    }
    /// What its `kumi report` took for the last conversation: the newest .json in any place's folder.
    pub fn report_candidates(projects: &Path) -> Vec<PathBuf> {
        let mut found = vec![];
        for place in std::fs::read_dir(projects).unwrap().flatten() {
            for file in std::fs::read_dir(place.path().join("conversations")).into_iter().flatten().flatten() {
                if file.file_name().to_string_lossy().ends_with(".json") {
                    found.push(file.path());
                }
            }
        }
        found
    }
}

#[test]
fn a_new_id_is_made_again_when_its_taken() {
    let taken = ["mv66me0m3fa91c", "mv66me0m4bb000"];
    let mut made = taken.iter().map(|id| id.to_string()).chain(["mv66me0m5cc111".to_string()]);
    assert_eq!(fresh_id(|id| taken.contains(&id), || made.next().unwrap()), "mv66me0m5cc111");
}

#[tokio::test(flavor = "current_thread")]
async fn a_new_id_is_none_kept_in_any_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = create_conversation_store(dir.path());
    store.save(A, "aaaaaa000001", &said("one", 1)).await.unwrap();
    store.save(B, "bbbbbb000002", &said("two", 2)).await.unwrap();
    let id = store.new_id().await;
    assert!(kumi_1_16_3::conversation(&id) && id != "aaaaaa000001" && id != "bbbbbb000002", "{id}");
    // Two new ones in a row differ, as the clock and six random digits make them.
    assert_ne!(store.new_id().await, store.new_id().await);
}

#[tokio::test(flavor = "current_thread")]
async fn a_conversation_is_found_by_its_id_the_start_of_it_or_its_name_in_any_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = create_conversation_store(dir.path());
    store.save(A, "mv66me0m3fa91c", &said("make the bass wider", 10)).await.unwrap();
    store.save(B, "mv66zz0m3fa91c", &said("tighten the drums", 20)).await.unwrap();
    store.save("unsaved", "kq01aa000000", &said("sketch some chords", 30)).await.unwrap();
    assert!(store.rename(B, "mv66zz0m3fa91c", Some("Drum Bus")).await.unwrap());
    let one = |found: ConversationMatch| match found {
        ConversationMatch::One(row) => (row.place, row.id),
        other => panic!("{other:?}"),
    };
    assert_eq!(one(store.find("mv66me0m3fa91c").await.unwrap()), (A.to_string(), "mv66me0m3fa91c".to_string()));
    assert_eq!(one(store.find("  MV66ZZ ").await.unwrap()), (B.to_string(), "mv66zz0m3fa91c".to_string()));
    assert_eq!(one(store.find("drum bus").await.unwrap()), (B.to_string(), "mv66zz0m3fa91c".to_string()));
    assert_eq!(one(store.find("kq01").await.unwrap()), ("unsaved".to_string(), "kq01aa000000".to_string()));
    // Several fit: the producer picks, newest first.
    let ConversationMatch::Several(rows) = store.find("mv66").await.unwrap() else { panic!() };
    assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["mv66zz0m3fa91c", "mv66me0m3fa91c"]);
    // Too short to be the start of an id, a first request isn't a name, and nothing at all.
    for word in ["mv6", "make the bass wider", "nothing", ""] {
        assert_eq!(store.find(word).await.unwrap(), ConversationMatch::None, "{word}");
    }
    // Every Set's, newest first, each with its place and current flag.
    let all = store.all().await.unwrap();
    assert_eq!(all.iter().map(|r| (r.place.as_str(), r.current)).collect::<Vec<_>>(), [("unsaved", true), (B, true), (A, true)]);
    assert_eq!(all[1].title(), "Drum Bus");
    assert_eq!(all[2].title(), "make the bass wider");
}

#[tokio::test(flavor = "current_thread")]
async fn one_let_go_says_so_and_one_deleted_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let store = create_conversation_store(dir.path());
    for n in 0..22 {
        store.save(A, &format!("old{n:03}aaaaaa"), &said(&format!("request {n}"), n)).await.unwrap();
    }
    // The two oldest, unused for 100 days, beyond the 20 newest and not current.
    for n in 0..2 {
        age(&dir.path().join(A).join(format!("conversations/old{n:03}aaaaaa.json")), 100 * DAY);
    }
    store.tidy(A).await.unwrap();
    assert_eq!(store.list(A).await.unwrap().len(), 20);
    assert_eq!(store.find("old000aaaaaa").await.unwrap(), ConversationMatch::Gone);
    assert!(store.delete(A, "old005aaaaaa").await.unwrap());
    assert!(!store.delete(A, "old005aaaaaa").await.unwrap());
    assert_eq!(store.find("old005aaaaaa").await.unwrap(), ConversationMatch::None);
    assert!(store.load(A, "old005aaaaaa").await.unwrap().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn names_pins_and_copies_live_beside_the_conversation_where_kumi_1_16_reads_past_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = create_conversation_store(dir.path());
    store.save(A, "mv66me0m3fa91c", &said("make the bass wider", 10)).await.unwrap();
    assert!(store.rename(A, "mv66me0m3fa91c", Some("  Bass\nideas  ")).await.unwrap());
    assert!(store.pin(A, "mv66me0m3fa91c", true).await.unwrap());
    store.copy(A, "mv66me0m9fork1", &said("make the bass wider", 11), "mv66me0m3fa91c").await.unwrap();
    store.copy(B, "mv66me0m9copy1", &said("make the bass wider", 12), "mv66me0m3fa91c").await.unwrap();
    assert_eq!(store.claim(A, "mv66me0m9fork1"), Claim::Ours);
    assert!(!store.rename(A, "nothere00000", Some("x")).await.unwrap(), "only a kept one");
    let rows = store.all().await.unwrap();
    let row = |id: &str| rows.iter().find(|r| r.id == id).unwrap().clone();
    assert_eq!((row("mv66me0m3fa91c").name.as_deref(), row("mv66me0m3fa91c").pinned), (Some("Bass ideas"), true));
    assert_eq!(row("mv66me0m9fork1").forked_from.as_deref(), Some("mv66me0m3fa91c"));
    assert_eq!(row("mv66me0m9copy1").forked_from.as_deref(), Some("mv66me0m3fa91c"));
    // Kumi 1.16.3 reads every conversation file, lists only them, and its report never takes a name, pin or lock for one.
    for place in [A, B] {
        let kept = dir.path().join(place).join("conversations");
        let names = kumi_1_16_3::names(&kept);
        assert!(!names.is_empty());
        for name in &names {
            assert!(kumi_1_16_3::read(&kept.join(name)).is_some(), "{name}");
        }
        let all: BTreeSet<String> =
            std::fs::read_dir(&kept).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(all.iter().any(|name| name.ends_with(".meta")) && all.len() > names.len(), "{all:?}");
    }
    assert!(dir.path().join(A).join("conversations/mv66me0m9fork1.lock").exists());
    for file in kumi_1_16_3::report_candidates(dir.path()) {
        assert!(kumi_1_16_3::read(&file).is_some(), "{}", file.display());
    }
    // Unpinned and unnamed, nothing is left beside it.
    assert!(store.rename(A, "mv66me0m3fa91c", None).await.unwrap());
    assert!(store.pin(A, "mv66me0m3fa91c", false).await.unwrap());
    assert!(!dir.path().join(A).join("conversations/mv66me0m3fa91c.meta").exists());
    store.release(A, "mv66me0m9fork1");
    assert!(!dir.path().join(A).join("conversations/mv66me0m9fork1.lock").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn the_index_made_again_from_the_files_is_the_one_kept() {
    let dir = tempfile::tempdir().unwrap();
    let store = create_conversation_store(dir.path());
    std::fs::create_dir_all(dir.path().join(A)).unwrap();
    std::fs::write(
        dir.path().join(A).join("last-seen.json"),
        r#"{"version":1,"path":"/Music/Night Drive.als","name":"Night Drive","pages":[]}"#,
    )
    .unwrap();
    store.save(A, "mv66me0m3fa91c", &said("make the bass wider", 10)).await.unwrap();
    store.save(B, "mv66zz0m3fa91c", &said("tighten the drums", 20)).await.unwrap();
    store.save("unsaved", "kq01aa000000", &said("sketch some chords", 30)).await.unwrap();
    store.rename(B, "mv66zz0m3fa91c", Some("Drum Bus")).await.unwrap();
    store.pin(A, "mv66me0m3fa91c", true).await.unwrap();
    store.copy(A, "mv66me0m9fork1", &said("make the bass wider", 40), "mv66me0m3fa91c").await.unwrap();
    store.move_conversation("kq01aa000000", "unsaved", B).await.unwrap();
    store.delete(B, "mv66zz0m3fa91c").await.unwrap();
    let file = dir.path().join("conversation-index.json");
    let entries = |text: &str| {
        let mut value: Value = serde_json::from_str(text).unwrap();
        value.as_object_mut().unwrap().remove("gone");
        value
    };
    let kept = entries(&std::fs::read_to_string(&file).unwrap());
    assert_eq!(kept["places"][A]["name"], "Night Drive");
    assert_eq!(kept["conversations"].as_object().unwrap().len(), 3, "{kept}");
    let listed = store.all().await.unwrap();
    // Gone, or unreadable: made again by a look at the files.
    for broken in [None, Some("{broken")] {
        match broken {
            None => std::fs::remove_file(&file).unwrap(),
            Some(text) => std::fs::write(&file, text).unwrap(),
        }
        assert_eq!(store.all().await.unwrap(), listed);
        assert_eq!(entries(&std::fs::read_to_string(&file).unwrap()), kept);
    }
    // A file another Kumi wrote is read again.
    let other = create_conversation_store(dir.path());
    other.save(A, "mv66me0m3fa91c", &said("make the bass wider, and mono below 120 Hz", 50)).await.unwrap();
    let row = store.all().await.unwrap().into_iter().find(|r| r.id == "mv66me0m3fa91c").unwrap();
    assert_eq!((row.saved_at, row.pinned, row.set.as_deref()), (50, true, Some("Night Drive")));
}

#[tokio::test(flavor = "current_thread")]
async fn a_conversation_another_open_kumi_has_is_taken_until_that_kumi_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let store = create_conversation_store(dir.path());
    store.save(A, "mv66me0m3fa91c", &said("make the bass wider", 10)).await.unwrap();
    let lock = dir.path().join(A).join("conversations/mv66me0m3fa91c.lock");
    // Another Kumi, still running.
    let mut other = if cfg!(windows) {
        let mut command = std::process::Command::new("ping");
        command.args(["-n", "30", "127.0.0.1"]);
        command
    } else {
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        command
    }
    .stdout(std::process::Stdio::null())
    .spawn()
    .unwrap();
    let started = kumi_common::process::started_at_ms(other.id()).unwrap();
    std::fs::write(&lock, json!({"pid":other.id(),"started":started}).to_string()).unwrap();
    assert_eq!(store.claim(A, "mv66me0m3fa91c"), Claim::Taken);
    // Not let go while it's open there, however old.
    for n in 0..21 {
        store.save(A, &format!("new{n:03}aaaaaa"), &said("newer", 20 + n)).await.unwrap();
    }
    age(&dir.path().join(A).join("conversations/mv66me0m3fa91c.json"), 200 * DAY);
    store.tidy(A).await.unwrap();
    assert!(store.load(A, "mv66me0m3fa91c").await.unwrap().is_some());
    // That Kumi closed without letting go (a crash): this one takes it.
    other.kill().unwrap();
    other.wait().unwrap();
    // Windows keeps a process's record while a handle to it is open.
    drop(other);
    assert_eq!(store.claim(A, "mv66me0m3fa91c"), Claim::Ours);
    assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&lock).unwrap()).unwrap()["pid"], std::process::id());
    // A pid used again by another process (started later than the lock says) is a lock left behind, too.
    std::fs::write(&lock, json!({"pid":std::process::id(),"started":1}).to_string()).unwrap();
    assert_eq!(store.claim(A, "mv66me0m3fa91c"), Claim::Ours);
    // Its own again, it keeps it; let go, the file goes; and a store dropped lets go of what it held.
    assert_eq!(store.claim(A, "mv66me0m3fa91c"), Claim::Ours);
    store.release(A, "mv66me0m3fa91c");
    assert!(!lock.exists());
    assert_eq!(store.claim(A, "mv66me0m3fa91c"), Claim::Ours);
    drop(store);
    assert!(!lock.exists());
}

/// A tiny generator, so each run makes the same cases.
struct Random(u64);
impl Random {
    fn next(&mut self, below: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % below.max(1)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn every_conversation_kumi_1_16_3_kept_is_still_kept() {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let mut let_go = (0, 0);
    for round in 0..120 {
        let dir = tempfile::tempdir().unwrap();
        let keep_days = [0, 1, 30, 90][random.next(4) as usize];
        let count = random.next(45) as usize;
        // A few shared times make ties, which both must order alike.
        let times: Vec<Duration> = (0..1 + random.next(count as u64 + 1))
            .map(|_| DAY * random.next(200) as u32 + Duration::from_secs(random.next(5000)))
            .collect();
        let ids: Vec<String> = (0..count).map(|n| format!("c{:03}{:08x}", n, random.next(u32::MAX as u64))).collect();
        let mut setup = vec![];
        for id in &ids {
            setup.push((id.clone(), times[random.next(times.len() as u64) as usize], random.next(5) == 0));
        }
        let current = match random.next(3) {
            0 if !ids.is_empty() => Some(ids[random.next(ids.len() as u64) as usize].clone()),
            1 => Some("gone00000000".to_string()),
            _ => None,
        };
        let make = |root: &Path| {
            let place = root.join(A);
            let kept = place.join("conversations");
            std::fs::create_dir_all(&kept).unwrap();
            for (id, ago, pinned) in &setup {
                let file = kept.join(format!("{id}.json"));
                std::fs::write(&file, serde_json::to_string(&said("words", 1)).unwrap()).unwrap();
                age(&file, *ago);
                if *pinned {
                    std::fs::write(kept.join(format!("{id}.meta")), r#"{"version":1,"pinned":true}"#).unwrap();
                }
            }
            if let Some(current) = &current {
                std::fs::write(place.join("current"), current).unwrap();
            }
            kept
        };
        let before = make(&dir.path().join("before"));
        let now = make(&dir.path().join("now"));
        kumi_1_16_3::trim(&before.parent().unwrap().to_path_buf());
        create_conversation_store_keeping(dir.path().join("now"), keep_days).tidy(A).await.unwrap();
        let old: BTreeSet<String> = kumi_1_16_3::names(&before).into_iter().collect();
        let new: BTreeSet<String> = kumi_1_16_3::names(&now).into_iter().collect();
        assert!(new.is_superset(&old), "round {round}, keeping {keep_days} days: lost {:?}", old.difference(&new).collect::<Vec<_>>());
        let_go.0 += setup.len() - old.len();
        let_go.1 += setup.len() - new.len();
        // And it lets go only what's old enough, never what's pinned.
        for (id, ago, pinned) in &setup {
            if !new.contains(&format!("{id}.json")) {
                assert!(!pinned && keep_days != 0 && *ago >= DAY * keep_days, "round {round}: {id} went");
            }
        }
    }
    // Both let some go: the cases reach past the 20 newest.
    assert!(let_go.0 > let_go.1 && let_go.1 > 0, "{let_go:?}");
}

#[test]
fn a_title_is_the_name_else_the_first_request_on_one_line_with_keys_hidden() {
    assert_eq!(title(Some("  Drum Bus "), "tighten the drums"), "Drum Bus");
    assert_eq!(title(None, "tighten\nthe   drums"), "tighten the drums");
    assert_eq!(title(Some(" "), "use key sk1234567890abcdefghij1234567890abcd now"), "use key [hidden] now");
    let long = title(None, &"word ".repeat(30));
    assert!(long.ends_with('…') && long.chars().count() <= 60, "{long}");
    assert_eq!(title(None, ""), "");
}
