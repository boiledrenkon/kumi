//! An item's chunk (the RPP text REAPER writes for it), for the changes the API has no precise inverse for: a split, a
//! take deleted or rendered, a glue. Such a change keeps the item's chunk from before it and puts it back whole, but only
//! while the item is exactly as the change left it (its print, below): then nothing of the producer's is overwritten.
//!
//! The print leaves out what only the screen or a selection changes (the item's selection, the MIDI editor's view and
//! filter, which notes are selected, open FX windows), so opening an item to look at it doesn't make it "changed".

use crate::reaper::{Item, Reaper};

/// The most of an item's state a change keeps to put back: it travels in a frame and sits in the host's journal.
pub const MOST_KEPT: usize = 24 << 20;

/// Lines that say only how the item is shown or selected, at any depth.
const VIEW_LINES: [&str; 10] =
    ["CFGEDITVIEW ", "CFGEDIT ", "EVTFILTER ", "KEYSNAP ", "TRACKSEL ", "VELLANE ", "SHOW ", "LASTSEL ", "DOCKED ", "FLOATPOS "];

/// The chunk without what only the screen or a selection changes: an item's selection and list number (in an item's chunk,
/// or in each item of a track's), the MIDI editor's view, which events are selected, FX windows.
pub fn normalized(chunk: &str) -> String {
    let mut out = String::with_capacity(chunk.len());
    // The blocks open around each line, by their first word: an item's own lines are those right inside `<ITEM`.
    let mut blocks: Vec<&str> = Vec::new();
    for line in chunk.lines() {
        let trimmed = line.trim_start();
        let opens = trimmed.starts_with('<');
        let closes = trimmed.trim_end() == ">";
        let in_item = blocks.last() == Some(&"<ITEM");
        let item_level = in_item && (trimmed.starts_with("SEL ") || trimmed.starts_with("IID "));
        if item_level || VIEW_LINES.iter().any(|prefix| trimmed.starts_with(prefix)) || trimmed == "FLOAT" {
            continue;
        }
        // A selected MIDI event (`e`, `em`, `<x`) is the same event as an unselected one.
        let line = if trimmed.starts_with("e ") || trimmed.starts_with("em ") {
            format!("E{}", &trimmed[1..])
        } else if trimmed.starts_with("<x ") {
            format!("<X{}", &trimmed[2..])
        } else {
            trimmed.to_string()
        };
        out.push_str(line.trim_end());
        out.push('\n');
        if opens {
            blocks.push(trimmed.split_whitespace().next().unwrap_or("<"));
        } else if closes {
            blocks.pop();
        }
    }
    out
}

/// A short, stable word for what the chunk holds: two chunks that differ only in their view and selection share it.
pub fn print_of(chunk: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalized(chunk).bytes() {
        hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The item's print, or None when REAPER won't give its chunk.
pub fn item_print(reaper: &Reaper, item: Item) -> Option<String> {
    reaper.item_chunk(item).map(|chunk| print_of(&chunk))
}

/// The chunk without its ids: the item's, its takes', their MIDI sources' and pools, their FX's. REAPER gives an item made
/// from it new ones, so it is a copy that shares nothing with the item it came from (a MIDI copy is not pooled with it).
pub fn without_ids(chunk: &str) -> String {
    chunk
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !["IGUID ", "GUID ", "POOLEDEVTS ", "FXID "].iter().any(|prefix| trimmed.starts_with(prefix))
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM: &str = "<ITEM\nPOSITION 8\nSEL 1\nIGUID {A}\nIID 2\nNAME \"vox\"\nGUID {T}\n<SOURCE MIDI\nHASDATA 1 960 QN\nPOOLEDEVTS {P}\ne 0 90 3c 64\nE 480 80 3c 00\n<x 100 0\n8H5/CQH3\n>\nCFGEDITVIEW 0 0 0\nGUID {S}\n>\n<TAKEFX\nSHOW 0\nLASTSEL 0\nDOCKED 0\nFLOATPOS 0 0 0 0\nFXID {F}\n>\n>\n";

    #[test]
    fn the_print_leaves_out_the_selection_and_the_view_and_nothing_else() {
        let looked_at = ITEM
            .replace("SEL 1", "SEL 0")
            .replace("IID 2", "IID 7")
            .replace("e 0 90", "E 0 90")
            .replace("<x 100", "<X 100")
            .replace("CFGEDITVIEW 0 0 0", "CFGEDITVIEW 12 4 0")
            .replace("SHOW 0", "SHOW 1")
            .replace("FLOATPOS 0 0 0 0", "FLOATPOS 10 10 300 200");
        assert_eq!(print_of(ITEM), print_of(&looked_at));
        for changed in
            [ITEM.replace("POSITION 8", "POSITION 9"), ITEM.replace("E 480 80", "E 481 80"), ITEM.replace("NAME \"vox\"", "NAME \"vox 2\"")]
        {
            assert_ne!(print_of(ITEM), print_of(&changed));
        }
        // The `SEL` of a nested block or of an active take is not the item's selection.
        assert_ne!(print_of(ITEM), print_of(&ITEM.replace("NAME \"vox\"", "TAKE SEL\nNAME \"vox\"")));
    }

    #[test]
    fn a_tracks_print_leaves_out_its_items_selection_too() {
        let track = format!("<TRACK {{X}}\nNAME \"Vox\"\nSEL 1\n{ITEM}>\n");
        let looked_at = track.replacen("SEL 1\nIGUID", "SEL 0\nIGUID", 1);
        assert_ne!(track, looked_at);
        assert_eq!(print_of(&track), print_of(&looked_at));
        // (a `SEL` right inside the track's own block is not an item's)
        assert_ne!(print_of(&track), print_of(&track.replacen("NAME \"Vox\"\nSEL 1", "NAME \"Vox\"\nSEL 0", 1)));
    }

    #[test]
    fn a_copy_has_no_ids_and_keeps_everything_else() {
        let copy = without_ids(ITEM);
        for id in ["IGUID", "GUID {T}", "GUID {S}", "POOLEDEVTS", "FXID"] {
            assert!(!copy.contains(id), "{id}: {copy}");
        }
        assert!(copy.contains("POSITION 8\n") && copy.contains("E 480 80 3c 00\n") && copy.contains("8H5/CQH3\n"));
        assert_eq!(copy.lines().count(), ITEM.lines().count() - 5);
    }
}
