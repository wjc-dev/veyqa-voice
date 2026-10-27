use std::fmt;
use std::sync::OnceLock;

use fancy_regex::{Captures, Regex, Replacer};

const PUNCTUATION: &str = r#"！？；：、…—（）【】〔〕《》〈〉「」『』“”‘’·,.!?;:"()[]{}<>~～，。"#;
const FILLER: &str = r"(?:嗯+|呃+|额+|唔+|啊+|呐+)";
const BOUNDARY: &str = r"[\s，。！？；：、,.!?;:]+";
const NUMERIC_PLACEHOLDERS: [(char, char); 3] =
    [('.', '\u{e000}'), (',', '\u{e001}'), (':', '\u{e002}')];

struct Patterns {
    inline_breaks: Regex,
    unsafe_controls: Regex,
    spaces: Regex,
    filler_between: Regex,
    filler_tokens: Regex,
    filler_prefix: Regex,
    stutter_function: Regex,
    stutter_phrase: Regex,
    immediate_repetition: Regex,
    horizontal_spaces: Regex,
    line_spaces: Regex,
    comparable: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| Patterns {
        inline_breaks: Regex::new(r"[\r\n\t\v\f\u{2028}\u{2029}]+").expect("inline breaks"),
        unsafe_controls: Regex::new(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]+")
            .expect("unsafe controls"),
        spaces: Regex::new(r"[ \u{00a0}]+").expect("spaces"),
        filler_between: Regex::new(&format!(
            r"([^\s，。！？；：、,.!?;:]{{2,16}}){BOUNDARY}{FILLER}{BOUNDARY}\1"
        ))
        .expect("filler between"),
        filler_tokens: Regex::new(&format!(r"(^|{BOUNDARY}){FILLER}(?=$|{BOUNDARY})"))
            .expect("filler tokens"),
        filler_prefix: Regex::new(&format!(r"^{FILLER}(?=[\u3400-\u9fffA-Za-z0-9])"))
            .expect("filler prefix"),
        stutter_function: Regex::new(r"([我你他她它这那就是不有要会能想说的喂])\1{2,}")
            .expect("stutter function"),
        stutter_phrase: Regex::new(&format!(
            r"(?<![A-Za-z0-9_])([^\s，。！？；：、,.!?;:]{{2,24}})(?:{BOUNDARY}\1){{2,}}"
        ))
        .expect("stutter phrase"),
        immediate_repetition: Regex::new(r"(.{2,12}?)(?:\1){2,}").expect("immediate repetition"),
        horizontal_spaces: Regex::new(r"[ \t]+").expect("horizontal spaces"),
        line_spaces: Regex::new(r" *\n *").expect("line spaces"),
        comparable: Regex::new(r"[^\w\u3400-\u9fff]+").expect("comparable"),
    })
}

fn replace_all<R: Replacer>(regex: &Regex, text: &str, replacement: R) -> String {
    regex
        .try_replacen(text, 0, replacement)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| text.to_string())
}

pub fn sanitize_inline_text(text: &str) -> String {
    let patterns = patterns();
    let text = replace_all(&patterns.inline_breaks, text, " ");
    let text = replace_all(&patterns.unsafe_controls, &text, "");
    replace_all(&patterns.spaces, &text, " ").trim().to_string()
}

fn collapse_adjacent_repetitions(text: &str) -> String {
    let patterns = patterns();
    let mut current = text.to_string();
    loop {
        let next = replace_all(&patterns.stutter_function, &current, "$1");
        let next = replace_all(&patterns.stutter_phrase, &next, "$1");
        let next = replace_all(&patterns.immediate_repetition, &next, "$1");
        if next == current {
            return current;
        }
        current = next;
    }
}

pub fn clean_spoken_disfluencies(
    text: &str,
    enabled: Option<bool>,
    mode: Option<&str>,
) -> String {
    let text = text.trim();
    let mut resolved_mode = mode.map(|value| value.trim().to_ascii_lowercase());
    if mode.is_none() && enabled.is_none() {
        resolved_mode = Some(
            std::env::var("DISFLUENCY_FILTER_MODE")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase(),
        );
    }
    if resolved_mode.as_deref().unwrap_or("").is_empty() {
        let enabled = enabled.unwrap_or_else(|| {
            std::env::var("DISFLUENCY_FILTER_ENABLED")
                .unwrap_or_else(|_| "false".to_string())
                .to_ascii_lowercase()
                == "true"
        });
        resolved_mode = Some(if enabled { "conservative" } else { "off" }.to_string());
    }
    let mode = match resolved_mode.as_deref() {
        Some("off") | Some("fillers") | Some("conservative") => {
            resolved_mode.as_deref().unwrap_or("off")
        }
        _ => "off",
    };
    if text.is_empty() || mode == "off" {
        return text.to_string();
    }

    let patterns = patterns();
    let mut result = replace_all(&patterns.filler_between, text, "$1");
    result = replace_all(&patterns.filler_tokens, &result, |captures: &Captures<'_, str>| {
        if captures.get(0).is_some_and(|item| item.start() == 0) {
            String::new()
        } else {
            " ".to_string()
        }
    });
    result = replace_all(&patterns.filler_prefix, &result, "");
    if mode == "conservative" {
        result = collapse_adjacent_repetitions(&result);
    }
    let result = replace_all(&patterns.horizontal_spaces, &result, " ");
    let result = replace_all(&patterns.line_spaces, &result, "\n");
    result.trim().to_string()
}

