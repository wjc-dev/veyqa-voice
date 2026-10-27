//! Actionable feedback without guessing that every empty result is a mic fault.

pub struct EmptyResultNotice {
    pub outcome: &'static str,
    pub notice: String,
}

pub fn empty_result_notice(audio_ms: f64, voiced_ms: f64, mode: &str, hotkey: &str) -> EmptyResultNotice {
    if audio_ms < 500.0 {
        let action = if mode == "hold" {
            format!("请按住{hotkey}说完再松开")
        } else {
            format!("请按一下{hotkey}开始，说完再按一下结束")
        };
        return EmptyResultNotice {
            outcome: "too_short",
            notice: format!("录音过短，{action}"),
        };
    }
    if voiced_ms < 100.0 {
        return EmptyResultNotice {
            outcome: "silence",
            notice: "没有检测到声音，请检查系统选中的麦克风和输入音量".to_string(),
        };
    }
    EmptyResultNotice {
        outcome: "empty_result",
        notice: "已收到声音，但语音服务没有返回文字，请重试".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hold_explains_hold_instead_of_blaming_microphone() {
        let notice = empty_result_notice(130.0, 130.0, "hold", "右 Option");
        assert_eq!(notice.outcome, "too_short");
        assert!(notice.notice.contains("按住"));
        assert!(!notice.notice.contains("检查麦克风"));
    }

    #[test]
    fn short_toggle_explains_toggle() {
        let notice = empty_result_notice(130.0, 130.0, "toggle", "右 Option");
        assert!(notice.notice.contains("再按"));
    }

    #[test]
    fn silence_and_provider_empty_are_distinct() {
        assert_eq!(
            empty_result_notice(2000.0, 0.0, "hold", "右 Option").outcome,
            "silence"
        );
        assert_eq!(
            empty_result_notice(2000.0, 1800.0, "hold", "右 Option").outcome,
            "empty_result"
        );
    }

    #[test]
    fn boundary_values_follow_the_python_thresholds() {
        assert_eq!(
            empty_result_notice(499.0, 0.0, "hold", "H").outcome,
            "too_short"
        );
        assert_eq!(
            empty_result_notice(500.0, 99.0, "hold", "H").outcome,
            "silence"
        );
        assert_eq!(
            empty_result_notice(500.0, 100.0, "hold", "H").outcome,
            "empty_result"
        );
    }
}
