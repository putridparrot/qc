use std::sync::{Arc, RwLock};

use nu_ansi_term::Style;
use reedline::{Highlighter, Hinter, History, StyledText};

pub type SharedHints = Arc<RwLock<Vec<String>>>;

fn matching_suffix(shortcuts: &[String], line: &str, pos: usize) -> Option<String> {
    let prefix = line.get(..pos)?;

    if prefix.is_empty() {
        return None;
    }

    // Prefer the shortest prefix match (most specific / least surprising).
    shortcuts
        .iter()
        .filter(|s| s.starts_with(prefix) && s.len() > prefix.len())
        .min_by_key(|s| s.len())
        .map(|s| s[prefix.len()..].to_owned())
}

fn common_prefix_len(left: &str, right: &str) -> usize {
    let mut matched = 0;

    for ((left_index, left_char), (_, right_char)) in left.char_indices().zip(right.char_indices())
    {
        if left_char != right_char {
            break;
        }

        matched = left_index + left_char.len_utf8();
    }

    matched
}

fn best_match_len(shortcuts: &[String], line: &str, pos: usize) -> usize {
    let Some(prefix) = line.get(..pos) else {
        return 0;
    };

    shortcuts
        .iter()
        .map(|shortcut| common_prefix_len(prefix, shortcut))
        .max()
        .unwrap_or(0)
}

/// Returns `true` when every character of `needle` appears in `haystack`
/// in the same order (case-insensitive). An empty needle never matches.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut haystack_iter = haystack.chars();
    'needle: for nc in needle.chars() {
        let nc_lower = nc.to_lowercase().next().unwrap_or(nc);
        loop {
            match haystack_iter.next() {
                Some(hc) => {
                    if hc.to_lowercase().next().unwrap_or(hc) == nc_lower {
                        continue 'needle;
                    }
                }
                None => return false,
            }
        }
    }
    true
}

pub struct ShortcutHinter {
    shortcuts: SharedHints,
    style: Style,
    current_hint: String,
}

impl ShortcutHinter {
    pub fn new(shortcuts: SharedHints, style: Style) -> Self {
        Self {
            shortcuts,
            style,
            current_hint: String::new(),
        }
    }

    fn first_token(hint: &str) -> String {
        let mut token = String::new();
        let mut reached_content = false;

        for character in hint.chars() {
            if reached_content && character.is_whitespace() {
                break;
            }

            if !character.is_whitespace() {
                reached_content = true;
            }

            token.push(character);
        }

        token
    }
}

pub struct HintHighlighter {
    shortcuts: SharedHints,
    partial_match_style: Style,
    exact_match_style: Style,
    fuzzy_match_style: Style,
    unmatched_style: Style,
    neutral_style: Style,
}

impl HintHighlighter {
    pub fn new(
        shortcuts: SharedHints,
        partial_match_style: Style,
        exact_match_style: Style,
        fuzzy_match_style: Style,
        unmatched_style: Style,
    ) -> Self {
        Self {
            shortcuts,
            partial_match_style,
            exact_match_style,
            fuzzy_match_style,
            unmatched_style,
            neutral_style: Style::new(),
        }
    }
}

impl Highlighter for HintHighlighter {
    fn highlight(&self, line: &str, cursor: usize) -> StyledText {
        let mut styled_text = StyledText::new();
        let shortcuts = self
            .shortcuts
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if line.is_empty() {
            styled_text.push((self.neutral_style, String::new()));
            return styled_text;
        }

        let match_len = best_match_len(&shortcuts, line, cursor);
        let exact_match = line
            .get(..cursor)
            .is_some_and(|prefix| shortcuts.iter().any(|shortcut| shortcut == prefix))
            && cursor == line.len();

        if exact_match {
            styled_text.push((self.exact_match_style, line.to_owned()));
            return styled_text;
        }

        if match_len == 0 {
            // No prefix match — check for a fuzzy (subsequence) match.
            let prefix = line.get(..cursor).unwrap_or("");
            if !prefix.is_empty() && shortcuts.iter().any(|s| is_subsequence(prefix, s)) {
                styled_text.push((self.fuzzy_match_style, line.to_owned()));
            } else {
                styled_text.push((self.neutral_style, line.to_owned()));
            }
            return styled_text;
        }

        if let Some(matched) = line.get(..match_len) {
            styled_text.push((self.partial_match_style, matched.to_owned()));
        }

        if let Some(unmatched) = line.get(match_len..)
            && !unmatched.is_empty()
        {
            styled_text.push((self.unmatched_style, unmatched.to_owned()));
        }

        styled_text
    }
}

impl Hinter for ShortcutHinter {
    fn handle(
        &mut self,
        line: &str,
        pos: usize,
        _history: &dyn History,
        use_ansi_coloring: bool,
        _cwd: &str,
    ) -> String {
        let shortcuts = self
            .shortcuts
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let Some(suffix) = matching_suffix(&shortcuts, line, pos) else {
            self.current_hint.clear();
            return String::new();
        };

        self.current_hint = suffix;

        if use_ansi_coloring {
            self.style.paint(&self.current_hint).to_string()
        } else {
            self.current_hint.clone()
        }
    }

    fn complete_hint(&self) -> String {
        self.current_hint.clone()
    }

    fn next_hint_token(&self) -> String {
        Self::first_token(&self.current_hint)
    }
}
