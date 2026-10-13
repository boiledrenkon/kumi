//! Each Set's kept conversations, on disk: `<place>/conversations/<id>.json`, with `<place>/current` naming the one
//! the place carries on with.

use crate::{
    core::{
        contracts::{
            ChangeState, ConversationStore, ConversationSummary, CurrentConversation, FoundExchange, SavedConversation, TranscriptRole,
        },
        errors::RuntimeError,
    },
    kernel::budget::transcript_of,
};
use async_trait::async_trait;
use kumi_common::js::{
    json::stringify,
    string::{head, trim},
};
use rand::TryRngCore;
use regex::Regex;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::LazyLock,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MAX_CONVERSATION_BYTES: usize = 256 * 1024;
const MAX_KEPT_CHANGES: usize = 100;
const MAX_KEPT: usize = 20;
fn error(e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::plain(e.to_string())
}
pub(crate) async fn write_privately(folder: &Path, name: &str, text: &str) -> Result<(), RuntimeError> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(folder).await.map_err(error)?;
    let temporary = folder.join(format!(".{name}-{}", uuid::Uuid::new_v4()));
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).await?;
        file.write_all(text.as_bytes()).await?;
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&temporary, folder.join(name)).await
    }
    .await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error(e));
    }
    Ok(())
}
pub(crate) async fn remove(file: impl AsRef<Path>) -> Result<(), RuntimeError> {
    match tokio::fs::remove_file(file).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(error(e)),
    }
}
pub fn new_conversation_id(at: i64) -> String {
    let mut number = at.unsigned_abs();
    let mut digits = Vec::new();
    loop {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(number % 36) as usize] as char);
        number /= 36;
        if number == 0 {
            break;
        }
    }
    if at < 0 {
        digits.push('-')
    }
    let mut bytes = [0; 3];
    rand::rngs::OsRng.try_fill_bytes(&mut bytes).expect("OS randomness");
    format!("{}{}", digits.into_iter().rev().collect::<String>(), hex::encode(bytes))
}
static PLACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([0-9a-f]{32}|unsaved)$").unwrap());
static CONVERSATION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-z]{6,24}$").unwrap());
/// The Set's name in a baseline file: its first key `name`, which comes before the pages.
static BASELINE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[{,]"name":("(?:[^"\\]|\\.)*")"#).unwrap());
/// The most kept conversations a search reads, newest first.
const MAX_SEARCHED: usize = 400;
pub(crate) async fn modified_ms(path: &Path) -> f64 {
    tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|s| s.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}
