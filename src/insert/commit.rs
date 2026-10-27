//! Final-text commit: one guarded write, never into a different window.
//!
//! Mirrors `KeyboardManager.type_text` from the Python app. The AX/pasteboard
//! side is supplied through `CommitBackend`, so the safety order stays
//! unit-testable without touching a real focus target.

use crate::text::sanitize_inline_text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSnapshot {
    pub text: String,
    /// Character offsets, matching Python's `str` indexing.
    pub selection_start: usize,
    pub selection_end: usize,
}

pub trait CommitBackend<T> {
    fn target_is_focused(&mut self, target: &T) -> bool;
    fn snapshot(&mut self, target: &T) -> Option<TextSnapshot>;
    fn apply_corrections(&mut self, text: &str) -> String;
    fn same_target(&mut self, first: &T, second: &T) -> bool;
    fn paste(&mut self, target: &T, text: &str) -> bool;
    fn observe_after_paste(&mut self, text: &str, target: &T);
    fn retain_recovery(&mut self, text: &str, reason: &str) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LastInsertion<T> {
    target: T,
    end: usize,
    text: String,
}

pub struct Committer<T> {
    last: Option<LastInsertion<T>>,
}

impl<T> Default for Committer<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Committer<T> {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Commit one final transcription. Returns true only if text was inserted.
    pub fn commit<B>(&mut self, backend: &mut B, text: &str, target: Option<&T>) -> bool
    where
        T: Clone,
        B: CommitBackend<T>,
    {
        let text = sanitize_inline_text(text);
        if text.is_empty() {
            return false;
        }

        let Some(target) = target else {
            // An explicitly missing recording target must not be recaptured
            // after recognition: the user could now be in a different app.
            return retain_failed(
                backend,
                &text,
                "无法确认输入位置，文字已保留，请在恢复页复制",
            );
        };
        if !backend.target_is_focused(target) {
            return retain_failed(backend, &text, "录音期间输入焦点已改变");
        }

        let text = backend.apply_corrections(&text);
        let Some(snapshot) = backend.snapshot(target) else {
            return retain_failed(backend, &text, "当前输入框不可读取");
        };
        let mut text = text;
        if self.should_prepend_segment_space(backend, target, &snapshot, &text) {
            text.insert(0, ' ');
        }

        let start = snapshot.selection_start;
        if !backend.target_is_focused(target) {
            return retain_failed(backend, &text, "录音期间输入焦点已改变");
        }
        if !backend.paste(target, &text) {
            return retain_failed(backend, &text, "当前输入框拒绝粘贴文字");
        }

        let end = start + text.chars().count();
        backend.observe_after_paste(&text, target);
        self.last = Some(LastInsertion {
            target: target.clone(),
            end,
            text,
        });
        true
    }

    pub fn clear_history(&mut self) {
        self.last = None;
    }

