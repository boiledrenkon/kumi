//! Renoise preset files Kumi reads for the tool (M4 n-devices): `device-preset-data` with a `path` instead of `presetData`
//! gets the .xrdp file's text here, from Renoise's own library or the producer's User Library only, and the tool writes
//! it as it writes preset data given in full (the same device type, the same document version or older, the same plug-in).

use crate::presets::{Refusal, MAX_PRESET_BYTES};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The kind that takes a preset file.
pub const KIND: &str = "device-preset-data";

/// Renoise's library folders a preset file may come from: the producer's User Library, and the library Renoise copies out of
/// the app for each of its versions (in the version's preferences folder, or on macOS its Application Support folder).
pub fn library_roots() -> Vec<PathBuf> {
    let Some(home) =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|home| !home.is_empty()).map(PathBuf::from)
    else {
        return vec![];
    };
    let var = |name: &str| std::env::var(name).ok();
    let env = daw_bridge::detect::Env { os: daw_bridge::detect::Os::current(), home: home.clone(), var: &var, root: None };
    let mut roots = vec![home.join("Documents").join("Renoise").join("User Library")];
    for prefs in daw_bridge::detect::detect_renoise(&env).prefs {
        roots.push(prefs.dir.join("Library"));
        if let Some(version) = prefs.dir.file_name() {
            roots.push(home.join("Library").join("Application Support").join("Renoise").join(version).join("Library"));
        }
    }
    roots
}

/// An .xrdp file's text and its preset's name (the file's name without its extension): the path resolved (every link
/// followed) inside one of `roots` (resolved too), an .xrdp file, at most the bound, a device preset document.
pub fn read_preset(path: &str, roots: &[PathBuf]) -> Result<(String, String), Refusal> {
    let asked = Path::new(path);
    if !asked.is_absolute() {
        return Err(Refusal::Bad("path: the whole path of an .xrdp preset file in Renoise's library".into()));
    }
    if !asked.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("xrdp")) {
        return Err(Refusal::Bad(format!("{path} isn't a device preset (.xrdp) file")));
    }
    let real = std::fs::canonicalize(asked).map_err(|_| Refusal::Bad(format!("there's no preset file at {path}")))?;
    let inside = roots.iter().filter_map(|root| std::fs::canonicalize(root).ok()).any(|root| real.starts_with(&root));
    if !inside {
        return Err(Refusal::Bad(format!(
            "{path} isn't in Renoise's library or the User Library (Kumi reads preset files only from there)"
        )));
    }
    let size = std::fs::metadata(&real).map(|meta| meta.len()).unwrap_or(u64::MAX);
    if !real.is_file() || size > MAX_PRESET_BYTES as u64 {
        return Err(Refusal::Bad(format!("{path} isn't a preset file of at most {} MB", MAX_PRESET_BYTES / 1_048_576)));
    }
    let text = std::fs::read_to_string(&real).map_err(|_| Refusal::Bad(format!("{path} isn't a text preset file Kumi can read")))?;
    if !text.contains("<FilterDevicePreset") || !text.contains("<DeviceSlot") {
        return Err(Refusal::Bad(format!("{path} isn't a device preset (no <FilterDevicePreset> with a <DeviceSlot>)")));
    }
    let name = real.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    Ok((text, name))
}

