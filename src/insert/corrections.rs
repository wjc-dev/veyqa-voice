//! Local ASR correction learning: conservative replacement pairs learned from
//! edits made immediately after insertion. Mirrors `src/correction_learning.py`.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use unicode_general_category::{get_general_category, GeneralCategory};

use crate::paths;
use crate::text::longest_match;
use crate::timeutil::local_timestamp;

pub const STORE_VERSION: i64 = 2;
pub const MAX_TERM_LENGTH: usize = 64;
pub const ANCHOR_LENGTH: usize = 64;
pub const MAX_OBSERVED_SPAN: usize = 2000;
pub const MAX_CORRECTION_EVENTS: usize = 200;

const UNSAFE_SHORT_FORMS: [&str; 16] = [
    "这个", "那个", "就是", "然后", "而且", "但是", "因为", "所以", "我们", "你们", "他们",
    "可以", "需要", "没有", "一个", "什么",
];

pub fn experimental_correction_learning_enabled() -> bool {
    env_is_true("EXPERIMENTAL_CORRECTION_LEARNING", "false")
}

pub fn learning_enabled() -> bool {
    experimental_correction_learning_enabled() && env_is_true("CORRECTION_LEARNING_ENABLED", "false")
}

/// Shared by every ASR engine's request context; requires the experimental
/// gate plus the explicit context switch.
pub fn correction_context_enabled() -> bool {
    experimental_correction_learning_enabled() && env_is_true("CORRECTION_CONTEXT_ENABLED", "false")
}

pub fn correction_minimum_count() -> i64 {
    parse_env_int("CORRECTION_CONTEXT_MIN_COUNT", 2).max(1)
}

fn env_value(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_is_true(name: &str, default: &str) -> bool {
    env_value(name, default).to_lowercase() == "true"
}

fn parse_env_int(name: &str, default: i64) -> i64 {
    env_value(name, &default.to_string())
        .trim()
        .parse::<i64>()
        .unwrap_or(default)
}

fn python_int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Bool(flag) => Some(i64::from(*flag)),
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|float| float.trunc() as i64)),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn count_of(rule: &Map<String, Value>) -> i64 {
    python_int(rule.get("count")).unwrap_or(0)
}

fn python_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|float| float != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

fn rule_enabled(rule: &Map<String, Value>) -> bool {
    match rule.get("enabled") {
        None => true,
        Some(value) => python_truthy(Some(value)),
    }
}

fn python_str(value: Option<&Value>) -> String {
    match value {
        None => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(flag)) => (if *flag { "True" } else { "False" }).to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Null) => "None".to_string(),
        Some(other) => other.to_string(),
    }
}

fn text_or_empty(value: Option<&Value>) -> String {
    if python_truthy(value) {
        python_str(value)
    } else {
        String::new()
    }
}

fn is_punctuation(character: char) -> bool {
    matches!(
        get_general_category(character),
        GeneralCategory::ConnectorPunctuation
            | GeneralCategory::DashPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::OtherPunctuation
            | GeneralCategory::OpenPunctuation
    )
}

fn is_only_space_or_punctuation(text: &str) -> bool {
    text.chars()
        .all(|character| character.is_whitespace() || is_punctuation(character))
}

fn trim_pair(wrong: &str, correct: &str) -> (String, String) {
    let mut wrong: Vec<char> = wrong.chars().collect();
    let mut correct: Vec<char> = correct.chars().collect();
    while !wrong.is_empty()
        && !correct.is_empty()
        && wrong[0].is_whitespace()
        && correct[0].is_whitespace()
    {
        wrong.remove(0);
        correct.remove(0);
    }
    while !wrong.is_empty()
        && !correct.is_empty()
        && wrong[wrong.len() - 1].is_whitespace()
        && correct[correct.len() - 1].is_whitespace()
    {
        wrong.pop();
        correct.pop();
    }
    let wrong: String = wrong.into_iter().collect();
    let correct: String = correct.into_iter().collect();
    (wrong.trim().to_string(), correct.trim().to_string())
}

fn trim_likely_continuation(wrong: &str, correct: &str) -> String {
    let contains_cjk = wrong
        .chars()
        .any(|character| ('\u{3400}'..='\u{9fff}').contains(&character));
    let starts_with_ascii_alnum = correct
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii() && character.is_ascii_alphanumeric());
    if contains_cjk && starts_with_ascii_alnum {
        let pattern = regex::Regex::new(r"[A-Za-z0-9]+(?:[ _./+-]+[A-Za-z0-9]+)*").unwrap();
        if let Some(found) = pattern.find(correct) {
            if found.start() == 0 {
                return found.as_str().trim_end().to_string();
            }
        }
    }
    correct.to_string()
}

