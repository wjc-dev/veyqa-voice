use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn init() -> Result<()> {
    let path = config_path();
    if !path.is_file() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("无法读取配置 {}", path.display()))?;

    // python-dotenv resolves ${VAR} against the process environment plus the
    // values parsed so far, and the engine mirrors load_dotenv(override=True).
    let mut resolved: HashMap<String, String> = std::env::vars().collect();
    let mut parsed: Vec<(String, String)> = Vec::new();
    for (key, value) in parse_env(&content) {
        let value = interpolate(&value, &resolved);
        resolved.insert(key.clone(), value.clone());
        parsed.push((key, value));
    }
    for (key, value) in parsed {
        // SAFETY: config::init runs before any engine threads and is not called
        // concurrently; std::env::set_var requires that on some platforms.
        unsafe { std::env::set_var(key, value) };
    }
    Ok(())
}

pub fn home_dir() -> PathBuf {
    crate::paths::home_dir()
}

pub fn data_dir() -> PathBuf {
    crate::paths::data_dir()
}

pub fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("VEYQA_CONFIG_FILE") {
        return crate::paths::expand_path(&path.to_string_lossy());
    }
    crate::paths::env_file()
}

pub fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_string()
}

pub fn var_or(name: &str, default: &str) -> String {
    let value = var(name);
    if value.is_empty() {
        default.to_string()
    } else {
        value
    }
}

pub fn bool_var(name: &str, default: bool) -> bool {
    match var(name).to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => true,
        "false" | "0" | "no" | "off" => false,
        _ => default,
    }
}

pub fn f32_var(name: &str, default: f32) -> f32 {
    var(name).parse().unwrap_or(default)
}

