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
        disk::{low_disk, MB},
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
use std::{path::Path, rc::Rc, sync::LazyLock, time::Duration};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

pub const MAKE_DEVICE_TOOL: &str = "make_device";
/// How many objects one call can ask about.
const MOST_OBJECTS: usize = 16;
static DATA: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(include_str!("tool-data.json")).expect("device tool data"));
pub type BrowserSees = Rc<dyn Fn(String, Signal) -> LocalBoxFuture<'static, Result<bool, RuntimeError>>>;
#[derive(Clone)]
pub struct DeviceToolOptions {
    pub user_library: String,
    pub browser_sees: BrowserSees,
    pub wait_ms: Option<u64>,
}
struct DeviceTool {
    options: DeviceToolOptions,
}
pub fn device_tool(options: DeviceToolOptions) -> Rc<dyn KernelTool> {
    Rc::new(DeviceTool { options })
}

/// The device's file name: its own, so making a device again with the same name replaces it. On Windows a name it takes
/// for a device ("Aux", "Con.Dist") can't be a file's, and its number goes right after that part ("Aux 2", "Con 2.Dist").
fn file_name(name: &str, windows: bool) -> String {
    let (stem, rest) = name.split_at(name.find('.').unwrap_or(name.len()));
    if !windows || !(windows_device_name(stem) || windows_device_name(name)) {
        return name.to_string();
    }
    let numbered = |number: &str| if windows_device_name(stem) { format!("{stem} {number}{rest}") } else { format!("{name} {number}") };
    (2..1000)
        .map(|index| numbered(&index.to_string()))
        .find(|candidate| !windows_device_name(candidate))
        .unwrap_or_else(|| numbered(&Uuid::new_v4().to_string()[..8]))
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
        // Max's objects, as this machine's Max describes them: no device is made.
        if let Some(objects) = input.get("objects").filter(|objects| !objects.is_null()) {
            let texts: Vec<String> = objects
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .collect();
            let mut pages = match (texts.is_empty(), reference::installed()) {
                (true, _) => {
                    "objects: give one or more of Max's objects, each as it's typed in a box (\"makenote 60 100 250\").".to_string()
                }
                (false, None) => page::NO_MAX.to_string(),
                (false, Some(reference)) => page::describe_all(reference, &texts[..texts.len().min(MOST_OBJECTS)]),
            };
            if texts.len() > MOST_OBJECTS {
                pages.push_str(&format!(
                    "\n\nOnly the first {MOST_OBJECTS} were looked up: ask for the other {} in another call.",
                    texts.len() - MOST_OBJECTS
                ));
            }
            return Ok(ToolResult::text(guide.map_or_else(|| pages.clone(), |guide| format!("{pages}\n\n{guide}"))));
        }
        if let Some(guide) = guide {
            return Ok(ToolResult::text(guide));
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
                let tests =
                    if verified.of > 0 { format!("{} of {} of its tests passed", verified.passed, verified.of) } else { "It has no tests".into() };
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
        if let Some(full) = low_disk(&self.options.user_library, 100.0 * MB, "Live's User Library is on").await {
            return Ok(ToolResult::error(format!("{full} Kumi couldn't write the device: free some space, then call make_device again.")));
        }
        let unwritable = |error: std::io::Error| {
            Ok(ToolResult::error(format!(
                "Kumi couldn't write the device to Live's User Library ({}): {error}. Make the folder writable, then call make_device again.",
                folder.to_string_lossy()
            )))
        };
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o755);
        if let Err(error) = builder.create(&folder).await {
            return unwritable(error);
        }
        let name = file_name(spec.name(), crate::system::platform() == "win32");
        let file = folder.join(format!("{name}.amxd"));
        let replaces = file.exists();
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
        let written: Result<(), std::io::Error> = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            options.mode(0o644);
            let mut out = options.open(&temporary).await?;
            out.write_all(&encode_amxd(spec.kind(), &patcher)).await?;
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
            return unwritable(error);
        }
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
        let (kind,next,extra)=match spec.kind() {
            DeviceType::MidiEffect => ("MIDI effect","load_device with this itemId on the MIDI track; Live puts a MIDI effect before the instrument.".to_string(),vec![]),
            DeviceType::AudioEffect => ("audio effect",format!("load_device with this itemId on the track, then hear it with audition while audio plays through that track (a silent render means {silent} and make it again)."),vec![&*MIX,&*OUTPUT]),
            DeviceType::Instrument => ("instrument",format!("load_device with this itemId on a MIDI track, write a short clip, and hear it with audition (a silent render means {silent} and make it again)."),vec![&*OUTPUT]),
        };
        let controls: Vec<_> = spec.controls().iter().chain(extra).map(describe_control).collect();
        let mut result = json!({"made":name,"type":kind,"itemId":item_id,"file":file.to_string_lossy(),"controls":controls});
        let fields = result.as_object_mut().unwrap();
        if replaces {
            fields.insert("replaced".into(), json!("a device by this name was in the folder: this one took its place"));
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
            fields.insert("note".into(), json!("Live's Browser hasn't listed it yet; load it in a moment."));
        }
        let again = "Calling make_device again with the same name replaces this device";
        let next = if found.wrong.is_empty() {
            format!("{next} {again}.")
        } else {
            format!(
                "It's made, and it loads as it is, but Max will show what's under wrong: fix that and call make_device again with the same name, which replaces this device. Or {next}"
            )
        };
        fields.insert("next".into(), json!(next));
        Ok(ToolResult::text(stringify(&result)))
    }
}

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn the_same_name_replaces_a_device_and_a_name_windows_takes_for_a_device_is_numbered_there() {
        assert_eq!(file_name("Bass", false), "Bass", "making it again replaces it");
        assert_eq!((file_name("Aux", false), file_name("Aux", true)), ("Aux".into(), "Aux 2".into()));
        assert_eq!(file_name("Con.Dist", true), "Con 2.Dist", "the part before the dot is what Windows reads");
        assert_eq!(file_name("LPT1 ", true), "LPT1  2");
        assert_eq!(file_name("Console", true), "Console");
    }
}
