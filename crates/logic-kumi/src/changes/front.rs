//! Logic in front, and its Edit menu's titles fresh. A menu item or a modified key command acts only while Logic is
//! the app in front, so a change that needs one asks macOS to bring Logic forward and checks that it is, and refuses
//! `busy` rather than fight an app that keeps the front. The Edit menu's Undo and Redo titles are set by Logic only
//! when it validates that menu: a key command of its own (the producer's Cmd-Z among them) validates nothing, so the
//! titles are refreshed on purpose, invisibly: a key equivalent Logic doesn't own while it is in front, or a press
//! and a cancel of the menu bar item while it is behind (its menu bar isn't on screen then).

use crate::ax::strings::Strings;
use crate::ax::{Ax, AxError, Root};
use crate::changes::{busy, nudge, Done};

/// The key equivalent posted to refresh the menus while Logic is in front: one Logic doesn't own.
const REFRESH_KEY: &str = "f18";
const REFRESH_MODS: [&str; 4] = ["cmd", "opt", "ctrl", "shift"];
/// How long the menu bar item's press and cancel get to validate the menu.
const PRESS_SETTLE_MS: u64 = 120;
const CANCEL_SETTLE_MS: u64 = 40;

/// Brings Logic in front for a menu item, a key or a click; `busy` when another app keeps the front.
pub fn ensure(ax: &mut Ax) -> Done<()> {
    if nudge::read(ax.in_front())? {
        return Ok(());
    }
    if nudge::read(ax.activate())? {
        return Ok(());
    }
    Err(busy("Logic Pro couldn't be brought in front (another app keeps it); nothing changed"))
}

/// The menu bar item of the Edit menu: its path below the menu bar, found by its title in Logic's language.
pub fn edit_menu(ax: &mut Ax, strings: &Strings) -> Result<String, AxError> {
    let bar = ax.tree(&Root::MenuBar, "", 1, 20, Some(&["title"]))?;
    let found = bar.children.iter().find(|item| item.role == "AXMenuBarItem" && strings.is(&item.title, "Edit"));
    Ok(found.map(|item| item.path.clone()).unwrap_or_else(|| "3".to_string()))
}

/// Makes Logic validate its Edit menu, so its Undo and Redo titles say what its history holds now.
pub fn validate(ax: &mut Ax, edit: &str, in_front: bool) -> Result<(), AxError> {
    if in_front {
        ax.keys(REFRESH_KEY, &REFRESH_MODS)?;
    } else {
        ax.action(&Root::MenuBar, edit, "AXPress", PRESS_SETTLE_MS)?;
        ax.action(&Root::MenuBar, edit, "AXCancel", CANCEL_SETTLE_MS)?;
    }
    Ok(())
}

/// The Edit menu's first two items: the Undo and Redo titles as they stand.
pub fn titles(ax: &mut Ax, edit: &str) -> Result<(String, String), AxError> {
    let menu = ax.tree(&Root::MenuBar, &format!("{edit}.0"), 1, 2, Some(&["title", "enabled"]))?;
    let title = |index: usize| menu.children.get(index).map(|item| item.title.clone()).unwrap_or_default();
    Ok((title(0), title(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::{Launch, MockHands};
    use serde_json::json;

    #[test]
    fn logic_is_brought_in_front_once_and_an_app_that_keeps_the_front_is_busy() {
        let hands = MockHands::new();
        hands.set_apps(vec![json!({"bundle": "com.apple.logic10", "pid": 7, "active": true})]);
        let mut launcher = hands.clone();
        let mut helper = Launch::launch(&mut launcher).unwrap();
        let mut ax = Ax::new(helper.as_mut(), 7);
        assert!(ensure(&mut ax).is_ok());
        assert_eq!(hands.calls_of("activate"), 0, "in front already");
        hands.set_apps(vec![json!({"bundle": "com.apple.logic10", "pid": 7, "active": false})]);
        hands.reply("activate", Ok(json!({"active": true})));
        assert!(ensure(&mut ax).is_ok());
        hands.reply("activate", Ok(json!({"active": false})));
        let refused = ensure(&mut ax).unwrap_err();
        assert_eq!(refused.0, "busy");
        assert!(refused.1.ends_with("nothing changed"), "{}", refused.1);
    }

    #[test]
    fn the_edit_menu_is_found_by_its_title_and_validated_by_a_key_in_front_or_a_press_behind() {
        let hands = MockHands::new();
        hands.set_tree(json!({"role": "AXMenuBar", "c": [
            {"i": "0", "role": "AXMenuBarItem", "title": "Apple"}, {"i": "1", "role": "AXMenuBarItem", "title": "Logic Pro"},
            {"i": "2", "role": "AXMenuBarItem", "title": "File"}, {"i": "3", "role": "AXMenuBarItem", "title": "Bearbeiten"}
        ]}));
        let strings = Strings::with("de", &[("Edit", "Bearbeiten")]);
        let mut launcher = hands.clone();
        let mut helper = Launch::launch(&mut launcher).unwrap();
        let mut ax = Ax::new(helper.as_mut(), 7);
        assert_eq!(edit_menu(&mut ax, &strings).unwrap(), "3");
        hands.reply("keys", Ok(json!({"posted": true, "active": true})));
        validate(&mut ax, "3", true).unwrap();
        hands.reply("action", Ok(json!({"result": "success"})));
        hands.reply("action", Ok(json!({"result": "success"})));
        validate(&mut ax, "3", false).unwrap();
        let calls = hands.calls();
        assert_eq!((calls[1].0.as_str(), calls[1].1["key"].as_str()), ("keys", Some("f18")));
        assert_eq!((calls[2].1["name"].as_str(), calls[3].1["name"].as_str()), (Some("AXPress"), Some("AXCancel")));
        assert_eq!((calls[2].1["root"].as_str(), calls[2].1["path"].as_str()), (Some("menubar"), Some("3")));
        hands.set_tree(json!({"role": "AXMenu", "c": [
            {"i": "3.0.0", "role": "AXMenuItem", "title": "Undo Renaming"}, {"i": "3.0.1", "role": "AXMenuItem", "title": "Can’t Redo"}
        ]}));
        assert_eq!(titles(&mut ax, "3").unwrap(), ("Undo Renaming".to_string(), "Can’t Redo".to_string()));
    }
}