pub fn normalize_for_comparison(text: &str) -> String {
    replace_all(&patterns().comparable, &text.to_lowercase(), "")
}

pub fn is_context_echo(transcript: &str, context: &str) -> bool {
    let candidate = normalize_for_comparison(transcript);
    let source = normalize_for_comparison(context);
    let candidate_len = candidate.chars().count();
    let source_len = source.chars().count();
    if candidate_len < 24 || source_len < 24 {
        return false;
    }
    if source.contains(&candidate) && candidate_len >= 32 {
        return true;
    }
    if candidate.contains(&source) && source_len >= 32 {
        return true;
    }
    if candidate_len >= 48 {
        return sequence_ratio(&candidate, &source) >= 0.78;
    }
    false
}

fn sequence_ratio(first: &str, second: &str) -> f64 {
    let a: Vec<char> = first.chars().collect();
    let b: Vec<char> = second.chars().collect();
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    2.0 * matching_characters(&a, &b, 0, a.len(), 0, b.len()) as f64 / total as f64
}

fn matching_characters(
    a: &[char],
    b: &[char],
    a_low: usize,
    a_high: usize,
    b_low: usize,
    b_high: usize,
) -> usize {
    let (a_start, b_start, size) = longest_match(a, b, a_low, a_high, b_low, b_high);
    if size == 0 {
        return 0;
    }
    size + matching_characters(a, b, a_low, a_start, b_low, b_start)
        + matching_characters(
            a,
            b,
            a_start + size,
            a_high,
            b_start + size,
            b_high,
        )
}

pub(crate) fn longest_match(
    a: &[char],
    b: &[char],
    a_low: usize,
    a_high: usize,
    b_low: usize,
    b_high: usize,
) -> (usize, usize, usize) {
    let rows = a_high - a_low;
    let cols = b_high - b_low;
    let mut best = (a_low, b_low, 0);
    if rows == 0 || cols == 0 {
        return best;
    }

    let mut table = vec![vec![0usize; cols + 1]; rows + 1];
    for i in (0..rows).rev() {
        for j in (0..cols).rev() {
            if a[a_low + i] == b[b_low + j] {
                let size = table[i + 1][j + 1] + 1;
                table[i][j] = size;
                let candidate = (a_low + i, b_low + j, size);
                if candidate.2 > best.2
                    || (candidate.2 == best.2
                        && (candidate.0 < best.0
                            || (candidate.0 == best.0 && candidate.1 < best.1)))
                {
                    best = candidate;
                }
            }
        }
    }
    best
}

pub fn has_sufficient_speech(total_audio_ms: f64, voiced_audio_ms: f64) -> bool {
    has_sufficient_speech_with(total_audio_ms, voiced_audio_ms, 280.0, 180.0)
}

pub fn has_sufficient_speech_with(
    total_audio_ms: f64,
    voiced_audio_ms: f64,
    minimum_total_ms: f64,
    minimum_voiced_ms: f64,
) -> bool {
    total_audio_ms >= minimum_total_ms && voiced_audio_ms >= minimum_voiced_ms
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedPunctuationMode(pub String);

impl fmt::Display for UnsupportedPunctuationMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Unsupported punctuation mode: {}", self.0)
    }
}

impl std::error::Error for UnsupportedPunctuationMode {}

fn protect_numeric_separators(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    for (index, character) in chars.iter().enumerate() {
        let surrounded_by_digits = index > 0
            && index + 1 < chars.len()
            && chars[index - 1].is_numeric()
            && chars[index + 1].is_numeric();
        if surrounded_by_digits {
            if let Some((_, placeholder)) = NUMERIC_PLACEHOLDERS
                .iter()
                .find(|(separator, _)| separator == character)
            {
                result.push(*placeholder);
                continue;
            }
        }
        result.push(*character);
    }
    result
}

fn restore_numeric_separators(text: &str) -> String {
    let mut result = text.to_string();
    for (separator, placeholder) in NUMERIC_PLACEHOLDERS {
        result = result.replace(placeholder, &separator.to_string());
    }
    result
}

