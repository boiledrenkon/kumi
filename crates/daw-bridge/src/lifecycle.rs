//! Installing a DAW bridge's files: install, upgrade, rollback and uninstall, each recorded in a receipt, with every
//! installed version kept as a generation in Kumi's folder to go back to. The DAW's copy is replaced by renaming a
//! finished copy into place, never written in place: a running REAPER keeps the old file mapped (Windows locks it).
use crate::auth::sha256_hex;
use crate::daw::Daw;
use crate::files::{ensure_secret, FileError, Reference};
use crate::owner;
use crate::paths::BridgePaths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const RECEIPT_SCHEMA: &str = "kumi-daw-receipt/1";
/// Earlier generations kept for rollback.
pub const KEPT_GENERATIONS: usize = 3;
const HISTORY: usize = 20;
/// Files inside a bridge folder that aren't the bridge's own: written beside it by the installer or by the DAW.
pub const NOT_PAYLOAD: [&str; 2] = ["bridge-reference.json", "preferences.xml"];

/// Where a DAW's bridge goes: one file (REAPER's extension) or one folder (Renoise's tool) in `dir`, and the
/// reference file beside or inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub daw: Daw,
    /// REAPER's `UserPlugins`, Renoise's `Scripts/Tools`.
    pub dir: PathBuf,
    /// `reaper_kumi.dylib`, `com.kumi.Bridge.xrnx`.
    pub name: String,
    pub folder: bool,
    pub reference: PathBuf,
}

impl Target {
    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRow {
    pub action: String,
    pub generation: Option<String>,
    pub ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub schema: String,
    pub daw: String,
    /// `installed` or `uninstalled`.
    pub status: String,
    /// The installed file or folder.
    pub target: String,
    pub reference: String,
    pub generation: Option<String>,
    pub version: Option<String>,
    /// [`tree_sha256`] of what was placed.
    pub sha256: Option<String>,
    /// Generations to roll back to, newest first.
    #[serde(default)]
    pub previous: Vec<String>,
    pub updated_ms: u64,
    #[serde(default)]
    pub history: Vec<HistoryRow>,
}

/// An action cut short leaves this; the next action cleans up after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    action: String,
    /// Paths to remove if they're still there.
    leftovers: Vec<String>,
    ms: u64,
}

#[derive(Debug)]
pub enum LifecycleError {
    Io(std::io::Error),
    File(FileError),
    /// Nothing to do that way: not installed, already installed, nothing to roll back to.
    State(String),
    Invalid(String),
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleError::Io(error) => error.fmt(f),
            LifecycleError::File(error) => error.fmt(f),
            LifecycleError::State(why) | LifecycleError::Invalid(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for LifecycleError {}

impl From<std::io::Error> for LifecycleError {
    fn from(error: std::io::Error) -> Self {
        LifecycleError::Io(error)
    }
}

impl From<FileError> for LifecycleError {
    fn from(error: FileError) -> Self {
        LifecycleError::File(error)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// A file's or folder's contents as one SHA-256: each file's path relative to the root (with `/`), size and SHA-256,
/// sorted by path. A folder's [`NOT_PAYLOAD`] files at its top are left out.
pub fn tree_sha256(path: &Path) -> std::io::Result<String> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_file() {
        return Ok(sha256_hex(&std::fs::read(path)?));
    }
    if !meta.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "neither a file nor a folder"));
    }
    let mut rows = vec![];
    collect(path, "", &mut rows)?;
    rows.sort();
    Ok(sha256_hex(rows.concat().as_bytes()))
}