enum Opcode {
    Equal(usize, usize, usize, usize),
    Replace(usize, usize, usize, usize),
    Delete(usize, usize, usize, usize),
    Insert(usize, usize, usize, usize),
}

/// `difflib.SequenceMatcher(autojunk=False).get_matching_blocks()`.
fn matching_blocks(a: &[char], b: &[char]) -> Vec<(usize, usize, usize)> {
    let mut queue = vec![(0usize, a.len(), 0usize, b.len())];
    let mut blocks: Vec<(usize, usize, usize)> = Vec::new();
    while let Some((a_low, a_high, b_low, b_high)) = queue.pop() {
        let (i, j, size) = longest_match(a, b, a_low, a_high, b_low, b_high);
        if size > 0 {
            blocks.push((i, j, size));
            if a_low < i && b_low < j {
                queue.push((a_low, i, b_low, j));
            }
            if i + size < a_high && j + size < b_high {
                queue.push((i + size, a_high, j + size, b_high));
            }
        }
    }
    blocks.sort_unstable();

    let (mut i1, mut j1, mut k1) = (0usize, 0usize, 0usize);
    let mut merged = Vec::new();
    for (i2, j2, k2) in blocks {
        if i1 + k1 == i2 && j1 + k1 == j2 {
            k1 += k2;
        } else {
            if k1 > 0 {
                merged.push((i1, j1, k1));
            }
            i1 = i2;
            j1 = j2;
            k1 = k2;
        }
    }
    if k1 > 0 {
        merged.push((i1, j1, k1));
    }
    merged.push((a.len(), b.len(), 0));
    merged
}

fn opcodes(a: &[char], b: &[char]) -> Vec<Opcode> {
    let mut result = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    for (a_index, b_index, size) in matching_blocks(a, b) {
        if i < a_index && j < b_index {
            result.push(Opcode::Replace(i, a_index, j, b_index));
        } else if i < a_index {
            result.push(Opcode::Delete(i, a_index, j, b_index));
        } else if j < b_index {
            result.push(Opcode::Insert(i, a_index, j, b_index));
        }
        i = a_index + size;
        j = b_index + size;
        if size > 0 {
            result.push(Opcode::Equal(a_index, i, b_index, j));
        }
    }
    result
}

pub fn extract_correction_pairs(original: &str, edited: &str) -> Vec<(String, String)> {
    if original.is_empty() || edited.is_empty() || original == edited {
        return Vec::new();
    }
    let a: Vec<char> = original.chars().collect();
    let b: Vec<char> = edited.chars().collect();

    let mut pairs = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for opcode in opcodes(&a, &b) {
        let Opcode::Replace(a_start, a_end, b_start, b_end) = opcode else {
            continue;
        };
        let raw_wrong: String = a[a_start..a_end].iter().collect();
        let raw_correct: String = b[b_start..b_end].iter().collect();
        let (wrong, correct) = trim_pair(&raw_wrong, &raw_correct);
        let correct = trim_likely_continuation(&wrong, &correct);
        if wrong.is_empty() || correct.is_empty() || wrong == correct {
            continue;
        }
        if wrong.chars().count() > MAX_TERM_LENGTH || correct.chars().count() > MAX_TERM_LENGTH {
            continue;
        }
        if is_only_space_or_punctuation(&wrong) && is_only_space_or_punctuation(&correct) {
            continue;
        }
        if seen.iter().any(|(w, c)| *w == wrong && *c == correct) {
            continue;
        }
        seen.push((wrong.clone(), correct.clone()));
        pairs.push((wrong, correct));
    }
    pairs
}

fn empty_store() -> Value {
    json!({"version": STORE_VERSION, "rules": [], "events": []})
}

fn default_path() -> PathBuf {
    let configured = std::env::var("CORRECTION_STORE_FILE").unwrap_or_default();
    let configured = configured.trim();
    if !configured.is_empty() {
        return paths::expand_path(configured);
    }
    paths::corrections_file()
}