/// The input the tool gets for a `device-preset-data` given a `path`: its preset data read from the file, and its name
/// (`presetName`, for the title). None when the change isn't one.
pub fn with_file(daw: daw_bridge::Daw, kind: &str, input: &Value, roots: &[PathBuf]) -> Option<Result<Value, Refusal>> {
    if daw != daw_bridge::Daw::Renoise || kind != KIND {
        return None;
    }
    let path = input.get("path")?;
    Some((|| {
        let Some(path) = path.as_str() else { return Err(Refusal::Bad("path: the whole path of an .xrdp preset file".into())) };
        if input.get("presetData").is_some() {
            return Err(Refusal::Bad("give presetData or path, not both".into()));
        }
        let (text, name) = read_preset(path, roots)?;
        let mut input = input.clone();
        if let Some(fields) = input.as_object_mut() {
            fields.remove("path");
            fields.insert("presetData".into(), text.into());
            fields.insert("presetName".into(), name.into());
        }
        Ok(input)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PRESET: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<FilterDevicePreset doc_version=\"14\">\n  <DeviceSlot type=\"CompressorDevice\">\n    <IsMaximized>true</IsMaximized>\n  </DeviceSlot>\n</FilterDevicePreset>\n";

    fn library() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("User Library").join("Effect Presets").join("Compressor");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(library.join("Drums Hard.xrdp"), PRESET).unwrap();
        (dir, library)
    }

    #[test]
    fn a_preset_file_in_the_library_becomes_the_preset_data_the_tool_writes() {
        let (dir, library) = library();
        let roots = vec![dir.path().join("User Library")];
        let path = library.join("Drums Hard.xrdp").to_string_lossy().into_owned();
        let input = json!({"ref": "e1:device:1.2", "path": path});
        let made = with_file(daw_bridge::Daw::Renoise, KIND, &input, &roots).unwrap().unwrap();
        assert_eq!(made["presetData"], PRESET);
        assert_eq!(made["presetName"], "Drums Hard");
        assert!(made.get("path").is_none());
        // Not this kind, not Renoise, no path: nothing to read.
        assert!(with_file(daw_bridge::Daw::Renoise, "device-preset", &input, &roots).is_none());
        assert!(with_file(daw_bridge::Daw::Reaper, KIND, &input, &roots).is_none());
        assert!(with_file(daw_bridge::Daw::Renoise, KIND, &json!({"ref": "e1:device:1.2", "presetData": PRESET}), &roots).is_none());
    }

    #[test]
    fn a_preset_file_is_refused_outside_the_library_through_a_link_too_or_when_it_is_no_preset() {
        let (dir, library) = library();
        let roots = vec![dir.path().join("User Library")];
        let refused = |path: &Path| match read_preset(&path.to_string_lossy(), &roots) {
            Err(Refusal::Bad(message)) => message,
            other => panic!("{path:?} not refused: {other:?}"),
        };
        // Outside the library.
        let outside = dir.path().join("Elsewhere.xrdp");
        std::fs::write(&outside, PRESET).unwrap();
        assert!(refused(&outside).contains("isn't in Renoise's library"));
        // A link in the library to a file outside it.
        #[cfg(unix)]
        {
            let link = library.join("Escape.xrdp");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(refused(&link).contains("isn't in Renoise's library"));
            // A folder link out of the library, with the file under it.
            let away = dir.path().join("away");
            std::fs::create_dir_all(&away).unwrap();
            std::fs::write(away.join("Far.xrdp"), PRESET).unwrap();
            std::os::unix::fs::symlink(&away, library.join("Away")).unwrap();
            assert!(refused(&library.join("Away").join("Far.xrdp")).contains("isn't in Renoise's library"));
        }
        // Another kind of file, a relative path, no file, not a device preset, too big.
        std::fs::write(library.join("Notes.txt"), "hello").unwrap();
        assert!(refused(&library.join("Notes.txt")).contains(".xrdp"));
        assert!(
            matches!(read_preset("Effect Presets/Compressor/Drums Hard.xrdp", &roots), Err(Refusal::Bad(m)) if m.contains("whole path"))
        );
        assert!(refused(&library.join("Missing.xrdp")).contains("no preset file"));
        std::fs::write(library.join("Chain.xrdp"), "<RenoiseDeviceChain doc_version=\"21\"/>").unwrap();
        assert!(refused(&library.join("Chain.xrdp")).contains("isn't a device preset"));
        let big = library.join("Big.xrdp");
        std::fs::write(&big, format!("{PRESET}{}", " ".repeat(MAX_PRESET_BYTES))).unwrap();
        assert!(refused(&big).contains("at most"));
        // Both a path and preset data.
        let both = json!({"ref": "e1:device:1.2", "path": library.join("Drums Hard.xrdp").to_string_lossy(), "presetData": PRESET});
        assert!(matches!(with_file(daw_bridge::Daw::Renoise, KIND, &both, &roots), Some(Err(Refusal::Bad(m))) if m.contains("not both")));
    }

    #[test]
    fn the_library_roots_are_the_user_library_and_renoises_own() {
        let roots = library_roots();
        assert!(roots.iter().any(|root| root.ends_with(Path::new("Documents/Renoise/User Library"))), "{roots:?}");
    }
}
