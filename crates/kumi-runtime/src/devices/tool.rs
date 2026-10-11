//! Build a checked Max for Live device in Live's User Library, then wait for its Browser.

use super::{
    amxd::{encode_amxd, DeviceType},
    gen::{audio_effect_patcher, instrument_patcher, GenSpec, MIX, OUTPUT},
    harness::{check_midi_device_isolated, IsolatedOptions},
    midi::midi_device_patcher,
    patch::{page, reference},
    patched::patched_device,
    spec::{check_spec, Control, DeviceSpec},
};
use crate::{
    core::{
        contracts::{JsonObject, KernelTool, ToolResult},
        errors::RuntimeError,
    },
    system::windows_device_name,
};
use async_trait::async_trait;
use futures::future::LocalBoxFuture;
use kumi_common::{
    abort::{Signal, SignalExt},
    js::{json::stringify, number::to_string},
    time::now_ms,
};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    rc::Rc,
    sync::LazyLock,
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

pub const MAKE_DEVICE_TOOL: &str = "make_device";
/// How many objects one call can ask about.
const MOST_OBJECTS: usize = 16;
/// How many replaced devices' files are kept, the newest.
const KEPT: usize = 50;
static DATA: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(include_str!("tool-data.json")).expect("device tool data"));
pub type BrowserSees = Rc<dyn Fn(String, Signal) -> LocalBoxFuture<'static, Result<bool, RuntimeError>>>;
#[derive(Clone)]
pub struct DeviceToolOptions {
    pub user_library: String,
    pub browser_sees: BrowserSees,
    pub wait_ms: Option<u64>,
    /// The devices this conversation made: the tool is made anew each turn, and these are kept for the conversation.
    pub made: Rc<RefCell<Made>>,
}
struct DeviceTool {
    options: DeviceToolOptions,
}
pub fn device_tool(options: DeviceToolOptions) -> Rc<dyn KernelTool> {
    Rc::new(DeviceTool { options })
}

/// The devices made in this conversation, each by the name it was asked for and the file it was made as. Making one
/// again by either name replaces it while its file is as Kumi wrote it. Any other device in the folder keeps its file:
/// Live keeps a Max for Live device in a Set as its file, so a saved Set may use it.
#[derive(Debug, Default)]
pub struct Made {
    conversation: String,
    devices: Vec<MadeDevice>,
    /// Where a replaced device's file is kept, if anywhere.
    kept: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct MadeDevice {
    asked: String,
    file: String,
    kind: DeviceType,
    written: u64,
}

impl Made {
    /// Made devices whose files are kept in `folder` when they're replaced.
    pub fn keeping(folder: PathBuf) -> Made {
        Made { kept: Some(folder), ..Made::default() }
    }

    /// The conversation from here on: the devices another made aren't this one's to replace.
    pub fn conversation(&mut self, id: &str) {
        if self.conversation != id {
            self.conversation = id.to_string();
            self.devices.clear();
        }
    }

    /// The device this conversation made by the name asked for (or made as that name), while its file is as Kumi wrote it.
    /// No name asked for is no device's.
    fn ours(&self, folder: &Path, asked: &str) -> Option<MadeDevice> {
        if asked.is_empty() {
            return None;
        }
        let unchanged = |device: &&MadeDevice| {
            std::fs::read(folder.join(format!("{}.amxd", device.file))).is_ok_and(|bytes| hashed(&bytes) == device.written)
        };
        let by_name = self.devices.iter().find(|device| device.asked == asked).filter(unchanged);
        by_name.or_else(|| self.devices.iter().find(|device| device.file == asked).filter(unchanged)).cloned()
    }