pub fn format_transcription_text(
    text: &str,
    mode: &str,
) -> Result<String, UnsupportedPunctuationMode> {
    let text = text.trim();
    let mode = mode.trim().to_ascii_lowercase();
    if mode == "auto" {
        return Ok(text.to_string());
    }
    if mode != "spaces" && mode != "none" {
        return Err(UnsupportedPunctuationMode(mode));
    }

    let protected = protect_numeric_separators(text);
    let mut formatted = String::with_capacity(protected.len());
    for character in protected.chars() {
        if mode == "spaces" {
            if character == '？' || character == '?' {
                formatted.push(character);
            } else if PUNCTUATION.contains(character) {
                formatted.push(' ');
            } else {
                formatted.push(character);
            }
        } else if !PUNCTUATION.contains(character) {
            formatted.push(character);
        }
    }

    let patterns = patterns();
    if mode == "spaces" {
        formatted = replace_all(&patterns.horizontal_spaces, &formatted, " ");
        formatted = replace_all(&patterns.line_spaces, &formatted, "\n");
    }
    Ok(restore_numeric_separators(&formatted).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_isolated_fillers() {
        assert_eq!(
            clean_spoken_disfluencies("嗯 而且 呃 我觉得这个方案可以", Some(true), None),
            "而且 我觉得这个方案可以"
        );
    }

    #[test]
    fn removes_filler_between_repeated_phrase() {
        assert_eq!(
            clean_spoken_disfluencies("而且 嗯 而且我发现它有问题", Some(true), None),
            "而且我发现它有问题"
        );
    }

    #[test]
    fn collapses_clear_stutter_but_keeps_single_character_words() {
        assert_eq!(
            clean_spoken_disfluencies("你好你好你好", Some(true), None),
            "你好"
        );
        assert_eq!(
            clean_spoken_disfluencies("喂喂喂 我我我觉得", Some(true), None),
            "喂 我觉得"
        );
        assert_eq!(
            clean_spoken_disfluencies("人人都可以常常看看", Some(true), None),
            "人人都可以常常看看"
        );
    }

    #[test]
    fn filter_can_be_disabled() {
        assert_eq!(
            clean_spoken_disfluencies("嗯 你好你好", Some(false), None),
            "嗯 你好你好"
        );
    }

    #[test]
    fn fillers_mode_does_not_rewrite_repeated_words() {
        assert_eq!(
            clean_spoken_disfluencies("嗯 你好你好你好", None, Some("fillers")),
            "你好你好你好"
        );
    }

    #[test]
    fn cleanup_composes_with_space_punctuation_mode() {
        let formatted = format_transcription_text("嗯，我觉得，嗯，这个可以？", "spaces").unwrap();
        assert_eq!(
            clean_spoken_disfluencies(&formatted, Some(true), None),
            "我觉得 这个可以？"
        );
    }

    #[test]
    fn keeps_meaningful_discourse_words_and_repeated_emphasis() {
        assert_eq!(
            clean_spoken_disfluencies("然后这个方案就是非常非常重要", Some(true), None),
            "然后这个方案就是非常非常重要"
        );
        assert_eq!(
            clean_spoken_disfluencies("哈哈哈 666", Some(true), None),
            "哈哈哈 666"
        );
    }

    #[test]
    fn line_breaks_can_never_become_chat_submit_events() {
        assert_eq!(
            sanitize_inline_text("第一行\r\n第二行\u{2028}第三行\t结束"),
            "第一行 第二行 第三行 结束"
        );
    }

    #[test]
    fn strips_control_characters() {
        assert_eq!(sanitize_inline_text("\u{0}abc\u{1f}"), "abc");
    }

    #[test]
    fn blocks_long_context_echo() {
        let context = "用户从事软件开发。常用术语：LightGBM、trade-off、特征工程。近期主题：示例项目。";
        assert!(is_context_echo(context, context));
        assert!(is_context_echo(
            "用户从事软件开发 常用术语 LightGBM trade-off 特征工程",
            context
        ));
    }

    #[test]
    fn allows_short_real_vocabulary_utterance() {
        let context = "用户从事软件开发。常用术语：LightGBM、trade-off、特征工程。近期主题：示例项目。";
        assert!(!is_context_echo("这个 LightGBM 模型怎么样", context));
    }

    #[test]
    fn silence_and_short_tap_do_not_open_speech_gate() {
        assert!(!has_sufficient_speech(1.3, 0.0));
        assert!(!has_sufficient_speech(500.0, 80.0));
        assert!(has_sufficient_speech(400.0, 200.0));
    }

    #[test]
    fn spaces_mode_preserves_numeric_separators_and_question_marks() {
        assert_eq!(
            format_transcription_text("3.14 1,000 10:30 你好。再见？OK!", "spaces").unwrap(),
            "3.14 1,000 10:30 你好 再见？OK"
        );
    }

    #[test]
    fn none_mode_removes_punctuation_but_keeps_numbers() {
        assert_eq!(
            format_transcription_text("你好，世界！3.14 与 1,000", "none").unwrap(),
            "你好世界3.14 与 1,000"
        );
    }

    #[test]
    fn rejects_unknown_punctuation_mode() {
        assert!(format_transcription_text("你好", "hyper").is_err());
    }

    #[test]
    fn normalizes_for_comparison() {
        assert_eq!(
            normalize_for_comparison("Hello, 世界! 3.14"),
            "hello世界314"
        );
    }
}