fn collect(dir: &Path, prefix: &str, rows: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if prefix.is_empty() && NOT_PAYLOAD.contains(&name.as_str()) {
            continue;
        }
        let relative = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(&entry.path(), &relative, rows)?;
        } else if kind.is_file() {
            let bytes = std::fs::read(entry.path())?;
            rows.push(format!("{relative}\0{}\0{}\n", bytes.len(), sha256_hex(&bytes)));
        } else {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{relative} is a link")));
        }
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path, top: bool) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_file() {
        std::fs::copy(from, to)?;
        return Ok(());
    }
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if top && NOT_PAYLOAD.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() || kind.is_file() {
            copy_tree(&entry.path(), &to.join(&name), false)?;
        } else {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "bridge files can't hold links"));
        }
    }
    Ok(())
}

fn remove_tree(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// What [`status`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub receipt: Option<Receipt>,
    /// The installed file or folder is there.
    pub present: bool,
    /// It is what the receipt says was placed.
    pub matches: bool,
    pub reference: bool,
    pub secret: bool,
}

/// One DAW's bridge lifecycle, over Kumi's folder for it.
pub struct Lifecycle {
    pub paths: BridgePaths,
    pub target: Target,
}

impl Lifecycle {
    pub fn new(kumi_home: &Path, target: Target) -> Self {
        Self { paths: BridgePaths::new(kumi_home, target.daw), target }
    }

    pub fn receipt(&self) -> Result<Option<Receipt>, LifecycleError> {
        let path = self.paths.receipt();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let receipt: Receipt =
            serde_json::from_str(&text).map_err(|error| LifecycleError::Invalid(format!("the receipt can't be read: {error}")))?;
        if receipt.schema != RECEIPT_SCHEMA || receipt.daw != self.target.daw.id() {
            return Err(LifecycleError::Invalid("the receipt is another kind".into()));
        }
        Ok(Some(receipt))
    }

    pub fn status(&self) -> Result<Status, LifecycleError> {
        let receipt = self.receipt()?.filter(|receipt| receipt.status == "installed");
        let target = receipt.as_ref().map(|receipt| PathBuf::from(&receipt.target)).unwrap_or_else(|| self.target.path());
        let present = target.exists();
        let matches = present && receipt.as_ref().and_then(|r| r.sha256.clone()) == tree_sha256(&target).ok();
        let reference = Reference::read(&self.target.reference).is_ok_and(|reference| reference.daw == self.target.daw.id());
        let secret = crate::files::read_secret(&self.paths.secret()).is_ok();
        Ok(Status { receipt, present, matches, reference, secret })
    }

    fn write_receipt(&self, receipt: &Receipt) -> Result<(), LifecycleError> {
        let mut text = serde_json::to_string_pretty(receipt).expect("receipts serialize");
        text.push('\n');
        owner::write_owner_only(&self.paths.receipt(), text.as_bytes())?;
        Ok(())
    }

    fn begin(&self, action: &str, leftovers: &[&Path]) -> Result<(), LifecycleError> {
        let journal = Journal {
            action: action.into(),
            leftovers: leftovers.iter().map(|path| path.to_string_lossy().into_owned()).collect(),
            ms: now_ms(),
        };
        owner::write_owner_only(&self.paths.journal(), serde_json::to_string(&journal).expect("journals serialize").as_bytes())?;
        Ok(())
    }

    fn end(&self) {
        let _ = std::fs::remove_file(self.paths.journal());
    }