pub struct CorrectionStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl CorrectionStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path: path.unwrap_or_else(default_path),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn load_unlocked(&self) -> Value {
        let Ok(text) = fs::read_to_string(&self.path) else {
            return empty_store();
        };
        let Ok(mut data) = serde_json::from_str::<Value>(&text) else {
            return empty_store();
        };
        let Some(object) = data.as_object_mut() else {
            return empty_store();
        };
        if !object.get("rules").is_some_and(Value::is_array) {
            return empty_store();
        }
        if !object.get("events").is_some_and(Value::is_array) {
            object.insert("events".to_string(), Value::Array(Vec::new()));
        }
        object.insert("version".to_string(), json!(STORE_VERSION));
        data
    }

    pub fn load(&self) -> Value {
        let _guard = self.lock();
        self.load_unlocked()
    }

    fn save_unlocked(&self, data: &Value) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建纠错词库目录 {}", parent.display()))?;
        }
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "corrections.json".to_string());
        let temporary = parent.join(format!(".{name}.{}", uuid::Uuid::new_v4().simple()));
        let write = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .with_context(|| format!("无法创建 {}", temporary.display()))?;
            let mut body = serde_json::to_string_pretty(data)?;
            body.push('\n');
            file.write_all(body.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)
                .with_context(|| format!("无法替换 {}", self.path.display()))?;
            Ok(())
        })();
        if write.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write
    }

    pub fn record(&self, wrong: &str, correct: &str) -> Result<()> {
        let (wrong, correct) = trim_pair(wrong, correct);
        if wrong.is_empty() || correct.is_empty() || wrong == correct {
            return Ok(());
        }
        let now = local_timestamp();
        let _guard = self.lock();
        let mut data = self.load_unlocked();
        let object = data.as_object_mut().expect("loaded store is an object");

        let mut found = false;
        for rule in object
            .get_mut("rules")
            .and_then(Value::as_array_mut)
            .expect("loaded store has rules")
        {
            let Some(rule) = rule.as_object_mut() else {
                continue;
            };
            if rule.get("wrong").and_then(Value::as_str) != Some(wrong.as_str())
                || rule.get("correct").and_then(Value::as_str) != Some(correct.as_str())
            {
                continue;
            }
            rule.insert(
                "count".to_string(),
                json!(count_of(rule).max(0) + 1),
            );
            rule.insert("last_seen".to_string(), json!(now));
            let enabled = rule_enabled(rule);
            rule.insert("enabled".to_string(), json!(enabled));
            found = true;
            break;
        }
        if !found {
            object
                .get_mut("rules")
                .and_then(Value::as_array_mut)
                .expect("loaded store has rules")
                .push(json!({
                    "wrong": wrong,
                    "correct": correct,
                    "count": 1,
                    "first_seen": now,
                    "last_seen": now,
                    "enabled": true,
                }));
        }

        let events = object
            .get_mut("events")
            .and_then(Value::as_array_mut)
            .expect("loaded store has events");
        events.push(json!({
            "wrong": wrong,
            "correct": correct,
            "timestamp": now,
        }));
        if events.len() > MAX_CORRECTION_EVENTS {
            events.drain(..events.len() - MAX_CORRECTION_EVENTS);
        }

        sort_rules(object.get_mut("rules").and_then(Value::as_array_mut).unwrap());
        self.save_unlocked(&data)
    }

    pub fn undo_last_record(&self) -> Result<Option<(String, String)>> {
        let _guard = self.lock();
        let mut data = self.load_unlocked();
        let object = data.as_object_mut().expect("loaded store is an object");

        let event = match object
            .get_mut("events")
            .and_then(Value::as_array_mut)
            .and_then(Vec::pop)
        {
            Some(event) => event,
            None => return Ok(None),
        };
        let wrong = text_or_empty(event.get("wrong"));
        let correct = text_or_empty(event.get("correct"));
        let last_timestamp = object
            .get("events")
            .and_then(Value::as_array)
            .and_then(|events| {
                events.iter().rev().find(|event| {
                    event.get("wrong").and_then(Value::as_str) == Some(wrong.as_str())
                        && event.get("correct").and_then(Value::as_str) == Some(correct.as_str())
                })
            })
            .and_then(|event| event.get("timestamp").cloned());

        let rules = object
            .get_mut("rules")
            .and_then(Value::as_array_mut)
            .expect("loaded store has rules");
        for index in 0..rules.len() {
            let matches = rules[index].as_object().is_some_and(|rule| {
                rule.get("wrong").and_then(Value::as_str) == Some(wrong.as_str())
                    && rule.get("correct").and_then(Value::as_str) == Some(correct.as_str())
            });
            if !matches {
                continue;
            }
            let count = (count_of(rules[index].as_object().unwrap()) - 1).max(0);
            if count == 0 {
                rules.remove(index);
            } else {
                let rule = rules[index].as_object_mut().unwrap();
                rule.insert("count".to_string(), json!(count));
                match last_timestamp {
                    Some(timestamp) => {
                        rule.insert("last_seen".to_string(), timestamp);
                    }
                    None => {
                        let first_seen = rule.get("first_seen").cloned().unwrap_or(Value::Null);
                        rule.insert("last_seen".to_string(), first_seen);
                    }
                }
            }
            break;
        }
        self.save_unlocked(&data)?;
        Ok(Some((wrong, correct)))
    }

    pub fn rules(&self) -> Vec<Map<String, Value>> {
        self.load()
            .get("rules")
            .and_then(Value::as_array)
            .map(|rules| {
                rules
                    .iter()
                    .filter_map(|rule| rule.as_object().cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn delete(&self, wrong: &str, correct: &str) -> Result<bool> {
        let _guard = self.lock();
        let mut data = self.load_unlocked();
        let object = data.as_object_mut().expect("loaded store is an object");
        let rules = object
            .get_mut("rules")
            .and_then(Value::as_array_mut)
            .expect("loaded store has rules");
        let before = rules.len();
        rules.retain(|rule| {
            !(rule.get("wrong").and_then(Value::as_str) == Some(wrong)
                && rule.get("correct").and_then(Value::as_str) == Some(correct))
        });
        let changed = rules.len() != before;
        if changed {
            if let Some(events) = object.get_mut("events").and_then(Value::as_array_mut) {
                events.retain(|event| {
                    !(event.get("wrong").and_then(Value::as_str) == Some(wrong)
                        && event.get("correct").and_then(Value::as_str) == Some(correct))
                });
            }
            self.save_unlocked(&data)?;
        }
        Ok(changed)
    }

    pub fn clear(&self) -> Result<()> {
        let _guard = self.lock();
        self.save_unlocked(&empty_store())
    }

    pub fn apply(&self, text: &str) -> String {
        let config = ApplyConfig {
            experimental: experimental_correction_learning_enabled(),
            auto_replace: env_is_true("CORRECTION_AUTO_REPLACE", "false"),
            minimum_count: parse_env_int("CORRECTION_REPLACE_MIN_COUNT", 2).max(2),
        };
        self.apply_with(text, &config)
    }

    pub fn apply_with(&self, text: &str, config: &ApplyConfig) -> String {
        if text.is_empty() || !config.experimental || !config.auto_replace {
            return text.to_string();
        }

        let mut groups: Vec<(String, Vec<Map<String, Value>>)> = Vec::new();
        for rule in self.rules() {
            let wrong = text_or_empty(rule.get("wrong"));
            let correct = text_or_empty(rule.get("correct"));
            if wrong.is_empty() || correct.is_empty() || !rule_enabled(&rule) {
                continue;
            }
            match groups.iter_mut().find(|(key, _)| *key == wrong) {
                Some((_, candidates)) => candidates.push(rule),
                None => groups.push((wrong, vec![rule])),
            }
        }

        let mut selected: Vec<(String, String)> = Vec::new();
        for (wrong, mut candidates) in groups {
            candidates.sort_by_key(|rule| std::cmp::Reverse(count_of(rule)));
            let best = &candidates[0];
            let best_count = count_of(best);
            let second_count = candidates.get(1).map(count_of).unwrap_or(0);
            if best_count >= config.minimum_count
                && best_count > second_count
                && safe_for_automatic_replacement(&wrong)
            {
                selected.push((
                    wrong,
                    text_or_empty(best.get("correct")),
                ));
            }
        }
        if selected.is_empty() {
            return text.to_string();
        }

        let mut matches: Vec<(usize, usize, String)> = Vec::new();
        for (wrong, correct) in &selected {
            if wrong.is_ascii() && wrong.chars().any(|character| character.is_ascii_alphanumeric()) {
                let pattern = format!(
                    "(?i)(?<![A-Za-z0-9_]){}(?![A-Za-z0-9_])",
                    fancy_regex::escape(wrong)
                );
                let Ok(regex) = fancy_regex::Regex::new(&pattern) else {
                    continue;
                };
                for found in regex.find_iter(text).flatten() {
                    matches.push((found.start(), found.end(), correct.clone()));
                }
            } else {
                for (start, found) in text.match_indices(wrong.as_str()) {
                    matches.push((start, start + found.len(), correct.clone()));
                }
            }
        }

        // Longest match wins at a position; edits are generated from the
        // original string so one correction can never trigger another rule.
        matches.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| (right.1 - right.0).cmp(&(left.1 - left.0)))
        });
        let mut accepted: Vec<(usize, usize, String)> = Vec::new();
        let mut cursor = 0usize;
        for (start, end, correct) in matches {
            if start < cursor {
                continue;
            }
            accepted.push((start, end, correct));
            cursor = end;
        }
        if accepted.is_empty() {
            return text.to_string();
        }

        let mut output = String::with_capacity(text.len());
        let mut cursor = 0usize;
        for (start, end, correct) in accepted {
            output.push_str(&text[cursor..start]);
            output.push_str(&correct);
            cursor = end;
        }
        output.push_str(&text[cursor..]);
        output
    }

    pub fn context_lines(&self, minimum_count: Option<i64>, limit: usize) -> Vec<String> {
        let enabled = experimental_correction_learning_enabled()
            && env_is_true("CORRECTION_CONTEXT_ENABLED", "false");
        if !enabled {
            return Vec::new();
        }
        let minimum = minimum_count
            .unwrap_or_else(|| parse_env_int("CORRECTION_CONTEXT_MIN_COUNT", 2).max(1));
        self.context_lines_with(minimum, limit)
    }

    pub fn context_lines_with(&self, minimum_count: i64, limit: usize) -> Vec<String> {
        let mut result = Vec::new();
        for rule in self.rules() {
            if !rule_enabled(&rule) || count_of(&rule) < minimum_count {
                continue;
            }
            result.push(format!(
                "{} → {}（已人工纠正 {} 次）",
                text_or_empty(rule.get("wrong")),
                text_or_empty(rule.get("correct")),
                count_of(&rule),
            ));
            if result.len() >= limit {
                break;
            }
        }
        result
    }
}

