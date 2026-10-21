//! What keeps the fixtures off a producer's work: the lab folder must be one the lab's setup marked (a `.kumi-eval-lab`
//! file in it), never Kumi's own folder's parent or the home folder itself, and the open project or song must be the lab's
//! scratch one.
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The file the lab's setup writes in a lab folder (`~/Development/kumi-daw-lab/<daw>/.kumi-eval-lab`).
pub const MARKER: &str = ".kumi-eval-lab";

/// The lab folder of a lab's kumi-home: its parent, which must hold the marker; refused for Kumi's own folder
/// (`~/.kumi`) and for any kumi-home whose parent is the home folder itself (or above it), whatever they hold.
pub fn lab_of(kumi_home: &Path, user_home: &Path) -> Result<PathBuf, String> {
    let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    // (the parent, which exists, is what's made real: a kumi-home not made yet still compares right)
    let parent = kumi_home.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let lab = real(parent);
    let kumi_home = lab.join(kumi_home.file_name().unwrap_or_default());
    let user_home = real(user_home);
    if kumi_home == user_home.join(".kumi") {
        return Err(format!("refusing: {} is Kumi's own folder, not a lab's kumi-home", kumi_home.display()));
    }
    let lab = lab.as_path();
    if user_home.starts_with(lab) {
        return Err(format!("refusing: {} is the home folder or above it, not a lab folder", lab.display()));
    }
    if !lab.join(MARKER).is_file() {
        return Err(format!("refusing: {} isn't a lab folder (it has no {MARKER} from the lab's setup)", lab.display()));
    }
    Ok(lab.to_path_buf())
}

/// Whether the open project or song is the lab's scratch one: unsaved and untitled, or saved under the lab folder; else
/// why not, naming it.
pub fn scratch(project: &Value, lab: &Path) -> Result<(), String> {
    let path = project["path"].as_str().unwrap_or_default();
    let name = project["name"].as_str().unwrap_or_default();
    let lab = lab.canonicalize().unwrap_or_else(|_| lab.to_path_buf());
    let under_lab = !path.is_empty() && Path::new(path).canonicalize().is_ok_and(|file| file.starts_with(&lab));
    let untitled = path.is_empty() && (name.is_empty() || name.eq_ignore_ascii_case("untitled"));
    if under_lab || untitled {
        Ok(())
    } else {
        Err(format!(
            "refusing: the open project isn't the lab's scratch one ({}), and the fixtures would rewrite it",
            if path.is_empty() { name } else { path }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A home folder with Kumi's own folder, a song saved in it, and a lab with its own kumi-home.
    fn home() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".kumi")).unwrap();
        std::fs::write(home.path().join("Song.rpp"), b"<REAPER_PROJECT").unwrap();
        let lab = home.path().join("Development/kumi-daw-lab/reaper");
        std::fs::create_dir_all(lab.join("kumi-home")).unwrap();
        std::fs::write(lab.join("Scratch.rpp"), b"<REAPER_PROJECT").unwrap();
        let path = home.path().to_path_buf();
        (home, path, lab)
    }

    #[test]
    fn kumis_own_folder_is_refused_so_a_project_under_home_is_never_rewritten() {
        let (_keep, home, _) = home();
        std::fs::write(home.join(MARKER), b"").unwrap(); // (even marked)
        let refused = lab_of(&home.join(".kumi"), &home).unwrap_err();
        assert!(refused.contains("Kumi's own folder"), "{refused}");
        // Before this, the lab was the kumi-home's parent, ~, and a project saved under it passed.
        assert!(lab_of(&home.join("other-kumi-home"), &home).unwrap_err().contains("home folder"));
    }

    #[test]
    fn only_a_marked_lab_folder_is_a_lab() {
        let (_keep, home, lab) = home();
        assert!(lab_of(&lab.join("kumi-home"), &home).unwrap_err().contains(MARKER), "no marker, no lab");
        std::fs::write(lab.join(MARKER), b"").unwrap();
        let found = lab_of(&lab.join("kumi-home"), &home).unwrap();
        assert_eq!(found, lab.canonicalize().unwrap());
        // In the lab: its scratch project, or an unsaved untitled one. Outside it: refused, named.
        assert!(scratch(&json!({"path": lab.join("Scratch.rpp").to_string_lossy(), "name": "Scratch.rpp"}), &found).is_ok());
        assert!(scratch(&json!({"path": "", "name": ""}), &found).is_ok());
        let outside = scratch(&json!({"path": home.join("Song.rpp").to_string_lossy(), "name": "Song.rpp"}), &found).unwrap_err();
        assert!(outside.contains("Song.rpp"), "{outside}");
        assert!(scratch(&json!({"path": "", "name": "My Mix"}), &found).is_err(), "an unsaved song with a title is someone's");
    }
}