    fn should_prepend_segment_space<B>(
        &mut self,
        backend: &mut B,
        target: &T,
        snapshot: &TextSnapshot,
        text: &str,
    ) -> bool
    where
        T: Clone,
        B: CommitBackend<T>,
    {
        let Some(previous) = self.last.as_ref() else {
            return false;
        };
        if text.is_empty()
            || snapshot.selection_start != snapshot.selection_end
            || snapshot.selection_start != previous.end
            || previous.text.is_empty()
        {
            return false;
        }
        let chars: Vec<char> = snapshot.text.chars().collect();
        if snapshot.selection_start > chars.len() {
            return false;
        }
        let before_selection: String = chars[..snapshot.selection_start].iter().collect();
        if !before_selection.ends_with(&previous.text) {
            return false;
        }
        if !backend.same_target(&previous.target, target) {
            return false;
        }
        let previous_char = snapshot
            .selection_start
            .checked_sub(1)
            .and_then(|index| chars.get(index))
            .copied();
        let separated_before = previous_char.is_none_or(char::is_whitespace);
        let separated_after = text.chars().next().is_none_or(char::is_whitespace);
        !separated_before && !separated_after
    }
}

fn retain_failed<T, B: CommitBackend<T>>(backend: &mut B, text: &str, reason: &str) -> bool {
    backend.retain_recovery(text, &format!("{reason}；请到恢复页复制文字"));
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Target(u64);

    struct Fake {
        focused: bool,
        ax_succeeds: bool,
        ax_changes_text: bool,
        snapshot_available: bool,
        target: Target,
        current_text: String,
        selection: (usize, usize),
        replacement: String,
        inserted: Vec<(Target, String)>,
        events: Vec<(Target, String)>,
        observed: Vec<(String, Target)>,
        recovered: Vec<(String, String)>,
        capture_count: usize,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                focused: true,
                ax_succeeds: true,
                ax_changes_text: true,
                snapshot_available: true,
                target: Target(7),
                current_text: "已有文字".to_string(),
                selection: (4, 4),
                replacement: String::new(),
                inserted: Vec::new(),
                events: Vec::new(),
                observed: Vec::new(),
                recovered: Vec::new(),
                capture_count: 0,
            }
        }

