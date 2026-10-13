//! The options an answer ends on when Kumi asks the producer to pick one, so one key can answer.

use std::sync::LazyLock;

use kumi_common::js::string::utf16_len;
use regex::Regex;

/// Longest option offered as a one-key answer (UTF-16 units); past it, the list is prose, not options.
const LONGEST: usize = 100;

/// An answer's question and options: a question ("?" or Japanese and Chinese "？"), then a numbered list (2
/// to 9 short items, "1." or "1)", or "1．" and "1、" with no space needed, in order) with nothing after it.
/// None when the answer doesn't end that way.
pub fn choices(answer: &str) -> Option<(String, Vec<String>)> {
    static ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([1-9])(?:[.)]\s+|[．、]\s*)(\S.*)$").unwrap());
    let mut items = Vec::new();
    let mut question = None;
    for line in answer.lines().rev().map(str::trim).filter(|line| !line.is_empty()) {
        match ITEM.captures(line) {
            Some(item) if question.is_none() => items.push((item[1].parse::<usize>().ok()?, plain(&item[2]))),
            _ => {
                question = Some(line);
                break;
            }
        }
    }
    items.reverse();
    let numbered = items.iter().enumerate().all(|(index, (n, _))| *n == index + 1);
    let short = items.iter().all(|(_, text)| !text.is_empty() && utf16_len(text) <= LONGEST);
    let question = question?;
    (question.contains(['?', '？']) && (2..=9).contains(&items.len()) && numbered && short)
        .then(|| (plain(question), items.into_iter().map(|(_, text)| text).collect()))
}

/// The option Kumi recommends, marked "(recommended)" at its end, with the mark taken off every option: the one Kumi
/// takes when nobody's there to pick. None when no option, or more than one, is marked.
pub fn recommended(options: &mut [String]) -> Option<usize> {
    static MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s*[(（]\s*recommended\s*[)）]\s*$").unwrap());
    let marked: Vec<usize> = options.iter().enumerate().filter(|(_, option)| MARK.is_match(option)).map(|(index, _)| index).collect();
    for option in options.iter_mut() {
        *option = MARK.replace(option, "").trim().to_string();
    }
    (marked.len() == 1).then(|| marked[0])
}

/// An option without its Markdown emphasis.
fn plain(text: &str) -> String {
    text.replace("**", "").replace("__", "").replace('`', "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_ending_on_a_short_numbered_list_offers_its_options() {
        assert_eq!(
            choices("I found two basses.\n\nWhich one should get the sidechain?\n\n1. **Sub Bass**\n2) `Reese`\n"),
            Some(("Which one should get the sidechain?".to_string(), vec!["Sub Bass".to_string(), "Reese".to_string()]))
        );
        assert_eq!(choices("Which track?\n1. Bass\n\n2. Lead\n\n3. A new track").map(|(_, c)| c.len()), Some(3));
        assert_eq!(
            choices("どちらのベースにサイドチェインをかけますか？\n1．サブベース\n2．リース").map(|(_, c)| c),
            Some(vec!["サブベース".to_string(), "リース".to_string()])
        );
        assert_eq!(
            choices("要给哪一条贝斯加侧链？\n1、低音\n2、Reese").map(|(_, c)| c),
            Some(vec!["低音".to_string(), "Reese".to_string()])
        );
    }

    #[test]
    fn one_recommended_option_is_taken_off_its_words_and_named() {
        let mut options = vec!["Sidechain the Reese".to_string(), "Duck both (recommended)".to_string(), "Leave it".to_string()];
        assert_eq!(recommended(&mut options), Some(1));
        assert_eq!(options, ["Sidechain the Reese", "Duck both", "Leave it"]);
        let mut options = vec!["Warm (Recommended)".to_string(), "Glassy".to_string()];
        assert_eq!(recommended(&mut options), Some(0));
        // Two marked, or none: Kumi takes neither.
        let mut options = vec!["A (recommended)".to_string(), "B (recommended)".to_string()];
        assert_eq!(recommended(&mut options), None);
        assert_eq!(options, ["A", "B"]);
        assert_eq!(recommended(&mut ["A".to_string(), "B".to_string()]), None);
        // Only a mark at the end counts.
        assert_eq!(recommended(&mut ["the recommended one".to_string(), "B".to_string()]), None);
    }

    #[test]
    fn anything_else_offers_nothing() {
        for answer in [
            "Done: added a compressor.",
            "Here's what changed:\n1. Tempo 120 → 124\n2. Swing 10%",
            "Which one?\n1. Bass",
            "Which one?\n1. Bass\n3. Lead",
            "Which one?\n1.5 dB\n2.5 dB",
            "Which one?\n1. Bass\n2. Lead\n\nOr tell me another.",
            &format!("Which one?\n1. {}\n2. Lead", "x".repeat(101)),
        ] {
            assert_eq!(choices(answer), None, "{answer}");
        }
    }
}