fn is_horizontal_whitespace(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn skip_horizontal(text: &str, mut pos: usize) -> usize {
    while let Some(c) = text[pos..].chars().next() {
        if !is_horizontal_whitespace(c) {
            break;
        }
        pos += c.len_utf8();
    }
    pos
}

fn skip_to_line_end(text: &str, mut pos: usize) -> usize {
    while let Some(c) = text[pos..].chars().next() {
        if c == '\n' || c == '\r' {
            break;
        }
        pos += c.len_utf8();
    }
    pos
}

fn skip_line_break(text: &str, pos: usize) -> usize {
    if text[pos..].starts_with("\r\n") {
        return pos + 2;
    }
    if let Some(c) = text[pos..].chars().next() {
        if c == '\n' || c == '\r' {
            return pos + c.len_utf8();
        }
    }
    pos
}

/// Parse a `.env` body with python-dotenv semantics (the loader the Python
/// engine uses). Unquoted values may contain spaces; only `'` / `"` values
/// strip surrounding quotes; a `#` only starts a comment after whitespace.
pub fn parse_env(text: &str) -> Vec<(String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut bindings = Vec::new();
    let mut pos = 0;

    while pos < text.len() {
        while let Some(c) = text[pos..].chars().next() {
            if !c.is_whitespace() {
                break;
            }
            pos += c.len_utf8();
        }
        if pos >= text.len() {
            break;
        }

        let line_start = pos;
        if text[pos..].starts_with("export")
            && text[pos + 6..]
                .chars()
                .next()
                .is_some_and(|c| is_horizontal_whitespace(c) || c == '\n' || c == '\r')
        {
            pos += 6;
            pos = skip_horizontal(text, pos);
        }

        if text[pos..].starts_with('#') {
            pos = skip_to_line_end(text, pos);
            pos = skip_line_break(text, pos);
            continue;
        }

        let key = if text[pos..].starts_with('\'') {
            match text[pos + 1..].find('\'') {
                Some(offset) => {
                    let key = text[pos + 1..pos + 1 + offset].to_string();
                    pos += 1 + offset + 1;
                    if key.is_empty() {
                        pos = skip_to_line_end(text, pos);
                        pos = skip_line_break(text, pos);
                        continue;
                    }
                    key
                }
                None => {
                    pos = skip_to_line_end(text, pos);
                    pos = skip_line_break(text, pos);
                    continue;
                }
            }
        } else {
            let end = text[pos..]
                .find(|c: char| c == '=' || c == '#' || c.is_whitespace())
                .map(|offset| pos + offset)
                .unwrap_or(text.len());
            let key = text[pos..end].to_string();
            pos = end;
            if key.is_empty() {
                pos = skip_to_line_end(text, pos);
                pos = skip_line_break(text, pos);
                continue;
            }
            key
        };

        pos = skip_horizontal(text, pos);
        if !text[pos..].starts_with('=') {
            // python-dotenv keeps a bare key as None and never exports it.
            pos = skip_to_line_end(text, pos);
            pos = skip_line_break(text, pos);
            let _ = line_start;
            continue;
        }
        pos += 1;
        pos = skip_horizontal(text, pos);

        let (value, next) = parse_value(text, pos);
        pos = next;

        pos = skip_horizontal(text, pos);
        if text[pos..].starts_with('#') {
            pos = skip_to_line_end(text, pos);
            pos = skip_line_break(text, pos);
        } else {
            let at_end = pos >= text.len()
                || text[pos..].starts_with('\n')
                || text[pos..].starts_with('\r');
            if at_end {
                pos = skip_line_break(text, pos);
            } else {
                // Anything else after a quoted value makes python-dotenv drop
                // the binding; skip it instead of failing the whole file.
                pos = skip_to_line_end(text, pos);
                pos = skip_line_break(text, pos);
                continue;
            }
        }

        bindings.push((key, value));
    }

    bindings
}

fn parse_value(text: &str, pos: usize) -> (String, usize) {
    match text[pos..].chars().next() {
        None => (String::new(), pos),
        Some(c) if c == '\n' || c == '\r' => (String::new(), pos),
        Some('\'') => {
            let body_start = pos + 1;
            let mut scan = body_start;
            while scan < text.len() {
                let c = text[scan..].chars().next().unwrap();
                if c == '\\' {
                    let next = scan + 1;
                    if next < text.len() {
                        let escaped = text[next..].chars().next().unwrap();
                        if escaped == '\\' || escaped == '\'' {
                            scan = next + escaped.len_utf8();
                            continue;
                        }
                    }
                    scan = next;
                    continue;
                }
                if c == '\'' {
                    let raw = &text[body_start..scan];
                    return (decode_single_quoted(raw), scan + 1);
                }
                scan += c.len_utf8();
            }
            (text[body_start..].to_string(), text.len())
        }
        Some('"') => {
            let body_start = pos + 1;
            let mut scan = body_start;
            while scan < text.len() {
                let c = text[scan..].chars().next().unwrap();
                if c == '\\' {
                    let next = scan + 1;
                    if next < text.len() {
                        let escaped = text[next..].chars().next().unwrap();
                        if escaped == '\\' || escaped == '"' {
                            scan = next + escaped.len_utf8();
                            continue;
                        }
                    }
                    scan = next;
                    continue;
                }
                if c == '"' {
                    let raw = &text[body_start..scan];
                    return (decode_double_quoted(raw), scan + 1);
                }
                scan += c.len_utf8();
            }
            (text[body_start..].to_string(), text.len())
        }
        Some(_) => {
            let end = text[pos..]
                .find(|c: char| c == '\n' || c == '\r')
                .map(|offset| pos + offset)
                .unwrap_or(text.len());
            let raw = &text[pos..end];
            (strip_inline_comment(raw).trim_end().to_string(), end)
        }
    }
}

fn strip_inline_comment(raw: &str) -> &str {
    let mut previous_whitespace = false;
    for (index, c) in raw.char_indices() {
        if c == '#' && previous_whitespace {
            return &raw[..index];
        }
        previous_whitespace = c.is_whitespace();
    }
    raw
}

fn decode_single_quoted(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            output.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => output.push('\\'),
            Some('\'') => output.push('\''),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    output
}

fn decode_double_quoted(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            output.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => output.push('\\'),
            Some('\'') => output.push('\''),
            Some('"') => output.push('"'),
            Some('a') => output.push('\u{7}'),
            Some('b') => output.push('\u{8}'),
            Some('f') => output.push('\u{c}'),
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('v') => output.push('\u{b}'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    output
}

fn interpolate(value: &str, env: &HashMap<String, String>) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let expression = &after[..end];
        let (name, default) = match expression.find(":-") {
            Some(index) => (&expression[..index], Some(&expression[index + 2..])),
            None => (expression, None),
        };
        match env.get(name) {
            Some(found) => output.push_str(found),
            None => output.push_str(default.unwrap_or("")),
        }
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(text: &str) -> Vec<(String, String)> {
        parse_env(text)
    }

    #[test]
    fn keeps_unquoted_paths_with_spaces() {
        let parsed = pairs("QWEN_CONTEXT_FILE=/Users/me/Library/Application Support/Veyqa/personal_context.txt\n");
        assert_eq!(
            parsed,
            vec![(
                "QWEN_CONTEXT_FILE".to_string(),
                "/Users/me/Library/Application Support/Veyqa/personal_context.txt".to_string()
            )]
        );
    }

    #[test]
    fn strips_quotes_and_escapes() {
        let parsed = pairs("A=\"x y\"\nB='a b'\nC=\"line\\nnext\"\nD='it\\'s'\n");
        assert_eq!(parsed[0].1, "x y");
        assert_eq!(parsed[1].1, "a b");
        assert_eq!(parsed[2].1, "line\nnext");
        assert_eq!(parsed[3].1, "it's");
    }

    #[test]
    fn inline_comments_need_whitespace() {
        let parsed = pairs("A=value # note\nB=value#not-comment\nC=  spaced  \n");
        assert_eq!(parsed[0].1, "value");
        assert_eq!(parsed[1].1, "value#not-comment");
        assert_eq!(parsed[2].1, "spaced");
    }

    #[test]
    fn supports_export_and_comments() {
        let parsed = pairs("# comment\nexport FOO=1\n\nBAR=2\n");
        assert_eq!(
            parsed,
            vec![
                ("FOO".to_string(), "1".to_string()),
                ("BAR".to_string(), "2".to_string())
            ]
        );
    }

    #[test]
    fn interpolates_posix_variables() {
        let mut env = HashMap::new();
        env.insert("HOME".to_string(), "/Users/me".to_string());
        assert_eq!(interpolate("${HOME}/x", &env), "/Users/me/x");
        assert_eq!(interpolate("${MISSING:-fallback}", &env), "fallback");
        assert_eq!(interpolate("${MISSING}", &env), "");
        assert_eq!(interpolate("$HOME/x", &env), "$HOME/x");
    }

    #[test]
    fn multiline_double_quoted_value() {
        let parsed = pairs("A=\"first\nsecond\"\nB=2\n");
        assert_eq!(parsed[0].1, "first\nsecond");
        assert_eq!(parsed[1].1, "2");
    }

    #[test]
    fn bare_key_is_not_exported() {
        let parsed = pairs("BARE\nA=1\n");
        assert_eq!(parsed, vec![("A".to_string(), "1".to_string())]);
    }
}