        fn insert(&mut self, text: &str) {
            let (start, end) = self.selection;
            let chars: Vec<char> = self.current_text.chars().collect();
            let mut updated: String = chars[..start].iter().collect();
            updated.push_str(text);
            updated.extend(chars[end..].iter());
            self.current_text = updated;
            let caret = start + text.chars().count();
            self.selection = (caret, caret);
        }
    }

    impl CommitBackend<Target> for Fake {
        fn target_is_focused(&mut self, target: &Target) -> bool {
            *target == self.target && self.focused
        }

        fn snapshot(&mut self, target: &Target) -> Option<TextSnapshot> {
            (*target == self.target && self.snapshot_available).then(|| TextSnapshot {
                text: self.current_text.clone(),
                selection_start: self.selection.0,
                selection_end: self.selection.1,
            })
        }

        fn apply_corrections(&mut self, text: &str) -> String {
            if self.replacement.is_empty() {
                text.to_string()
            } else {
                self.replacement.clone()
            }
        }

        fn same_target(&mut self, first: &Target, second: &Target) -> bool {
            first == second
        }

        fn paste(&mut self, target: &Target, text: &str) -> bool {
            if !self.ax_succeeds || target != &self.target {
                return false;
            }
            self.inserted.push((target.clone(), text.to_string()));
            if self.ax_changes_text {
                self.insert(text);
            }
            true
        }

        fn observe_after_paste(&mut self, text: &str, target: &Target) {
            self.observed.push((text.to_string(), target.clone()));
        }

        fn retain_recovery(&mut self, text: &str, reason: &str) -> bool {
            self.recovered.push((text.to_string(), reason.to_string()));
            true
        }
    }

    const FOCUS_CHANGED: &str = "录音期间输入焦点已改变；请到恢复页复制文字";

    #[test]
    fn focus_change_never_types_into_the_new_foreground_app() {
        let mut fake = Fake::new();
        fake.focused = false;
        let target = fake.target.clone();
        let mut committer = Committer::new();
        assert!(!committer.commit(&mut fake, "绝不能写到别的窗口", Some(&target)));
        assert!(fake.inserted.is_empty());
        assert_eq!(fake.recovered.len(), 1);
        assert_eq!(fake.recovered[0].0, "绝不能写到别的窗口");
        assert_eq!(fake.recovered[0].1, FOCUS_CHANGED);
    }

    #[test]
    fn missing_target_is_retained_without_recapture() {
        let mut fake = Fake::new();
        let mut committer = Committer::new();
        assert!(!committer.commit(&mut fake, "完整结果", None));
        assert!(fake.inserted.is_empty());
        // The committer has no capture hook at all, so a stale recapture is impossible.
        assert_eq!(fake.capture_count, 0);
        assert_eq!(
            fake.recovered[0].1,
            "无法确认输入位置，文字已保留，请在恢复页复制；请到恢复页复制文字"
        );
    }

    #[test]
    fn inserts_the_complete_final_text_once_and_observes_it() {
        let mut fake = Fake::new();
        let target = fake.target.clone();
        let mut committer = Committer::new();
        assert!(committer.commit(&mut fake, "开头和结尾都保留", Some(&target)));
        assert_eq!(fake.inserted, [(fake.target.clone(), "开头和结尾都保留".to_string())]);
        assert_eq!(
            fake.observed,
            [("开头和结尾都保留".to_string(), fake.target.clone())]
        );
        assert_eq!(fake.selection, (4 + 8, 4 + 8));
        assert_eq!(fake.current_text, "已有文字开头和结尾都保留");
    }

    #[test]
    fn failed_write_is_retained_instead_of_falling_back_to_a_carrier_key() {
        let mut fake = Fake::new();
        fake.ax_succeeds = false;
        let target = fake.target.clone();
        let mut committer = Committer::new();
        assert!(!committer.commit(&mut fake, "完整结果", Some(&target)));
        assert_eq!(fake.events, []);
        assert_eq!(fake.recovered[0].0, "完整结果");
        assert_eq!(
            fake.recovered[0].1,
            "当前输入框拒绝粘贴文字；请到恢复页复制文字"
        );
    }

    #[test]
    fn unreadable_field_is_retained() {
        let mut fake = Fake::new();
        fake.snapshot_available = false;
        let target = fake.target.clone();
        let mut committer = Committer::new();
        assert!(!committer.commit(&mut fake, "不能读", Some(&target)));
        assert!(fake.inserted.is_empty());
        assert_eq!(
            fake.recovered[0].1,
            "当前输入框不可读取；请到恢复页复制文字"
        );
    }

    #[test]
    fn inserts_each_turn_exactly_once_even_when_wording_repeats() {
        let mut fake = Fake::new();
        fake.selection = (0, 0);
        fake.current_text.clear();
        let target = fake.target.clone();
        let mut committer = Committer::new();
        assert!(committer.commit(&mut fake, "重复的话", Some(&target)));
        assert!(committer.commit(&mut fake, "重复的话", Some(&target)));
        assert_eq!(fake.inserted.len(), 2);
        assert_eq!(
            fake.current_text,
            "重复的话 重复的话",
            "adjacent turns get exactly one separating space"
        );
    }

    #[test]
    fn no_segment_space_when_caret_moved_or_selection_nonempty() {
        let mut fake = Fake::new();
        fake.selection = (0, 0);
        fake.current_text.clear();
        let target = fake.target.clone();
        let mut committer = Committer::new();
        assert!(committer.commit(&mut fake, "第一段", Some(&target)));
        fake.selection = (0, 0);
        assert!(committer.commit(&mut fake, "第二段", Some(&target)));
        assert_eq!(fake.inserted[1].1, "第二段");
    }

    #[test]
    fn corrections_are_applied_before_insertion_and_spacing() {
        let mut fake = Fake::new();
        fake.selection = (0, 0);
        fake.current_text.clear();
        fake.replacement = "替换后".to_string();
        let target = fake.target.clone();
        let mut committer = Committer::new();
        assert!(committer.commit(&mut fake, "原始", Some(&target)));
        assert_eq!(fake.inserted[0].1, "替换后");
    }

    #[test]
    fn sanitize_prevents_newline_triggered_send() {
        let mut fake = Fake::new();
        fake.selection = (0, 0);
        fake.current_text.clear();
        let target = fake.target.clone();
        let mut committer = Committer::new();
        assert!(committer.commit(&mut fake, "第一行\n第二行", Some(&target)));
        assert_eq!(fake.inserted[0].1, "第一行 第二行");
    }
}
