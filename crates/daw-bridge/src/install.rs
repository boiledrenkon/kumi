//! Where each DAW's bridge goes, from what detection found.
use crate::daw::Daw;
use crate::detect::{Arch, Os, ReaperInstall, RenoiseInstall, RenoisePrefs, RENOISE_MIN};
use crate::lifecycle::Target;
use std::path::Path;

/// The Renoise tool's id: song `tool_data` is keyed by it, so it never changes.
pub const RENOISE_TOOL_ID: &str = "com.kumi.Bridge";
pub const RENOISE_TOOL_FOLDER: &str = "com.kumi.Bridge.xrnx";
/// Inside the Renoise tool's folder.
pub const RENOISE_REFERENCE: &str = "bridge-reference.json";
/// Beside REAPER's extension in UserPlugins (REAPER loads only `reaper_*` libraries, so it ignores this). In `paths`,
/// which the extension links (it finds its reference by this name).
pub use crate::paths::REAPER_REFERENCE;

/// REAPER loads `reaper_*.dylib|dll|so` from UserPlugins: one file per platform (macOS's is universal).
pub fn reaper_extension_name(os: Os) -> &'static str {
    match os {
        Os::Mac => "reaper_kumi.dylib",
        Os::Windows => "reaper_kumi.dll",
        Os::Linux => "reaper_kumi.so",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// The DAW hasn't run with this folder yet (REAPER makes its resource folder, Renoise its version folder).
    NeverOpened(String),
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetError::NeverOpened(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for TargetError {}

/// The extension goes in `<resource>/UserPlugins` (made when the resource folder exists).
pub fn reaper_target(install: &ReaperInstall, os: Os) -> Result<Target, TargetError> {
    if !install.resource.is_dir() {
        return Err(TargetError::NeverOpened(format!("open REAPER once first: {} doesn't exist yet", install.resource.display())));
    }
    let dir = install.user_plugins();
    Ok(Target { daw: Daw::Reaper, reference: dir.join(REAPER_REFERENCE), name: reaper_extension_name(os).into(), folder: false, dir })
}

/// The tool goes in the newest version folder at or above 3.5, in `Scripts/Tools`.
pub fn renoise_target(install: &RenoiseInstall) -> Result<Target, TargetError> {
    let Some(prefs) = &install.target else {
        return Err(TargetError::NeverOpened("open Renoise 3.5 or newer once first: it has no preferences folder for 3.5 yet".into()));
    };
    Ok(tool_in(prefs))
}

/// The tool's place in each version folder at or above 3.5: where a newer Renoise may have copied Kumi's tool, for the
/// lifecycle to follow and take out with it.
pub fn renoise_targets(install: &RenoiseInstall) -> Vec<Target> {
    install.prefs.iter().filter(|prefs| prefs.version >= RENOISE_MIN).map(tool_in).collect()
}

fn tool_in(prefs: &RenoisePrefs) -> Target {
    let dir = prefs.tools();
    Target {
        daw: Daw::Renoise,
        reference: dir.join(RENOISE_TOOL_FOLDER).join(RENOISE_REFERENCE),
        name: RENOISE_TOOL_FOLDER.into(),
        folder: true,
        dir,
    }
}

/// Whether an extension can load into an app: they share an architecture (REAPER under Rosetta runs x86_64, so a
/// universal extension covers both). Unknown on either side counts as fitting.
pub fn arch_fits(extension: &Path, app_archs: &[Arch]) -> bool {
    let ours = crate::detect::binary_archs(extension);
    ours.is_empty() || app_archs.is_empty() || ours.iter().any(|arch| app_archs.contains(arch))
}

/// Why the REAPER Kumi would put its extension in can't load it (a 32-bit REAPER on Windows can't load the x64 DLL),
/// in words; None when it can, or either side's architecture isn't known. Install refuses then, rather than say the
/// extension is in a REAPER that will never load it.
pub fn unloadable(extension: &Path, install: &ReaperInstall) -> Option<String> {
    if arch_fits(extension, &install.archs) {
        return None;
    }
    let names = |archs: &[Arch]| {
        let words: Vec<&str> = archs
            .iter()
            .map(|arch| match arch {
                Arch::X86_64 => "x64",
                Arch::Arm64 => "ARM64",
                Arch::Arm64ec => "ARM64EC",
                Arch::X86 => "32-bit x86",
                Arch::Arm => "32-bit ARM",
                Arch::Other => "another architecture",
            })
            .collect();
        words.join(" and ")
    };
    let app = install.app.as_ref().map(|app| format!(" ({})", app.display())).unwrap_or_default();
    Some(format!(
        "this REAPER{app} is {}, and Kumi's extension is {}, which it can't load",
        names(&install.archs),
        names(&crate::detect::binary_archs(extension))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{RenoisePrefs, ResourceFrom, Version};

    #[test]
    fn a_reaper_that_cant_load_the_extension_is_said_before_it_goes_in() {
        let root = tempfile::tempdir().unwrap();
        // A PE header for x64, as the Windows extension has.
        let mut pe = vec![0u8; 0x200];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        let extension = root.path().join("reaper_kumi.dll");
        std::fs::write(&extension, &pe).unwrap();
        let reaper = |archs: Vec<Arch>| ReaperInstall {
            app: Some(root.path().join("REAPER").join("reaper.exe")),
            version: None,
            archs,
            resource: root.path().join("REAPER"),
            resource_from: ResourceFrom::Portable,
            opened: true,
        };
        let said = unloadable(&extension, &reaper(vec![Arch::X86])).expect("a 32-bit REAPER can't load it");
        assert!(said.contains("is 32-bit x86, and Kumi's extension is x64, which it can't load"), "{said}");
        assert!(said.contains("reaper.exe"), "{said}");
        assert_eq!(unloadable(&extension, &reaper(vec![Arch::X86_64])), None);
        assert_eq!(unloadable(&extension, &reaper(vec![])), None, "an app whose architecture isn't known");
    }

    #[test]
    fn targets_need_the_folder_the_daw_makes() {
        let root = tempfile::tempdir().unwrap();
        let mut reaper = ReaperInstall {
            app: None,
            version: None,
            archs: vec![],
            resource: root.path().join("REAPER"),
            resource_from: ResourceFrom::Default,
            opened: false,
        };
        assert!(matches!(reaper_target(&reaper, Os::Mac), Err(TargetError::NeverOpened(_))));
        std::fs::create_dir(&reaper.resource).unwrap();
        reaper.opened = true;
        let target = reaper_target(&reaper, Os::Linux).unwrap();
        assert_eq!(target.path(), root.path().join("REAPER/UserPlugins/reaper_kumi.so"));
        assert_eq!(target.reference, root.path().join("REAPER/UserPlugins/kumi-bridge-reference.json"));

        let mut renoise = RenoiseInstall { apps: vec![], prefs: vec![], target: None, overridden: false };
        assert!(renoise_target(&renoise).is_err());
        renoise.target = Some(RenoisePrefs { version: Version(3, 5, 4), dir: root.path().join("V3.5.4") });
        let target = renoise_target(&renoise).unwrap();
        assert_eq!(target.path(), root.path().join("V3.5.4/Scripts/Tools/com.kumi.Bridge.xrnx"));
        assert!(target.folder);
    }
}