pub struct FileConversationStore {
    directory: PathBuf,
}
pub fn create_conversation_store(directory: impl Into<PathBuf>) -> Rc<FileConversationStore> {
    Rc::new(FileConversationStore { directory: directory.into() })
}
impl FileConversationStore {
    fn folder(&self, place: &str) -> Result<PathBuf, RuntimeError> {
        if !PLACE.is_match(place) {
            return Err(error("invalid place"));
        }
        Ok(self.directory.join(place))
    }
    fn kept(&self, place: &str) -> Result<PathBuf, RuntimeError> {
        Ok(self.folder(place)?.join("conversations"))
    }
    fn file(&self, place: &str, id: &str) -> Result<PathBuf, RuntimeError> {
        if !CONVERSATION.is_match(id) {
            return Err(error("invalid conversation id"));
        }
        Ok(self.kept(place)?.join(format!("{id}.json")))
    }
    async fn read(file: &Path) -> Option<SavedConversation> {
        let bytes = tokio::fs::read(file).await.ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        if !value["savedAt"].is_number()
            || value["checkpoint"]["version"].as_f64() != Some(1.0)
            || !value["checkpoint"]["messages"].is_array()
        {
            return None;
        }
        let mut kept = json!({"savedAt":value["savedAt"],"checkpoint":value["checkpoint"]});
        for key in ["changes", "first", "turns"] {
            let allowed = match key {
                "changes" => value[key].is_array(),
                "first" => value[key].is_string(),
                _ => value[key].is_number(),
            };
            if allowed {
                kept[key] = value[key].clone()
            }
        }
        serde_json::from_value(kept).ok()
    }
    async fn current_id(&self, place: &str) -> Option<String> {
        let folder = self.folder(place).ok()?;
        let text = tokio::fs::read_to_string(folder.join("current")).await.ok()?;
        let id = trim(&text);
        CONVERSATION.is_match(id).then(|| id.to_owned())
    }
    async fn migrate(&self, place: &str) -> Result<(), RuntimeError> {
        let legacy = self.folder(place)?.join("conversation.json");
        let Some(value) = Self::read(&legacy).await else { return Ok(()) };
        let id = new_conversation_id(value.saved_at);
        write_privately(&self.kept(place)?, &format!("{id}.json"), &stringify(&serde_json::to_value(&value).map_err(error)?)).await?;
        if self.current_id(place).await.is_none() {
            write_privately(&self.folder(place)?, "current", &id).await?;
        }
        remove(&legacy).await
    }
    async fn names(&self, place: &str) -> Vec<String> {
        let Ok(folder) = self.kept(place) else { return vec![] };
        let Ok(mut dir) = tokio::fs::read_dir(folder).await else { return vec![] };
        let mut names = vec![];
        while let Ok(Some(entry)) = dir.next_entry().await {
            if let Some(name) = entry.file_name().to_str() {
                if name.strip_suffix(".json").is_some_and(|id| CONVERSATION.is_match(id)) {
                    names.push(name.to_owned());
                }
            }
        }
        names.sort();
        names
    }
    /// A saved Set's name, read from the start of its baseline (the pages after it can be megabytes).
    async fn set_name(&self, place: &str) -> Option<String> {
        if place == "unsaved" {
            return None;
        }
        let file = tokio::fs::File::open(self.folder(place).ok()?.join("last-seen.json")).await.ok()?;
        let mut start = Vec::with_capacity(16 * 1024);
        file.take(16 * 1024).read_to_end(&mut start).await.ok()?;
        let text = String::from_utf8_lossy(&start);
        let quoted = BASELINE_NAME.captures(&text)?.get(1)?.as_str();
        serde_json::from_str::<String>(quoted).ok().filter(|name| !trim(name).is_empty())
    }
    async fn trim(&self, place: &str) -> Result<(), RuntimeError> {
        let current = format!("{}.json", self.current_id(place).await.as_deref().unwrap_or("undefined"));
        let names = self.names(place).await;
        let mut others = vec![];
        for name in names.iter().filter(|n| **n != current) {
            let at = tokio::fs::metadata(self.kept(place)?.join(name))
                .await
                .ok()
                .and_then(|s| s.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            others.push((name, at));
        }
        others.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (name, _) in others.into_iter().skip(MAX_KEPT - usize::from(names.contains(&current))) {
            remove(self.kept(place)?.join(name)).await?
        }
        Ok(())
    }
}
/// A message as kept on disk: a picture the producer added is named, not kept, so one screenshot
/// can't crowd out the conversation.
fn without_pictures(message: &Value) -> Value {
    let Some(parts) = message["content"].as_array().filter(|_| message["role"] == "user") else { return message.clone() };
    if !parts.iter().any(|part| part["type"] == "file") {
        return message.clone();
    }
    let mut kept = message.clone();
    kept["content"] = parts
        .iter()
        .map(|part| {
            if part["type"] != "file" {
                return part.clone();
            }
            let name = part["filename"].as_str().unwrap_or("a picture");
            json!({"type":"text","text":format!("[The producer showed {name} here; pictures aren't kept with saved conversations.]")})
        })
        .collect();
    kept
}
// Disk checkpoints also contain older string-form messages; keep those forms intact.
fn bounded_messages(all: &[Value]) -> Vec<Value> {
    let sizes: Vec<_> = all.iter().map(|m| stringify(m).len() + 1).collect();
    let mut total = sizes.iter().sum::<usize>() + 1;
    let mut start = 0;
    while start < all.len() && total > MAX_CONVERSATION_BYTES {
        loop {
            total -= sizes[start];
            start += 1;
            if start >= all.len() || all[start]["role"] == "user" {
                break;
            }
        }
    }
    let mut messages = all[start..].to_vec();
    if messages.is_empty() {
        if let Some(last) = all.iter().rposition(|m| m["role"] == "user") {
            if stringify(&json!(&all[last..])).len() <= 4 * MAX_CONVERSATION_BYTES {
                messages = all[last..].to_vec();
            }
        }
    }
    if !messages.is_empty() && messages.len() < all.len() {
        const SHORT: &str = "[Kumi removed the earlier part of this conversation to save room.]\n\n";
        if messages[0]["role"] == "user" {
            if let Some(content) = messages[0]["content"].as_array_mut() {
                if let Some(part) = content.iter_mut().find(|p| p["type"] == "text") {
                    if let Some(text) = part["text"].as_str() {
                        if !text.starts_with(SHORT) {
                            part["text"] = json!(format!("{SHORT}{text}"))
                        }
                    }
                } else {
                    content.insert(0, json!({"type":"text","text":trim(SHORT)}));
                }
            }
        }
    }
    messages
}
#[async_trait(?Send)]
impl ConversationStore for FileConversationStore {
    async fn current(&self, place: &str) -> Result<Option<CurrentConversation>, RuntimeError> {
        if self.migrate(place).await.is_err() {
            return Ok(None);
        }
        let Some(id) = self.current_id(place).await else { return Ok(None) };
        let Some(conversation) = Self::read(&self.file(place, &id)?).await else { return Ok(None) };
        Ok(Some(CurrentConversation { id, conversation }))
    }
    async fn load(&self, place: &str, id: &str) -> Result<Option<SavedConversation>, RuntimeError> {
        if self.migrate(place).await.is_err() {
            return Ok(None);
        }
        let Ok(file) = self.file(place, id) else { return Ok(None) };
        Ok(Self::read(&file).await)
    }
    async fn save(&self, place: &str, id: &str, conversation: &SavedConversation) -> Result<(), RuntimeError> {
        let all: &Vec<Value> = &conversation.checkpoint.messages.iter().map(without_pictures).collect();
        let messages = bounded_messages(all);
        if messages.is_empty() {
            return Ok(());
        }
        let said: Vec<_> = transcript_of(all).into_iter().filter(|l| l.role == TranscriptRole::User).collect();
        let mut checkpoint = conversation.checkpoint.clone();
        checkpoint.messages = messages;
        let saved = SavedConversation {
            saved_at: conversation.saved_at,
            checkpoint,
            changes: conversation
                .changes
                .as_ref()
                .filter(|c| !c.is_empty())
                .map(|c| c[c.len().saturating_sub(MAX_KEPT_CHANGES)..].to_vec()),
            first: Some(head(conversation.first.as_deref().or_else(|| said.first().map(|l| l.text.as_str())).unwrap_or(""), 200)),
            turns: Some(conversation.turns.unwrap_or(0).max(said.len() as u32)),
        };
        self.file(place, id)?;
        write_privately(&self.kept(place)?, &format!("{id}.json"), &stringify(&serde_json::to_value(saved).map_err(error)?)).await?;
        write_privately(&self.folder(place)?, "current", id).await?;
        self.trim(place).await
    }
    async fn fresh(&self, place: &str) -> Result<(), RuntimeError> {
        let folder = self.folder(place)?;
        if self.current_id(place).await.is_some() {
            write_privately(&folder, "current", "").await?
        }
        Ok(())
    }
    async fn list(&self, place: &str) -> Result<Vec<ConversationSummary>, RuntimeError> {
        if self.migrate(place).await.is_err() {
            return Ok(vec![]);
        }
        let current = self.current_id(place).await;
        let mut rows = vec![];
        for name in self.names(place).await {
            let Some(value) = Self::read(&self.kept(place)?.join(&name)).await else { continue };
            let id = name[..name.len() - 5].to_owned();
            let said: Vec<_> = transcript_of(&value.checkpoint.messages).into_iter().filter(|l| l.role == TranscriptRole::User).collect();
            rows.push(ConversationSummary {
                id: id.clone(),
                saved_at: value.saved_at,
                first: value.first.unwrap_or_else(|| said.first().map(|l| head(&l.text, 200)).unwrap_or_default()),
                turns: value.turns.unwrap_or(said.len() as u32),
                current: Some(&id) == current.as_ref(),
            });
        }
        rows.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
        Ok(rows)
    }
    async fn move_conversation(&self, id: &str, from: &str, to: &str) -> Result<(), RuntimeError> {
        let file = self.file(from, id)?;
        let Some(value) = Self::read(&file).await else { return Ok(()) };
        self.file(to, id)?;
        write_privately(&self.kept(to)?, &format!("{id}.json"), &stringify(&serde_json::to_value(value).map_err(error)?)).await?;
        write_privately(&self.folder(to)?, "current", id).await?;
        remove(&file).await?;
        if self.current_id(from).await.as_deref() == Some(id) {
            write_privately(&self.folder(from)?, "current", "").await?
        }
        self.trim(to).await
    }
    async fn search(
        &self,
        words: &[String],
        needed: usize,
        limit: usize,
        skip: Option<(&str, &str)>,
    ) -> Result<Vec<FoundExchange>, RuntimeError> {
        if words.is_empty() || limit == 0 {
            return Ok(vec![]);
        }
        let mut files = vec![];
        if let Ok(mut places) = tokio::fs::read_dir(&self.directory).await {
            while let Ok(Some(entry)) = places.next_entry().await {
                let Some(place) = entry.file_name().to_str().filter(|p| PLACE.is_match(p)).map(str::to_owned) else { continue };
                for name in self.names(&place).await {
                    let path = self.kept(&place)?.join(&name);
                    let at = modified_ms(&path).await;
                    files.push((place.clone(), name, path, at));
                }
            }
        }
        files.sort_by(|a, b| b.3.total_cmp(&a.3));
        files.truncate(MAX_SEARCHED);
        let mut found = vec![];
        for (place, name, path, _) in files {
            let id = name[..name.len() - 5].to_owned();
            if skip == Some((place.as_str(), id.as_str())) {
                continue;
            }
            // Reading and scoring hundreds of files: the app keeps drawing between them.
            tokio::task::yield_now().await;
            let Some(conversation) = Self::read(&path).await else { continue };
            let mut exchange = |said: String, answer: String, tools: Vec<String>| {
                let text = format!("{said}\n{answer}\n{}", tools.join(" ")).to_lowercase();
                let matched = words.iter().filter(|word| text.contains(word.as_str())).count();
                if matched >= needed {
                    found.push(FoundExchange {
                        place: place.clone(),
                        set: None,
                        conversation: id.clone(),
                        saved_at: conversation.saved_at,
                        said,
                        answer,
                        tools,
                        matched,
                    });
                }
            };
            // An exchange is a request and the answers to it, with the tools they used.
            let mut current: Option<(String, Vec<String>, Vec<String>)> = None;
            for line in transcript_of(&conversation.checkpoint.messages) {
                match line.role {
                    TranscriptRole::User => {
                        if let Some((said, answers, tools)) = current.take() {
                            exchange(said, answers.join("\n"), tools);
                        }
                        current = Some((line.text, vec![], vec![]));
                    }
                    TranscriptRole::Assistant => {
                        if let Some((_, answers, tools)) = current.as_mut() {
                            if !line.text.is_empty() {
                                answers.push(line.text);
                            }
                            tools.extend(line.tools.unwrap_or_default());
                        }
                    }
                }
            }
            if let Some((said, answers, tools)) = current.take() {
                exchange(said, answers.join("\n"), tools);
            }
            // What the conversation changed in Live, in plain words.
            let titles: Vec<_> =
                conversation.changes.iter().flatten().filter(|c| c.state != ChangeState::Undone).map(|c| c.title.as_str()).collect();
            if !titles.is_empty() {
                exchange(conversation.first.clone().unwrap_or_default(), format!("Changed: {}", titles.join("; ")), vec![]);
            }
        }
        found.sort_by(|a, b| b.matched.cmp(&a.matched).then(b.saved_at.cmp(&a.saved_at)));
        found.truncate(limit);
        let mut names: Vec<(String, Option<String>)> = vec![];
        for hit in &mut found {
            if let Some((_, name)) = names.iter().find(|(place, _)| *place == hit.place) {
                hit.set = name.clone();
            } else {
                let name = self.set_name(&hit.place).await;
                names.push((hit.place.clone(), name.clone()));
                hit.set = name;
            }
        }
        Ok(found)
    }
}