    /// Cleans up after an action that was cut short, and after old copies a running DAW held.
    fn recover(&self) -> Result<(), LifecycleError> {
        if let Ok(text) = std::fs::read_to_string(self.paths.journal()) {
            if let Ok(journal) = serde_json::from_str::<Journal>(&text) {
                for leftover in journal.leftovers {
                    let _ = remove_tree(Path::new(&leftover));
                }
            }
            self.end();
        }
        let old = format!("{}.kumi-old-", self.target.name);
        if let Ok(entries) = std::fs::read_dir(&self.target.dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(&old) {
                    let _ = remove_tree(&entry.path());
                }
            }
        }
        Ok(())
    }

    /// Copies `source` into a new generation; returns its name and hash.
    fn stage(&self, source: &Path, version: &str) -> Result<(String, String), LifecycleError> {
        if version.is_empty() || !version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')) {
            return Err(LifecycleError::Invalid(format!("not a version: {version}")));
        }
        owner::create_owner_only_dir(&self.paths.generations())?;
        // Named by version and time; two in the same millisecond take the next free one.
        let mut ms = now_ms();
        let (generation, dir) = loop {
            let generation = format!("{version}-{ms}");
            let dir = self.paths.generations().join(&generation);
            match std::fs::create_dir(&dir) {
                Ok(()) => break (generation, dir),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => ms += 1,
                Err(error) => return Err(error.into()),
            }
        };
        let staged = dir.join(&self.target.name);
        if let Err(error) = copy_tree(source, &staged, true) {
            let _ = remove_tree(&dir);
            return Err(error.into());
        }
        let sha = tree_sha256(&staged)?;
        if sha != tree_sha256(source)? {
            let _ = remove_tree(&dir);
            return Err(LifecycleError::Invalid("the copy differs from its source".into()));
        }
        Ok((generation, sha))
    }

    /// Puts a generation's copy in the DAW's folder: a finished copy beside the target, then renamed over it. A
    /// folder, or a file the system won't replace while it's in use, first has the old one renamed aside.
    fn place(&self, generation: &str) -> Result<String, LifecycleError> {
        let source = self.paths.generations().join(generation).join(&self.target.name);
        let sha = tree_sha256(&source)?;
        std::fs::create_dir_all(&self.target.dir)?;
        let target = self.target.path();
        let token = &crate::auth::random_hex16()[..12];
        let new = self.target.dir.join(format!(".{}.kumi-new-{token}", self.target.name));
        let old = self.target.dir.join(format!("{}.kumi-old-{token}", self.target.name));
        self.begin("place", &[&new])?;
        // Renoise writes its tool preferences inside the folder: they stay with the tool.
        let kept_preferences = self.target.folder.then(|| std::fs::read(target.join("preferences.xml")).ok()).flatten();
        copy_tree(&source, &new, true)?;
        if let Some(preferences) = kept_preferences {
            std::fs::write(new.join("preferences.xml"), preferences)?;
        }
        if tree_sha256(&new)? != sha {
            let _ = remove_tree(&new);
            return Err(LifecycleError::Invalid("the copy beside the bridge differs from its generation".into()));
        }
        let replaced = if !self.target.folder { std::fs::rename(&new, &target).is_ok() } else { false };
        if !replaced {
            if target.exists() {
                std::fs::rename(&target, &old)?;
            }
            if let Err(error) = std::fs::rename(&new, &target) {
                if old.exists() {
                    let _ = std::fs::rename(&old, &target);
                }
                let _ = remove_tree(&new);
                self.end();
                return Err(error.into());
            }
            // A DLL Windows holds open can't be removed yet: the next action removes it.
            let _ = remove_tree(&old);
        }
        self.end();
        Ok(sha)
    }

    fn write_reference(&self) -> Result<(), LifecycleError> {
        let reference = Reference::new(self.target.daw, &self.paths);
        if let Some(parent) = self.target.reference.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = owner::temporary_beside(&self.target.reference);
        std::fs::write(&temporary, reference.to_json())?;
        std::fs::rename(&temporary, &self.target.reference)?;
        Ok(())
    }

    fn prune(&self, receipt: &Receipt) {
        let keep: Vec<&String> = receipt.generation.iter().chain(receipt.previous.iter()).collect();
        if let Ok(entries) = std::fs::read_dir(self.paths.generations()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !keep.iter().any(|kept| **kept == name) {
                    let _ = remove_tree(&entry.path());
                }
            }
        }
    }

    fn record(&self, mut receipt: Receipt, action: &str) -> Result<Receipt, LifecycleError> {
        receipt.updated_ms = now_ms();
        receipt.history.insert(0, HistoryRow { action: action.into(), generation: receipt.generation.clone(), ms: receipt.updated_ms });
        receipt.history.truncate(HISTORY);
        receipt.previous.truncate(KEPT_GENERATIONS);
        self.write_receipt(&receipt)?;
        self.prune(&receipt);
        Ok(receipt)
    }

    /// Installs `source` (the bridge's file or folder) as `version`, or upgrades to it when installed already: the
    /// secret is made if missing, the reference written, and the version before kept to roll back to.
    pub fn install(&self, source: &Path, version: &str) -> Result<Receipt, LifecycleError> {
        owner::create_owner_only_dir(&self.paths.root)?;
        // The bridge writes into these but may not be able to make them (Renoise's Lua can't make folders).
        owner::create_owner_only_dir(&self.paths.discovery())?;
        owner::create_owner_only_dir(&self.paths.logs())?;
        self.recover()?;
        let before = self.receipt()?.filter(|receipt| receipt.status == "installed");
        let (generation, _) = self.stage(source, version)?;
        ensure_secret(&self.paths.secret())?;
        let sha = match self.place(&generation) {
            Ok(sha) => sha,
            Err(error) => {
                let _ = remove_tree(&self.paths.generations().join(&generation));
                return Err(error);
            }
        };
        self.write_reference()?;
        let (action, previous) = match &before {
            Some(before) => ("upgrade", before.generation.iter().chain(before.previous.iter()).cloned().collect()),
            None => ("install", vec![]),
        };
        let receipt = Receipt {
            schema: RECEIPT_SCHEMA.into(),
            daw: self.target.daw.id().into(),
            status: "installed".into(),
            target: self.target.path().to_string_lossy().into_owned(),
            reference: self.target.reference.to_string_lossy().into_owned(),
            generation: Some(generation),
            version: Some(version.into()),
            sha256: Some(sha),
            previous,
            updated_ms: 0,
            history: before.map(|before| before.history).unwrap_or_default(),
        };
        self.record(receipt, action)
    }

    /// Goes back to the generation installed before this one.
    pub fn rollback(&self) -> Result<Receipt, LifecycleError> {
        self.recover()?;
        let Some(mut receipt) = self.receipt()?.filter(|receipt| receipt.status == "installed") else {
            return Err(LifecycleError::State(format!("Kumi's {} bridge isn't installed", self.target.daw)));
        };
        let Some(earlier) = receipt.previous.first().cloned() else {
            return Err(LifecycleError::State("there's no earlier version to go back to".into()));
        };
        if !self.paths.generations().join(&earlier).join(&self.target.name).exists() {
            return Err(LifecycleError::State(format!("the earlier version {earlier} is no longer kept")));
        }
        let sha = self.place(&earlier)?;
        self.write_reference()?;
        receipt.previous.remove(0);
        receipt.version = earlier.rsplit_once('-').map(|(version, _)| version.to_string());
        receipt.generation = Some(earlier);
        receipt.sha256 = Some(sha);
        self.record(receipt, "rollback")
    }

    /// Removes the bridge from the DAW's folder: the file or folder Kumi placed (one changed since is moved into
    /// Kumi's folder instead, never deleted), its reference, and the kept generations. The secret stays unless
    /// `purge_secret`.
    pub fn uninstall(&self, purge_secret: bool) -> Result<Receipt, LifecycleError> {
        self.recover()?;
        let Some(mut receipt) = self.receipt()? else {
            return Err(LifecycleError::State(format!("Kumi's {} bridge isn't installed", self.target.daw)));
        };
        let target = PathBuf::from(&receipt.target);
        if target.exists() {
            if tree_sha256(&target).ok() == receipt.sha256 {
                let token = &crate::auth::random_hex16()[..12];
                let aside = target.with_file_name(format!("{}.kumi-old-{token}", self.target.name));
                std::fs::rename(&target, &aside)?;
                let _ = remove_tree(&aside);
            } else {
                let kept = self.paths.root.join("changed");
                std::fs::create_dir_all(&kept)?;
                std::fs::rename(&target, kept.join(format!("{}-{}", self.target.name, now_ms())))?;
            }
        }
        let reference = PathBuf::from(&receipt.reference);
        if Reference::read(&reference).is_ok_and(|reference| reference.daw == self.target.daw.id()) {
            let _ = std::fs::remove_file(&reference);
        }
        if purge_secret {
            let _ = std::fs::remove_file(self.paths.secret());
        }
        receipt.status = "uninstalled".into();
        receipt.generation = None;
        receipt.version = None;
        receipt.sha256 = None;
        receipt.previous.clear();
        self.record(receipt, "uninstall")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge_folder(root: &Path, version: &str) -> PathBuf {
        let dir = root.join(format!("source-{version}")).join("com.kumi.Bridge.xrnx");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("main.lua"), format!("-- {version}\n")).unwrap();
        std::fs::write(dir.join("lib").join("json.lua"), "return {}\n").unwrap();
        dir
    }

    fn renoise(root: &Path) -> Lifecycle {
        let tools = root.join("prefs").join("V3.5.4").join("Scripts").join("Tools");
        let target = Target {
            daw: Daw::Renoise,
            dir: tools.clone(),
            name: "com.kumi.Bridge.xrnx".into(),
            folder: true,
            reference: tools.join("com.kumi.Bridge.xrnx").join("bridge-reference.json"),
        };
        Lifecycle::new(&root.join("kumi"), target)
    }

    #[test]
    fn install_upgrade_two_rollbacks_and_uninstall_of_a_folder() {
        let root = tempfile::tempdir().unwrap();
        let life = renoise(root.path());
        let installed = life.install(&bridge_folder(root.path(), "1.0"), "1.0").unwrap();
        assert_eq!((installed.status.as_str(), installed.version.as_deref()), ("installed", Some("1.0")));
        let status = life.status().unwrap();
        assert!(status.present && status.matches && status.reference && status.secret, "{status:?}");
        assert!(life.paths.discovery().is_dir() && life.paths.logs().is_dir());
        let secret = std::fs::read(life.paths.secret()).unwrap();

        // The tool's own preferences survive upgrades and aren't part of its hash.
        std::fs::write(life.target.path().join("preferences.xml"), "<prefs/>").unwrap();
        assert!(life.status().unwrap().matches);
        life.install(&bridge_folder(root.path(), "2.0"), "2.0").unwrap();
        let third = life.install(&bridge_folder(root.path(), "3.0"), "3.0").unwrap();
        assert_eq!(third.previous.len(), 2);
        assert_eq!(std::fs::read_to_string(life.target.path().join("main.lua")).unwrap(), "-- 3.0\n");
        assert_eq!(std::fs::read_to_string(life.target.path().join("preferences.xml")).unwrap(), "<prefs/>");
        assert_eq!(std::fs::read(life.paths.secret()).unwrap(), secret);

        assert_eq!(life.rollback().unwrap().version.as_deref(), Some("2.0"));
        assert_eq!(std::fs::read_to_string(life.target.path().join("main.lua")).unwrap(), "-- 2.0\n");
        assert_eq!(life.rollback().unwrap().version.as_deref(), Some("1.0"));
        assert_eq!(std::fs::read_to_string(life.target.path().join("main.lua")).unwrap(), "-- 1.0\n");
        assert!(matches!(life.rollback(), Err(LifecycleError::State(_))));
        assert!(life.status().unwrap().matches);
        assert_eq!(std::fs::read_dir(life.paths.generations()).unwrap().count(), 1);

        let removed = life.uninstall(false).unwrap();
        assert_eq!(removed.status, "uninstalled");
        assert!(!life.target.path().exists() && !life.target.reference.exists());
        assert!(life.paths.secret().exists());
        assert_eq!(std::fs::read_dir(&life.target.dir).unwrap().count(), 0);
        assert_eq!(
            removed.history.iter().map(|row| row.action.as_str()).collect::<Vec<_>>(),
            ["uninstall", "rollback", "rollback", "upgrade", "upgrade", "install"]
        );
        // Installing again after an uninstall is an install.
        assert_eq!(life.install(&bridge_folder(root.path(), "4.0"), "4.0").unwrap().history[0].action, "install");
    }

    #[test]
    fn a_file_target_is_replaced_by_rename_and_a_changed_one_is_kept_on_uninstall() {
        let root = tempfile::tempdir().unwrap();
        let plugins = root.path().join("REAPER").join("UserPlugins");
        let target = Target {
            daw: Daw::Reaper,
            dir: plugins.clone(),
            name: "reaper_kumi.dylib".into(),
            folder: false,
            reference: plugins.join("kumi-bridge-reference.json"),
        };
        let life = Lifecycle::new(&root.path().join("kumi"), target);
        let source = root.path().join("reaper_kumi.dylib");
        std::fs::write(&source, b"one").unwrap();
        life.install(&source, "0.1.0").unwrap();
        // Something the user dropped beside it is never touched.
        std::fs::write(plugins.join("reaper_other.dylib"), b"theirs").unwrap();
        std::fs::write(&source, b"two").unwrap();
        life.install(&source, "0.2.0").unwrap();
        assert_eq!(std::fs::read(plugins.join("reaper_kumi.dylib")).unwrap(), b"two");
        let reference = Reference::read(&plugins.join("kumi-bridge-reference.json")).unwrap();
        assert_eq!(reference.daw, "reaper");
        assert_eq!(reference.port, Some(9770));

        std::fs::write(plugins.join("reaper_kumi.dylib"), b"edited").unwrap();
        assert!(!life.status().unwrap().matches);
        life.uninstall(true).unwrap();
        assert!(!plugins.join("reaper_kumi.dylib").exists());
        assert_eq!(std::fs::read(plugins.join("reaper_other.dylib")).unwrap(), b"theirs");
        let kept: Vec<_> = std::fs::read_dir(life.paths.root.join("changed")).unwrap().flatten().collect();
        assert_eq!(std::fs::read(kept[0].path()).unwrap(), b"edited");
        assert!(!life.paths.secret().exists());
    }

    #[test]
    fn leftovers_of_an_interrupted_action_are_cleaned_up() {
        let root = tempfile::tempdir().unwrap();
        let life = renoise(root.path());
        life.install(&bridge_folder(root.path(), "1.0"), "1.0").unwrap();
        let stray = life.target.dir.join(".com.kumi.Bridge.xrnx.kumi-new-abc");
        let held = life.target.dir.join("com.kumi.Bridge.xrnx.kumi-old-def");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::create_dir_all(&held).unwrap();
        life.begin("place", &[&stray]).unwrap();
        life.install(&bridge_folder(root.path(), "2.0"), "2.0").unwrap();
        assert!(!stray.exists() && !held.exists() && !life.paths.journal().exists());
        assert!(life.status().unwrap().matches);
    }

    #[test]
    fn tree_hashes_see_names_and_contents_but_not_the_installers_files() {
        let root = tempfile::tempdir().unwrap();
        let folder = bridge_folder(root.path(), "1.0");
        let first = tree_sha256(&folder).unwrap();
        std::fs::write(folder.join("bridge-reference.json"), "{}").unwrap();
        assert_eq!(tree_sha256(&folder).unwrap(), first);
        std::fs::write(folder.join("lib").join("bridge-reference.json"), "{}").unwrap();
        assert_ne!(tree_sha256(&folder).unwrap(), first);
        assert!(life_version_rejected(root.path()));
    }

    fn life_version_rejected(root: &Path) -> bool {
        let life = renoise(root);
        matches!(life.install(&bridge_folder(root, "x"), "../x"), Err(LifecycleError::Invalid(_)))
    }
}
