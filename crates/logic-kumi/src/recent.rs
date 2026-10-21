//! `read {kind: "recent"}`: the projects Logic opened lately, as its File > Open Recent submenu lists them, most recent
//! first. Accessibility reads the submenu's items without opening the menu (each is `_openRecentDocument:` by its
//! identifier, titled with the project's file name), and no item says where its project is: Logic keeps the list in
//! macOS's shared file list, which macOS keeps from other processes, and its preferences hold no project path. So a
//! row's `path` is known when the project is the one open (its window's document) or when Spotlight finds exactly one
//! project of that file name where Kumi may look (never under `~/Music`, which macOS keeps from a process without the
//! Files and Folders permission); else it is `null`.

use std::path::Path;

use serde_json::{json, Value};

use crate::ax::{Ax, AxError, Root};
use crate::project::Project;

/// The identifiers AppKit gives the recent documents' items and the item that clears them.
const OPEN_RECENT_ITEM: &str = "_openRecentDocument:";
const CLEAR_ITEM: &str = "clearRecentDocuments:";

/// A project's name as a producer says it: its file name without the package's extension.
pub fn name_of(file: &str) -> String {
    for extension in [".logicx", ".logic"] {
        if let Some(stem) = file.strip_suffix(extension) {
            if !stem.is_empty() {
                return stem.to_string();
            }
        }
    }
    file.to_string()
}

/// The recent projects' file names, in the submenu's order.
pub fn titles(project: &Project, ax: &mut Ax) -> Result<Vec<String>, AxError> {
    let bar = ax.tree(&Root::MenuBar, "", 1, 20, Some(&["role", "title"]))?;
    let file = bar
        .children
        .iter()
        .find(|item| item.role == "AXMenuBarItem" && project.strings.is(&item.title, "File"))
        .ok_or_else(|| AxError::Missing("its File menu".into()))?;
    let menu = ax.tree(&Root::MenuBar, &format!("{}.0", file.path), 1, 60, Some(&["role", "title"]))?;
    let item = menu
        .children
        .iter()
        .find(|item| item.role == "AXMenuItem" && project.strings.is(&item.title, "Open Recent"))
        .ok_or_else(|| AxError::Missing("its File > Open Recent menu".into()))?;
    let submenu = ax.tree(&Root::MenuBar, &format!("{}.0", item.path), 1, 60, Some(&["role", "title", "id"]))?;
    if !submenu.children.iter().any(|item| item.id == OPEN_RECENT_ITEM || item.id == CLEAR_ITEM) {
        return Err(AxError::Missing("the recent projects in its File > Open Recent menu".into()));
    }
    Ok(submenu
        .children
        .iter()
        .filter(|item| item.role == "AXMenuItem" && item.id == OPEN_RECENT_ITEM && !item.title.is_empty())
        .map(|item| item.title.clone())
        .collect())
}

/// The rows: `{index, name, path, open}` for each recent project, the open one's path from its window, the others'
/// from Spotlight when it finds one project of the name.
pub fn rows(project: &Project, ax: &mut Ax) -> Result<Vec<Value>, AxError> {
    let titles = titles(project, ax)?;
    let open_file = project.path.as_deref().and_then(|path| Path::new(path.trim_end_matches('/')).file_name()).map(|n| n.to_string_lossy());
    let unknown: Vec<String> = titles.iter().filter(|title| open_file.as_deref() != Some(title.as_str())).cloned().collect();
    let found = crate::disk::projects_named(&unknown);
    Ok(titles
        .iter()
        .enumerate()
        .map(|(at, title)| {
            let open = open_file.as_deref() == Some(title.as_str());
            let path = if open {
                project.path.clone()
            } else {
                found.iter().find(|(name, _)| name == title).and_then(|(_, paths)| (paths.len() == 1).then(|| paths[0].clone()))
            };
            json!({"index": at + 1, "name": name_of(title), "path": path, "open": open})
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::strings::Strings;
    use crate::helper::{Launch, MockHands};

    #[test]
    fn the_recent_projects_are_the_open_recent_items_with_a_path_where_one_is_known() {
        assert_eq!(name_of("Scratch.logicx"), "Scratch");
        assert_eq!(name_of("Old Song.logic"), "Old Song");
        assert_eq!(name_of("Song"), "Song");
        let hands = MockHands::new();
        hands.reply(
            "tree",
            Ok(json!({"node": {"role": "AXMenuBar", "c": [
                {"i": "0", "role": "AXMenuBarItem", "title": "Apple"}, {"i": "1", "role": "AXMenuBarItem", "title": "Logic Pro"},
                {"i": "2", "role": "AXMenuBarItem", "title": "File"}
            ]}})),
        );
        hands.reply(
            "tree",
            Ok(json!({"node": {"role": "AXMenu", "i": "2.0", "c": [
                {"i": "2.0.0", "role": "AXMenuItem", "title": "New"}, {"i": "2.0.3", "role": "AXMenuItem", "title": "Open Recent"}
            ]}})),
        );
        hands.reply(
            "tree",
            Ok(json!({"node": {"role": "AXMenu", "i": "2.0.3.0", "c": [
                {"i": "2.0.3.0.0", "role": "AXMenuItem", "title": "Scratch.logicx", "id": OPEN_RECENT_ITEM},
                {"i": "2.0.3.0.1", "role": "AXMenuItem", "title": "Demo.logicx", "id": OPEN_RECENT_ITEM},
                {"i": "2.0.3.0.2", "role": "AXMenuItem", "title": "Twice.logicx", "id": OPEN_RECENT_ITEM},
                {"i": "2.0.3.0.3", "role": "AXMenuItem"},
                {"i": "2.0.3.0.4", "role": "AXMenuItem", "title": "Clear Menu", "id": CLEAR_ITEM}
            ]}})),
        );
        let mut launcher = hands.clone();
        let mut helper = Launch::launch(&mut launcher).unwrap();
        let mut ax = Ax::new(helper.as_mut(), 7);
        let mut project = Project::new(Strings::english());
        project.set_identity(Some("id"), Some("/Users/me/Music/Logic/Scratch.logicx"), "Scratch");
        crate::disk::set_hook(Some(crate::disk::replaying(&[
            json!({"kind": "spotlight", "path": "Demo.logicx\nTwice.logicx", "answer": {
                "Demo.logicx": ["/Users/me/Projects/Demo.logicx"], "Twice.logicx": ["/a/Twice.logicx", "/b/Twice.logicx"]
            }}),
        ])));
        let rows = rows(&project, &mut ax).unwrap();
        crate::disk::set_hook(None);
        assert_eq!(
            rows,
            vec![
                json!({"index": 1, "name": "Scratch", "path": "/Users/me/Music/Logic/Scratch.logicx", "open": true}),
                json!({"index": 2, "name": "Demo", "path": "/Users/me/Projects/Demo.logicx", "open": false}),
                json!({"index": 3, "name": "Twice", "path": null, "open": false}),
            ]
        );
        let ops: Vec<(String, Value)> = hands.calls();
        assert!(ops.iter().all(|(op, params)| op == "tree" && params["root"] == "menubar"), "reads of the menu bar only: {ops:?}");
    }
}
