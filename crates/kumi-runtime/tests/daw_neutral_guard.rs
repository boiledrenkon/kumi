//! Kumi's core never reaches into Live's integration: outside integrations/ableton, only the registry
//! (integrations/mod.rs) names it, and the crate root takes Live's constructors from the registry.

use std::path::{Path, PathBuf};

fn sources(folder: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// A line's code, without a comment after it (a `//` inside a string, as in a URL, ends it early: that only hides
/// more of the line, never adds to it).
fn code(line: &str) -> &str {
    line.split("//").next().unwrap_or("")
}

#[test]
fn nothing_outside_lives_integration_and_the_registry_names_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![];
    sources(&root, &mut files);
    let mut found = vec![];
    for file in files {
        let name = file.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        if name.starts_with("integrations/ableton/") || name == "integrations/mod.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            if code(line).contains("ableton::") || code(line).contains("mod ableton") {
                found.push(format!("{name}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(found.is_empty(), "these reach into Live's integration; take what they need from a neutral module:\n{}", found.join("\n"));
}
