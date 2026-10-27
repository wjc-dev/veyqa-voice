//! Shared user-managed hotwords for every cloud ASR engine.
//! Mirrors `src/custom_vocabulary.py`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use fancy_regex::Regex;

use crate::paths;

pub const MAX_TERM_CHARACTERS: usize = 64;
/// Spelling restoration is cheap enough for the full Qwen vocabulary size.
pub const MAX_SPELLING_TERMS: usize = 2_000;

/// `CUSTOM_VOCABULARY_FILE`, or the per-user default when unset.
pub fn configured_path() -> PathBuf {
    match std::env::var("CUSTOM_VOCABULARY_FILE") {
        Ok(value) => PathBuf::from(value),
        Err(_) => paths::custom_vocabulary_file(),
    }
}

/// Load one hotword per line, preserving order and removing duplicates.
///
/// The file stays plain text so it can be edited, backed up, or reviewed
/// without a database. Invalid or excessively long entries are ignored instead
/// of making the cloud ASR request fail.
pub fn load_custom_vocabulary(path: Option<&Path>, limit: usize) -> Vec<String> {
    let fallback;
    let path = match path {
        Some(path) => path,
        None => {
            fallback = paths::custom_vocabulary_file();
            &fallback
        }
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut terms = Vec::new();
    let mut seen = HashSet::new();
    for raw_line in split_lines(&text) {
        let term = raw_line.trim();
        if term.is_empty() || term.starts_with('#') {
            continue;
        }
        if term.chars().count() > MAX_TERM_CHARACTERS || term.split_whitespace().count() > 7 {
            continue;
        }
        let dedupe_key = term.to_lowercase();
        if !seen.insert(dedupe_key) {
            continue;
        }
        terms.push(term.to_string());
        if terms.len() >= limit {
            break;
        }
    }
    terms
}

/// Restores the exact spelling of identifier-like hotwords after ASR.
///
/// Cloud hotwords bias recognition but do not force symbols: a term such as
/// `user_id` regularly comes back as `user id`. This matcher is built from the
/// same terms and rewrites only whole-word variants (case and `_`/`-`/space
/// differences), so the committed text always uses the user's spelling.
#[derive(Debug)]
pub struct SpellingRestorer {
    pattern: Regex,
    targets: HashMap<String, String>,
}

impl SpellingRestorer {
    pub fn restore(&self, text: &str) -> String {
        let mut output = String::with_capacity(text.len());
        let mut cursor = 0usize;
        for captures in self.pattern.captures_iter(text).flatten() {
            let Some(found) = captures.get(0) else {
                continue;
            };
            let matched = found.as_str();
            let Some(target) = self.targets.get(&normalize_alias(matched)) else {
                continue;
            };
            if matched == target {
                continue;
            }
            output.push_str(&text[cursor..found.start()]);
            output.push_str(target);
            cursor = found.end();
        }
        if cursor == 0 {
            return text.to_string();
        }
        output.push_str(&text[cursor..]);
        output
    }
}

/// Builds the rewrite pattern once per settings load. Plain lowercase words
/// are ignored so ordinary English is never reformatted by accident.
pub fn spelling_restorer(terms: &[String]) -> Option<SpellingRestorer> {
    let mut aliases: HashMap<String, Option<String>> = HashMap::new();
    for term in terms {
        let term = term.trim();
        if !term.chars().any(|character| character.is_ascii_alphabetic()) {
            continue;
        }
        let distinctive = term.chars().any(|character| {
            character.is_ascii_uppercase() || character == '_' || character == '-'
        });
        if !distinctive {
            continue;
        }
        for alias in spelling_aliases(term) {
            let key = normalize_alias(&alias);
            if key
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .count()
                < 3
            {
                continue;
            }
            match aliases.get_mut(&key) {
                Some(existing) => {
                    if existing.as_deref() != Some(term) {
                        // Two terms collapse to the same spoken form; leave it
                        // untouched instead of guessing.
                        *existing = None;
                    }
                }
                None => {
                    aliases.insert(key, Some(term.to_string()));
                }
            }
        }
    }

    let mut pairs: Vec<(String, String)> = aliases
        .into_iter()
        .filter_map(|(alias, target)| target.map(|target| (alias, target)))
        .collect();
    if pairs.is_empty() {
        return None;
    }
    // Longest alias first so `user id` is never shadowed by a shorter prefix.
    pairs.sort_by(|left, right| {
        right
            .0
            .len()
            .cmp(&left.0.len())
            .then_with(|| left.0.cmp(&right.0))
    });
    let alternatives = pairs
        .iter()
        .map(|(alias, _)| alias_pattern(alias))
        .collect::<Vec<_>>()
        .join("|");
    let pattern = Regex::new(&format!(
        r"(?i)(?<![A-Za-z0-9_])(?:{alternatives})(?![A-Za-z0-9_])"
    ))
    .ok()?;
    Some(SpellingRestorer {
        pattern,
        targets: pairs.into_iter().collect(),
    })
}

fn spelling_aliases(term: &str) -> Vec<String> {
    let mut aliases = vec![
        term.to_string(),
        replace_separators(term, ' '),
        remove_separators(term),
    ];
    aliases.sort();
    aliases.dedup();
    aliases
}

fn replace_separators(text: &str, replacement: char) -> String {
    let mut output = String::with_capacity(text.len());
    let mut in_run = false;
    for character in text.chars() {
        if character == '_' || character == '-' {
            if !in_run {
                output.push(replacement);
                in_run = true;
            }
        } else {
            output.push(character);
            in_run = false;
        }
    }
    output
}

fn remove_separators(text: &str) -> String {
    text.chars()
        .filter(|character| !matches!(character, '_' | '-' | ' '))
        .collect()
}

fn normalize_alias(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn alias_pattern(alias: &str) -> String {
    let mut pattern = String::with_capacity(alias.len() + 8);
    let mut in_space = false;
    for character in alias.chars() {
        if character.is_whitespace() {
            if !in_space {
                pattern.push_str(r"\s+");
                in_space = true;
            }
        } else {
            pattern.push_str(&fancy_regex::escape(&character.to_string()));
            in_space = false;
        }
    }
    pattern
}

/// Python `str.splitlines`, including the extra separators `str::lines` lacks.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        let boundary = matches!(
            character,
            '\n' | '\r'
                | '\u{000b}'
                | '\u{000c}'
                | '\u{001c}'
                | '\u{001d}'
                | '\u{001e}'
                | '\u{0085}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        lines.push(&text[start..index]);
        if character == '\r' {
            if let Some(&(_, '\n')) = chars.peek() {
                chars.next();
                start = index + 2;
                continue;
            }
        }
        start = index + character.len_utf8();
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "veyqa-vocabulary-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn write_temp(contents: &str) -> PathBuf {
        let path = temp_dir().join("custom_vocabulary.txt");
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn keeps_order_skips_comments_and_dedupes_case_insensitively() {
        let path = write_temp("# comment\n项目代号\n\n LightGBM \nlightgbm\nTrade Off\n");
        let terms = load_custom_vocabulary(Some(&path), 2000);
        assert_eq!(terms, vec!["项目代号", "LightGBM", "Trade Off"]);
    }

    #[test]
    fn rejects_terms_over_character_or_word_limits() {
        let long_term = "字".repeat(MAX_TERM_CHARACTERS + 1);
        let many_words = "a b c d e f g h";
        let contents = format!("{long_term}\n{many_words}\n有效\n");
        let path = write_temp(&contents);
        assert_eq!(load_custom_vocabulary(Some(&path), 2000), vec!["有效"]);
    }

    #[test]
    fn limit_is_checked_after_append_like_python() {
        let path = write_temp("一\n二\n三\n");
        assert_eq!(load_custom_vocabulary(Some(&path), 2), vec!["一", "二"]);
        assert_eq!(load_custom_vocabulary(Some(&path), 0), vec!["一"]);
    }

    #[test]
    fn missing_or_invalid_file_yields_empty_list() {
        let directory = temp_dir();
        let missing = directory.join("missing.txt");
        assert!(load_custom_vocabulary(Some(&missing), 2000).is_empty());

        let path = directory.join("invalid.txt");
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(load_custom_vocabulary(Some(&path), 2000).is_empty());
    }

    #[test]
    fn split_lines_matches_python_for_common_separators() {
        assert_eq!(split_lines("a\r\nb\rc\nd"), vec!["a", "b", "c", "d"]);
        assert_eq!(split_lines("a\n"), vec!["a"]);
        assert_eq!(split_lines(""), Vec::<&str>::new());
        assert_eq!(split_lines("a\u{2028}b"), vec!["a", "b"]);
    }

    #[test]
    fn restores_identifier_spellings_from_spoken_variants() {
        let restorer = spelling_restorer(&["user_id".into(), "LightGBM".into()]).unwrap();
        assert_eq!(
            restorer.restore("把 user id 和 userid 改成 lightgbm 吧"),
            "把 user_id 和 user_id 改成 LightGBM 吧"
        );
        assert_eq!(restorer.restore("User  ID 和 USER_ID"), "user_id 和 user_id");
        assert_eq!(restorer.restore("user_id"), "user_id");
    }

    #[test]
    fn spelling_restoration_respects_word_boundaries_and_is_idempotent() {
        let restorer = spelling_restorer(&["user_id".into()]).unwrap();
        for untouched in ["user ids", "myuser id", "superuserid", "user_id_backup"] {
            assert_eq!(restorer.restore(untouched), untouched);
        }
        let once = restorer.restore("打开 user id 字段");
        assert_eq!(once, "打开 user_id 字段");
        assert_eq!(restorer.restore(&once), once);
    }

    #[test]
    fn plain_lowercase_words_and_ambiguous_aliases_are_not_rewritten() {
        assert!(spelling_restorer(&["api".into()]).is_none());
        let ambiguous =
            spelling_restorer(&["user_id".into(), "user-id".into()]).unwrap();
        assert_eq!(ambiguous.restore("user id"), "user id");
        assert_eq!(ambiguous.restore("userid"), "userid");
    }
}