pub struct ApplyConfig {
    pub experimental: bool,
    pub auto_replace: bool,
    pub minimum_count: i64,
}

impl Default for ApplyConfig {
    fn default() -> Self {
        Self {
            experimental: true,
            auto_replace: true,
            minimum_count: 2,
        }
    }
}

pub fn safe_for_automatic_replacement(wrong: &str) -> bool {
    let normalized = wrong.trim().to_lowercase();
    if UNSAFE_SHORT_FORMS.contains(&normalized.as_str()) || normalized.chars().count() < 2 {
        return false;
    }
    if normalized.is_ascii() {
        let compact: String = normalized
            .chars()
            .filter(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
            .collect();
        return compact.chars().count() >= 3;
    }
    true
}

fn sort_rules(rules: &mut [Value]) {
    rules.sort_by(|left, right| {
        let left = left.as_object();
        let right = right.as_object();
        let left_count = left.map_or(0, count_of);
        let right_count = right.map_or(0, count_of);
        left_count
            .cmp(&right_count)
            .reverse()
            .then_with(|| {
                python_str(left.and_then(|rule| rule.get("last_seen")))
                    .cmp(&python_str(right.and_then(|rule| rule.get("last_seen"))))
            })
            .then_with(|| {
                python_str(left.and_then(|rule| rule.get("wrong")))
                    .cmp(&python_str(right.and_then(|rule| rule.get("wrong"))))
            })
    });
}

pub const OBSERVE_INITIAL_DELAY_SECONDS: f64 = 0.25;
pub const OBSERVE_POLL_SECONDS: f64 = 0.5;
pub const OBSERVE_STABLE_SECONDS: f64 = 2.0;
pub const OBSERVE_TIMEOUT_SECONDS: f64 = 45.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveStep {
    Continue,
    Stop,
    Record(Vec<(String, String)>),
}

/// Watches one inserted transcript until the user stops editing it, using the
/// same anchors and stability rule as `CorrectionLearner._observe`.
pub struct ObserveSession {
    inserted: String,
    prefix: String,
    suffix: String,
    inserted_start: usize,
    last_candidate: String,
    last_change: f64,
    observed_change: bool,
}

impl ObserveSession {
    /// `now` is the clock origin for `poll`; the caller starts it after the
    /// baseline snapshot.
    pub fn new(inserted: &str, baseline: &crate::insert::commit::TextSnapshot, now: f64) -> Option<Self> {
        if inserted.is_empty() {
            return None;
        }
        let chars: Vec<char> = baseline.text.chars().collect();
        let caret_start = baseline.selection_start.min(chars.len());
        let inserted_len = inserted.chars().count();
        let mut inserted_start = caret_start.checked_sub(inserted_len)?;
        let matches = |start: usize| -> bool {
            start + inserted_len <= chars.len()
                && chars[start..start + inserted_len].iter().collect::<String>() == inserted
        };
        if !matches(inserted_start) {
            let nearby_start = caret_start.saturating_sub(inserted_len + 8);
            let window_end = (caret_start + 1).min(chars.len());
            if nearby_start >= window_end {
                return None;
            }
            let window: String = chars[nearby_start..window_end].iter().collect();
            let position = window.rfind(inserted)?;
            let found = nearby_start + window[..position].chars().count();
            if !matches(found) {
                return None;
            }
            inserted_start = found;
        }
        let caret_start = inserted_start + inserted_len;
        let prefix: String = chars[inserted_start.saturating_sub(ANCHOR_LENGTH)..inserted_start]
            .iter()
            .collect();
        let suffix: String = chars[caret_start..(caret_start + ANCHOR_LENGTH).min(chars.len())]
            .iter()
            .collect();
        Some(Self {
            inserted: inserted.to_string(),
            prefix,
            suffix,
            inserted_start,
            last_candidate: inserted.to_string(),
            last_change: now,
            observed_change: false,
        })
    }

    pub fn poll(&mut self, snapshot: Option<&crate::insert::commit::TextSnapshot>, now: f64) -> ObserveStep {
        let Some(snapshot) = snapshot else {
            return self.finish();
        };
        if let Some(candidate) = extract_candidate(
            &snapshot.text,
            snapshot.selection_start,
            &self.prefix,
            &self.suffix,
            self.inserted_start,
        ) {
            if candidate != self.last_candidate {
                self.last_candidate = candidate;
                self.last_change = now;
                self.observed_change = self.last_candidate != self.inserted;
            }
        }
        if self.observed_change && now - self.last_change >= OBSERVE_STABLE_SECONDS {
            return self.finish();
        }
        if now >= OBSERVE_TIMEOUT_SECONDS {
            return self.finish();
        }
        ObserveStep::Continue
    }

    pub fn finish(&self) -> ObserveStep {
        if !self.observed_change || self.last_candidate == self.inserted {
            return ObserveStep::Stop;
        }
        ObserveStep::Record(extract_correction_pairs(&self.inserted, &self.last_candidate))
    }
}

/// Extraction of the edited span while the observer is still watching.
pub fn extract_candidate(
    current: &str,
    selection_start: usize,
    prefix: &str,
    suffix: &str,
    original_start: usize,
) -> Option<String> {
    let chars: Vec<char> = current.chars().collect();
    let selection_start = selection_start.min(chars.len());
    let start = if prefix.is_empty() {
        0
    } else {
        let lower = original_start.saturating_sub(ANCHOR_LENGTH * 3);
        let upper = (original_start + MAX_OBSERVED_SPAN).min(chars.len());
        if lower > upper {
            return None;
        }
        let window: String = chars[lower..upper].iter().collect();
        let position = window.rfind(prefix)?;
        lower + window[..position].chars().count() + prefix.chars().count()
    };

    let end = if !suffix.is_empty() {
        let rest: String = chars[start.min(chars.len())..].iter().collect();
        let found = rest.find(suffix)?;
        start + rest[..found].chars().count()
    } else {
        selection_start
    };
    if end < start || end - start > MAX_OBSERVED_SPAN {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempStore {
        directory: PathBuf,
        store: CorrectionStore,
    }

    impl TempStore {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "veyqa-corrections-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&directory).unwrap();
            let store = CorrectionStore::new(Some(directory.join("corrections.json")));
            Self { directory, store }
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn auto() -> ApplyConfig {
        ApplyConfig::default()
    }

    #[test]
    fn extracts_spoken_term_replacement() {
        assert_eq!(
            extract_correction_pairs("我觉得锤道需要讨论", "我觉得trade off需要讨论"),
            vec![("锤道".to_string(), "trade off".to_string())]
        );
    }

    #[test]
    fn ignores_continued_typing() {
        assert_eq!(
            extract_correction_pairs("这是语音输入", "这是语音输入，继续打字"),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn keeps_english_correction_but_not_following_continuation() {
        assert_eq!(
            extract_correction_pairs("我觉得锤道", "我觉得trade off 然后继续输入"),
            vec![("锤道".to_string(), "trade off".to_string())]
        );
    }

    #[test]
    fn ignores_punctuation_only_edits() {
        assert_eq!(
            extract_correction_pairs("你好，世界。", "你好 世界。"),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn single_observation_is_not_applied() {
        let temp = TempStore::new();
        temp.store.record("锤道", "trade off").unwrap();
        assert_eq!(
            temp.store.apply_with("这个锤道需要讨论", &auto()),
            "这个锤道需要讨论"
        );
    }

    #[test]
    fn applies_after_two_matching_observations() {
        let temp = TempStore::new();
        temp.store.record("锤道", "trade off").unwrap();
        temp.store.record("锤道", "trade off").unwrap();
        assert_eq!(
            temp.store.apply_with("这个锤道需要讨论", &auto()),
            "这个trade off需要讨论"
        );
    }

    #[test]
    fn conflicting_tie_is_not_applied() {
        let temp = TempStore::new();
        temp.store.record("锤道", "trade off").unwrap();
        temp.store.record("锤道", "trade-off").unwrap();
        assert_eq!(temp.store.apply_with("锤道", &auto()), "锤道");
    }

    #[test]
    fn higher_frequency_wins_and_enters_context() {
        let temp = TempStore::new();
        temp.store.record("锤道", "trade off").unwrap();
        temp.store.record("锤道", "trade-off").unwrap();
        temp.store.record("锤道", "trade off").unwrap();
        assert_eq!(temp.store.apply_with("锤道", &auto()), "trade off");
        assert_eq!(
            temp.store.context_lines_with(2, 100),
            vec!["锤道 → trade off（已人工纠正 2 次）".to_string()]
        );
    }

    #[test]
    fn switch_can_disable_local_replacement() {
        let temp = TempStore::new();
        temp.store.record("锤道", "trade off").unwrap();
        temp.store.record("锤道", "trade off").unwrap();
        let config = ApplyConfig {
            auto_replace: false,
            ..auto()
        };
        assert_eq!(temp.store.apply_with("锤道", &config), "锤道");
    }

    #[test]
    fn experimental_gate_disables_replacement() {
        let temp = TempStore::new();
        temp.store.record("锤道", "trade off").unwrap();
        temp.store.record("锤道", "trade off").unwrap();
        let config = ApplyConfig {
            experimental: false,
            ..auto()
        };
        assert_eq!(temp.store.apply_with("锤道", &config), "锤道");
    }

    #[test]
    fn rules_persist_across_new_store_instance() {
        let temp = TempStore::new();
        temp.store.record("锤道", "trade off").unwrap();
        temp.store.record("锤道", "trade off").unwrap();
        let reopened = CorrectionStore::new(Some(temp.store.path().to_path_buf()));
        assert_eq!(reopened.apply_with("讨论锤道", &auto()), "讨论trade off");
    }

    #[test]
    fn english_rules_respect_word_boundaries() {
        let temp = TempStore::new();
        temp.store.record("off", "of").unwrap();
        temp.store.record("off", "of").unwrap();
        assert_eq!(temp.store.apply_with("office off", &auto()), "office of");
    }

    #[test]
    fn replacements_do_not_cascade() {
        let temp = TempStore::new();
        for _ in 0..2 {
            temp.store.record("锤道", "trade off").unwrap();
            temp.store.record("trade off", "权衡").unwrap();
        }
        assert_eq!(temp.store.apply_with("锤道", &auto()), "trade off");
    }

    #[test]
    fn common_short_phrase_is_never_auto_replaced() {
        let temp = TempStore::new();
        temp.store.record("这个", "那个").unwrap();
        temp.store.record("这个", "那个").unwrap();
        assert_eq!(temp.store.apply_with("这个方案", &auto()), "这个方案");
    }

    #[test]
    fn latest_learning_event_can_be_undone() {
        let temp = TempStore::new();
        temp.store.record("锤道", "trade off").unwrap();
        temp.store.record("锤道", "trade off").unwrap();
        assert_eq!(
            temp.store.undo_last_record().unwrap(),
            Some(("锤道".to_string(), "trade off".to_string()))
        );
        assert_eq!(temp.store.rules()[0]["count"], json!(1));
        assert_eq!(temp.store.apply_with("锤道", &auto()), "锤道");
    }

    #[test]
    fn store_uses_v2_schema_and_caps_events() {
        let temp = TempStore::new();
        for _ in 0..(MAX_CORRECTION_EVENTS + 25) {
            temp.store.record("锤道", "trade off").unwrap();
        }
        let data = temp.store.load();
        assert_eq!(data["version"], json!(STORE_VERSION));
        assert_eq!(
            data["events"].as_array().unwrap().len(),
            MAX_CORRECTION_EVENTS
        );
        assert_eq!(data["rules"][0]["count"], json!(MAX_CORRECTION_EVENTS + 25));
        let mode = fs::metadata(temp.store.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn unsafe_short_and_english_fragments_are_never_replaced() {
        assert!(!safe_for_automatic_replacement("这个"));
        assert!(!safe_for_automatic_replacement("a"));
        assert!(!safe_for_automatic_replacement("of"));
        assert!(safe_for_automatic_replacement("off"));
        assert!(safe_for_automatic_replacement("tool"));
        assert!(safe_for_automatic_replacement("锤道"));
    }

    #[test]
    fn longest_match_wins_without_overlap() {
        let temp = TempStore::new();
        for _ in 0..2 {
            temp.store.record("锤道", "trade off").unwrap();
            temp.store.record("锤", "hammer").unwrap();
        }
        assert_eq!(temp.store.apply_with("锤道", &auto()), "trade off");
    }

    #[test]
    fn delete_removes_rule_and_events() {
        let temp = TempStore::new();
        temp.store.record("锤道", "trade off").unwrap();
        assert!(temp.store.delete("锤道", "trade off").unwrap());
        assert!(temp.store.rules().is_empty());
        assert_eq!(temp.store.load()["events"], json!([]));
        assert!(!temp.store.delete("锤道", "trade off").unwrap());
    }

    #[test]
    fn extract_candidate_uses_anchor_and_suffix() {
        let current = "前面我觉得trade off后面";
        let candidate = extract_candidate(current, 13, "我觉得", "后面", 5).unwrap();
        assert_eq!(candidate, "trade off");
        assert_eq!(extract_candidate(current, 13, "缺失", "后面", 5), None);
    }

    fn snapshot(text: &str, caret: usize) -> crate::insert::commit::TextSnapshot {
        crate::insert::commit::TextSnapshot {
            text: text.to_string(),
            selection_start: caret,
            selection_end: caret,
        }
    }

    #[test]
    fn observe_session_records_a_stable_correction() {
        let baseline = snapshot("这是语音输入测试", "这是".chars().count() + 4);
        let mut session = ObserveSession::new("语音输入", &baseline, 0.0).unwrap();
        let edited = snapshot("这是语音书入测试", "这是".chars().count() + 4);
        assert_eq!(session.poll(Some(&edited), 0.5), ObserveStep::Continue);
        assert_eq!(session.poll(Some(&edited), 2.0), ObserveStep::Continue);
        assert_eq!(
            session.poll(Some(&edited), 2.6),
            ObserveStep::Record(vec![("输".to_string(), "书".to_string())])
        );
    }

    #[test]
    fn observe_session_stops_without_an_edit() {
        let baseline = snapshot("前面语音后面", 4);
        let mut session = ObserveSession::new("语音", &baseline, 0.0).unwrap();
        assert_eq!(session.poll(Some(&baseline), 0.5), ObserveStep::Continue);
        assert_eq!(session.finish(), ObserveStep::Stop);
    }

    #[test]
    fn observe_session_finds_text_that_shifted_slightly() {
        let baseline = snapshot("前面  语音后面", 6);
        let mut session = ObserveSession::new("语音", &baseline, 0.0).unwrap();
        assert_eq!(
            session.poll(Some(&snapshot("前面  语音后面", 6)), 3.0),
            ObserveStep::Continue
        );
    }

    #[test]
    fn observe_session_times_out_after_forty_five_seconds() {
        let baseline = snapshot("语音", 2);
        let mut session = ObserveSession::new("语音", &baseline, 0.0).unwrap();
        let edited = snapshot("语言", 2);
        assert_eq!(session.poll(Some(&edited), 1.0), ObserveStep::Continue);
        assert_eq!(
            session.poll(Some(&edited), OBSERVE_TIMEOUT_SECONDS),
            ObserveStep::Record(vec![("音".to_string(), "言".to_string())])
        );
    }
}