    fn made(&mut self, device: MadeDevice) {
        self.devices.retain(|kept| kept.file != device.file && kept.asked != device.asked);
        self.devices.push(device);
    }
}

fn hashed(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// The device's file name: its own, numbered past a file the folder has ("Echo 2"). On Windows a name it takes for a
/// device ("Aux", "Con.Dist") is taken as a file's would be, and its number goes right after that part ("Aux 2",
/// "Con 2.Dist").
fn free_name(folder: &Path, name: &str, windows: bool) -> String {
    let (stem, rest) = name.split_at(name.find('.').unwrap_or(name.len()));
    let numbered = |number: &str| {
        if windows && windows_device_name(stem) {
            format!("{stem} {number}{rest}")
        } else {
            format!("{name} {number}")
        }
    };
    for index in 1..1000 {
        let candidate = if index == 1 { name.into() } else { numbered(&index.to_string()) };
        let taken = (windows && windows_device_name(&candidate)) || folder.join(format!("{candidate}.amxd")).exists();
        if !taken {
            return candidate;
        }
    }
    numbered(&Uuid::new_v4().to_string()[..8])
}

/// A copy of a device's file before it's replaced, in `kept` (the newest [`KEPT`] stay): where it is, None when it
/// couldn't be copied.
async fn keep(file: &Path, name: &str, kept: &Path) -> Option<PathBuf> {
    tokio::fs::create_dir_all(kept).await.ok()?;
    let copy = kept.join(format!("{name} {}.amxd", now_ms()));
    tokio::fs::copy(file, &copy).await.ok()?;
    let mut copies: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(kept)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .filter(|(_, path)| path.extension().is_some_and(|extension| extension == "amxd"))
        .collect();
    copies.sort();
    for (_, old) in copies.iter().rev().skip(KEPT) {
        let _ = tokio::fs::remove_file(old).await;
    }
    Some(copy)
}

fn kind_name(kind: DeviceType) -> &'static str {
    match kind {
        DeviceType::MidiEffect => "MIDI effect",
        DeviceType::AudioEffect => "audio effect",
        DeviceType::Instrument => "instrument",
    }
}

fn describe_control(control: &Control) -> String {
    match control {
        Control::Choice(c) => format!("{} ({}; {})", c.name, c.options.join(" / "), c.default),
        Control::Switch(c) => format!("{} (on/off; {})", c.name, if c.default { "on" } else { "off" }),
        Control::Number(c) | Control::Integer(c) => format!(
            "{} ({}–{}{}; {})",
            c.name,
            to_string(c.min),
            to_string(c.max),
            if c.unit.as_str().is_empty() { String::new() } else { format!(" {}", c.unit) },
            to_string(c.default)
        ),
    }
}

/// What this machine's Max says of the objects asked about, each as it's typed in a box.
fn pages(texts: &[String]) -> String {
    let mut pages = match (texts.is_empty(), reference::installed()) {
        (true, _) => "objects: give one or more of Max's objects, each as it's typed in a box (\"makenote 60 100 250\").".to_string(),
        (false, None) => page::NO_MAX.to_string(),
        (false, Some(reference)) => page::describe_all(reference, &texts[..texts.len().min(MOST_OBJECTS)]),
    };
    if texts.len() > MOST_OBJECTS {
        pages.push_str(&format!(
            "\n\nOnly the first {MOST_OBJECTS} were looked up: ask for the other {} in another call.",
            texts.len() - MOST_OBJECTS
        ));
    }
    pages
}

/// Why a device's file couldn't be written.
fn unwritable(folder: &Path, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::StorageFull {
        format!(
            "Kumi couldn't write the device: the disk Live's User Library is on ({}) is full. Free some space, then call make_device again.",
            folder.to_string_lossy()
        )
    } else {
        format!(
            "Kumi couldn't write the device to Live's User Library ({}): {error}. Make the folder writable, then call make_device again.",
            folder.to_string_lossy()
        )
    }
}

fn io_error(error: std::io::Error) -> RuntimeError {
    RuntimeError::plain(error.to_string())
}
#[async_trait(?Send)]
impl KernelTool for DeviceTool {
    fn name(&self) -> &str {
        MAKE_DEVICE_TOOL
    }
    fn description(&self) -> &str {
        DATA["description"].as_str().unwrap()
    }
    fn input_schema(&self) -> JsonObject {
        DATA["schema"].as_object().unwrap().clone()
    }
    async fn execute(&self, input: JsonObject, signal: Signal) -> Result<ToolResult, RuntimeError> {
        let guide = (input.get("guide").and_then(Value::as_bool) == Some(true)).then(|| {
            let kind = input.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "midi_effect" | "audio_effect" | "instrument") {
                DATA[kind].as_str().unwrap().to_string()
            } else {
                ["midi_effect", "audio_effect", "instrument"].map(|k| DATA[k].as_str().unwrap()).join("\n\n")
            }
        });
        // What's given, as a model leaves a field it doesn't use: null, false, "" or [] is nothing.
        let given = |key: &str| match input.get(key) {
            None | Some(Value::Null | Value::Bool(false)) => false,
            Some(Value::String(text)) => !text.trim().is_empty(),
            Some(Value::Array(items)) => !items.is_empty(),
            Some(_) => true,
        };
        let objects = input.get("objects").filter(|objects| !matches!(objects, Value::Null | Value::Bool(false)));
        let asked: Vec<String> = match objects {
            Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
            Some(Value::String(text)) => vec![text.as_str()],
            _ => Vec::new(),
        }
        .into_iter()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .collect();
        // Max's objects as this machine's Max describes them, or a guide, with no device given to make beside them.
        if !given("code") && !given("patch") && (objects.is_some() || guide.is_some()) {
            let mut said: Vec<String> = Vec::new();
            if objects.is_some() {
                said.push(pages(&asked));
            }
            said.extend(guide);
            if ["name", "controls", "about", "tests"].iter().any(|key| given(key)) {
                said.push("To make the device as well, give its code or its patch beside these.".to_string());
            }
            return Ok(ToolResult::text(said.join("\n\n")));
        }
        // The device is always made: what doesn't fit is made the nearest way it can be, and said beside it.
        let (spec, mut found) = check_spec(&input);
        let checked = match &spec {
            DeviceSpec::Patch(_) if reference::installed().is_some() => {
                "Kumi checked every object against this machine's Max, every cord against the ports it meets, and the order its messages go in, and laid it out top to bottom".to_string()
            }
            DeviceSpec::Patch(_) => "Kumi checked the patch (Max wasn't found here, so its objects weren't checked against it) and laid it out top to bottom".to_string(),
            DeviceSpec::MidiEffect(midi) => {
                let verified = check_midi_device_isolated(midi, IsolatedOptions::default()).await;
                found.wrong.extend(verified.problems);
                let tests = if verified.of > 0 {
                    format!("{} of {} of its tests passed", verified.passed, verified.of)
                } else if given("tests") {
                    "None of its tests ran (changed says why)".into()
                } else {
                    "It has no tests".into()
                };
                // What the spec's checks found counts too: the summary never says nothing's wrong beside a list that does.
                if found.wrong.is_empty() {
                    let ran = if midi.runs_free { "no errors; it runs free" } else { "no errors, no hanging notes" };
                    format!("{tests}, and Kumi's checks found nothing wrong ({ran})")
                } else {
                    format!("{tests}; what Kumi found is under wrong")
                }
            }
            _ => "Kumi checked its outputs, inputs and controls; Max compiles the code when Live loads it".into(),
        };
        signal.check()?;
        let folder = Path::new(&self.options.user_library).join("Kumi");
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o755);
        if let Err(error) = builder.create(&folder).await {
            return Ok(ToolResult::error(unwritable(&folder, &error)));
        }
        // A device this conversation made is replaced; any other keeps its file, and this one is numbered past it.
        let asked_name = input.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let ours = self.options.made.borrow().ours(&folder, &asked_name);
        let windows = crate::system::platform() == "win32";
        let name = match &ours {
            Some(device) => device.file.clone(),
            None => free_name(&folder, spec.name(), windows),
        };
        if ours.is_none() && name != spec.name() {
            let stem = spec.name().split('.').next().unwrap_or_default();
            let why = if windows && (windows_device_name(stem) || windows_device_name(spec.name())) {
                format!("Windows keeps the name {} for a device", spec.name())
            } else {
                format!("{} is another device in Live's User Library, and a saved Set may use it", spec.name())
            };
            found.changed.push(format!("name: made as \"{name}\": {why}."));
        }
        let file = folder.join(format!("{name}.amxd"));
        let temporary = folder.join(format!(".{}.amxd", Uuid::new_v4()));
        let patcher = match &spec {
            DeviceSpec::MidiEffect(s) => {
                let mut s = s.clone();
                s.name = name.clone();
                midi_device_patcher(&s)
            }
            DeviceSpec::AudioEffect(s) => {
                let mut s = GenSpec::from(s);
                s.name = name.clone();
                audio_effect_patcher(&s)
            }
            DeviceSpec::Instrument(s) => {
                let mut s = GenSpec::from(s);
                s.name = name.clone();
                instrument_patcher(&s)
            }
            DeviceSpec::Patch(s) => {
                let mut s = s.clone();
                s.name = name.clone();
                let (patcher, patched) = patched_device(&s, reference::installed());
                found.append(patched);
                patcher
            }
        };
        let bytes = encode_amxd(spec.kind(), &patcher);
        let kept_in = self.options.made.borrow().kept.clone();
        let kept = match (&ours, kept_in) {
            (Some(_), Some(kept_in)) => keep(&file, &name, &kept_in).await,
            _ => None,
        };
        let written: Result<(), std::io::Error> = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            options.mode(0o644);
            let mut out = options.open(&temporary).await?;
            out.write_all(&bytes).await?;
            out.flush().await?;
            drop(out);
            tokio::fs::rename(&temporary, &file).await
        }
        .await;
        let removed = tokio::fs::remove_file(&temporary).await;
        if let Err(error) = removed {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(io_error(error));
            }
        }
        if let Err(error) = written {
            return Ok(ToolResult::error(unwritable(&folder, &error)));
        }
        self.options.made.borrow_mut().made(MadeDevice {
            asked: asked_name,
            file: name.clone(),
            kind: spec.kind(),
            written: hashed(&bytes),
        });
        let item_id = format!("user_library/Kumi/{name}");
        let deadline = now_ms().saturating_add(self.options.wait_ms.unwrap_or(20_000) as i64);
        let mut seen = false;
        while !seen && now_ms() < deadline {
            seen = (self.options.browser_sees)(item_id.clone(), signal.clone()).await.unwrap_or(false);
            if !seen {
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            signal.check()?;
        }
        // A silent render means the code didn't compile, or a patch passes no sound.
        let silent =
            if matches!(spec, DeviceSpec::Patch(_)) { "the patch passes no sound: fix it" } else { "the code didn't compile: fix it" };
        let (next,extra)=match spec.kind() {
            DeviceType::MidiEffect => ("load_device with this itemId on the MIDI track; Live puts a MIDI effect before the instrument.".to_string(),vec![]),
            DeviceType::AudioEffect => (format!("load_device with this itemId on the track, then hear it with audition while audio plays through that track (a silent render means {silent} and make it again)."),vec![&*MIX,&*OUTPUT]),
            DeviceType::Instrument => (format!("load_device with this itemId on a MIDI track, write a short clip, and hear it with audition (a silent render means {silent} and make it again)."),vec![&*OUTPUT]),
        };
        let controls: Vec<_> = spec.controls().iter().chain(extra).map(describe_control).collect();
        let mut result =
            json!({"made":name,"type":kind_name(spec.kind()),"itemId":item_id,"file":file.to_string_lossy(),"controls":controls});
        let fields = result.as_object_mut().unwrap();
        if let Some(device) = &ours {
            let kept = kept.map_or(String::new(), |kept| format!("; the old one is kept at {}", kept.to_string_lossy()));
            fields.insert(
                "replaced".into(),
                json!(format!("the {} this conversation made as {}{kept}", kind_name(device.kind), device.file)),
            );
        }
        if let DeviceSpec::Instrument(s) = &spec {
            fields.insert("voices".into(), json!(s.voices));
        }
        fields.insert("checks".into(), json!(checked));
        for (key, said) in [("wrong", &found.wrong), ("changed", &found.changed), ("notes", &found.notes)] {
            if !said.is_empty() {
                fields.insert(key.into(), json!(said));
            }
        }
        if !seen {
            fields.insert("browser".into(), json!("Live's Browser hasn't listed it yet; load it in a moment."));
        }
        let again = format!("call make_device again in this conversation with the name \"{name}\", which replaces this device");
        let next = if found.wrong.is_empty() {
            format!("{next} To change it, {again}.")
        } else {
            format!("It's made, and it loads as it is, but what's under wrong needs fixing: fix it and {again}. Or {next}")
        };
        fields.insert("next".into(), json!(next));
        // What was asked beside the device: Max's objects, and the guide.
        if !asked.is_empty() {
            fields.insert("objects".into(), json!(pages(&asked)));
        }
        if let Some(guide) = guide {
            fields.insert("guide".into(), json!(guide));
        }
        Ok(ToolResult::text(stringify(&result)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_another_device_has_is_numbered_and_a_name_windows_takes_for_a_device_too() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(free_name(folder.path(), "Bass", false), "Bass");
        std::fs::write(folder.path().join("Bass.amxd"), "").unwrap();
        assert_eq!(free_name(folder.path(), "Bass", false), "Bass 2", "a file's name is taken everywhere");
        assert_eq!((free_name(folder.path(), "Aux", false), free_name(folder.path(), "Aux", true)), ("Aux".into(), "Aux 2".into()));
        assert_eq!(free_name(folder.path(), "Con.Dist", true), "Con 2.Dist", "the part before the dot is what Windows reads");
        assert_eq!(free_name(folder.path(), "LPT1 ", true), "LPT1  2");
        assert_eq!(free_name(folder.path(), "Console", true), "Console");
    }

    #[test]
    fn only_a_device_this_conversation_made_and_kumi_wrote_is_ours_to_replace() {
        let folder = tempfile::tempdir().unwrap();
        let mut made = Made::default();
        made.conversation("first");
        std::fs::write(folder.path().join("Echo 2.amxd"), b"written").unwrap();
        made.made(MadeDevice { asked: "Echo".into(), file: "Echo 2".into(), kind: DeviceType::MidiEffect, written: hashed(b"written") });
        assert_eq!(made.ours(folder.path(), "Echo").map(|device| device.file), Some("Echo 2".into()), "by the name asked for");
        assert_eq!(made.ours(folder.path(), "Echo 2").map(|device| device.file), Some("Echo 2".into()), "by the name it was made as");
        assert!(made.ours(folder.path(), "Delay").is_none());
        made.made(MadeDevice { asked: "".into(), file: "Echo 3".into(), kind: DeviceType::MidiEffect, written: hashed(b"written") });
        std::fs::write(folder.path().join("Echo 3.amxd"), b"written").unwrap();
        assert!(made.ours(folder.path(), "").is_none(), "two devices made without a name are two");
        std::fs::write(folder.path().join("Echo 2.amxd"), b"saved in Max").unwrap();
        assert!(made.ours(folder.path(), "Echo").is_none(), "a file changed since is someone's own");
        std::fs::write(folder.path().join("Echo 2.amxd"), b"written").unwrap();
        made.conversation("second");
        assert!(made.ours(folder.path(), "Echo").is_none(), "another conversation's");
    }

    #[test]
    fn a_full_disk_and_an_unwritable_folder_say_what_to_do() {
        let folder = Path::new("/Music/User Library/Kumi");
        let full = unwritable(folder, &std::io::Error::from(std::io::ErrorKind::StorageFull));
        assert!(full.contains("is full. Free some space, then call make_device again."), "{full}");
        let denied = unwritable(folder, &std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(denied.contains("Make the folder writable, then call make_device again."), "{denied}");
    }
}
