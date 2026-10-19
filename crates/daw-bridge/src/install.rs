//! Where each DAW's bridge goes, from what detection found (Logic's goes in Kumi's own folder: Logic loads nothing).
use crate::daw::Daw;
use crate::detect::{Arch, Os, ReaperInstall, RenoiseInstall};
use crate::lifecycle::Target;
use crate::paths::BridgePaths;
use std::path::Path;

/// The Renoise tool's id: song `tool_data` is keyed by it, so it never changes.
pub const RENOISE_TOOL_ID: &str = "com.kumi.Bridge";
pub const RENOISE_TOOL_FOLDER: &str = "com.kumi.Bridge.xrnx";
/// Inside the Renoise tool's folder.
pub const RENOISE_REFERENCE: &str = "bridge-reference.json";
/// Beside REAPER's extension in UserPlugins (REAPER loads only `reaper_*` libraries, so it ignores this).
pub const REAPER_REFERENCE: &str = "kumi-bridge-reference.json";
/// Logic's bridge folder, in `<Kumi's folder>/bridges/logic/bridge/`: the binary, its helper(s), a version file, and
/// the reference inside it.
pub const LOGIC_BRIDGE_FOLDER: &str = "logic-kumi";
/// Inside Logic's bridge folder (left out of the folder's hash, as Renoise's is).
pub const LOGIC_REFERENCE: &str = "bridge-reference.json";
/// The bridge's binary, inside the folder: what the host starts, with `--kumi-home`.
pub const LOGIC_BINARY: &str = "logic-kumi";
/// The Accessibility helper(s) beside it, `kumi-logic-hands-<what follows>` (one per build the bridge may run).
pub const LOGIC_HELPER_PREFIX: &str = "kumi-logic-hands-";
/// The bridge's version, one line, inside the folder: part of what the lifecycle hashes.
pub const LOGIC_VERSION_FILE: &str = "version";

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
    let dir = prefs.tools();
    Ok(Target {
        daw: Daw::Renoise,
        reference: dir.join(RENOISE_TOOL_FOLDER).join(RENOISE_REFERENCE),
        name: RENOISE_TOOL_FOLDER.into(),
        folder: true,
        dir,
    })
}

/// Logic's bridge goes in Kumi's own folder for Logic (`paths` are Logic's): a `logic-kumi` folder in `bridge/`, with
/// the reference inside it. Nothing waits on Logic having run, so this never fails.
pub fn logic_target(paths: &BridgePaths) -> Target {
    let dir = paths.root.join("bridge");
    Target {
        daw: Daw::Logic,
        reference: dir.join(LOGIC_BRIDGE_FOLDER).join(LOGIC_REFERENCE),
        name: LOGIC_BRIDGE_FOLDER.into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{RenoisePrefs, ResourceFrom, Version};

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

    /// Logic's bridge is a folder of Kumi's own, installed through the lifecycle like Renoise's tool: the reference
    /// inside it is left out of its hash, the version file is part of it, and the binary stays executable.
    #[test]
    fn logic_goes_in_kumis_own_folder_and_its_binary_stays_executable() {
        use crate::lifecycle::Lifecycle;
        let root = tempfile::tempdir().unwrap();
        let kumi = root.path().join("kumi");
        let paths = BridgePaths::new(&kumi, Daw::Logic);
        let target = logic_target(&paths);
        assert_eq!(target.dir, kumi.join("bridges/logic/bridge"));
        assert_eq!(target.path(), kumi.join("bridges/logic/bridge/logic-kumi"));
        assert_eq!(target.reference, kumi.join("bridges/logic/bridge/logic-kumi/bridge-reference.json"));
        assert!(target.folder && target.daw == Daw::Logic);

        let source = root.path().join("built").join(LOGIC_BRIDGE_FOLDER);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join(LOGIC_BINARY), b"#!/bin/sh\necho bridge\n").unwrap();
        std::fs::write(source.join(format!("{LOGIC_HELPER_PREFIX}arm64")), b"#!/bin/sh\necho hands\n").unwrap();
        std::fs::write(source.join(LOGIC_VERSION_FILE), "0.1.0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for name in [LOGIC_BINARY.to_string(), format!("{LOGIC_HELPER_PREFIX}arm64")] {
                std::fs::set_permissions(source.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let life = Lifecycle::new(&kumi, target.clone());
        let receipt = life.install(&source, "0.1.0").unwrap();
        assert_eq!(receipt.target, target.path().to_string_lossy());
        let status = life.status().unwrap();
        assert!(status.present && status.matches && status.reference && status.secret, "{status:?}");
        assert_eq!(std::fs::read_to_string(target.path().join(LOGIC_VERSION_FILE)).unwrap(), "0.1.0\n");
        let reference = crate::files::Reference::read(&target.reference).unwrap();
        assert_eq!((reference.daw.as_str(), reference.port), ("logic", Some(9790)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(target.path().join(LOGIC_BINARY)).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "the host runs it: {mode:o}");
        }
        // The version file is the bridge's: a changed one is a changed bridge. The reference isn't.
        std::fs::write(target.reference.clone(), reference.to_json()).unwrap();
        assert!(life.status().unwrap().matches);
        std::fs::write(target.path().join(LOGIC_VERSION_FILE), "0.2.0\n").unwrap();
        assert!(!life.status().unwrap().matches);
    }
}
