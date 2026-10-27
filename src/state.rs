#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputState {
    Idle,
    Recording,
    Streaming,
    Processing,
    Warning,
    Error,
}

impl InputState {
    /// Diagnostics uses the Python enum names.
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Recording => "RECORDING",
            Self::Streaming => "STREAMING",
            Self::Processing => "PROCESSING",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
        }
    }

    pub fn is_recording(self) -> bool {
        matches!(self, InputState::Recording | InputState::Streaming)
    }

    pub fn can_start_recording(self) -> bool {
        self == InputState::Idle
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StateVisual {
    pub fallback_text: &'static str,
    pub description: &'static str,
    pub env_key: &'static str,
}

pub fn visual(state: InputState) -> &'static StateVisual {
    match state {
        InputState::Idle => &StateVisual {
            fallback_text: "🎙️",
            description: "空闲",
            env_key: "IDLE",
        },
        InputState::Recording => &StateVisual {
            fallback_text: "🔴",
            description: "正在启动录音",
            env_key: "RECORDING",
        },
        InputState::Streaming => &StateVisual {
            fallback_text: "🟢",
            description: "实时识别中",
            env_key: "RECORDING",
        },
        InputState::Processing => &StateVisual {
            fallback_text: "🔵",
            description: "正在生成最终结果",
            env_key: "PROCESSING",
        },
        InputState::Warning => &StateVisual {
            fallback_text: "⚠️",
            description: "警告",
            env_key: "PROCESSING",
        },
        InputState::Error => &StateVisual {
            fallback_text: "❗️",
            description: "错误",
            env_key: "PROCESSING",
        },
    }
}

pub struct StatusDisplay {
    pub title: &'static str,
    pub icon_key: Option<&'static str>,
    pub tooltip: String,
}

pub fn status_display(
    state: InputState,
    queue_length: usize,
    has_custom_icon: bool,
) -> StatusDisplay {
    let visual = visual(state);
    let icon_key = has_custom_icon.then_some(visual.env_key);
    let title = if has_custom_icon {
        ""
    } else {
        visual.fallback_text
    };

    let mut tooltip = format!("Veyqa Voice - {}（点击打开设置）", visual.description);
    if queue_length > 0 {
        tooltip.push_str(&format!(" | 待处理任务 {queue_length}"));
    }

    StatusDisplay {
        title,
        icon_key,
        tooltip,
    }
}

pub fn service_label(service: &str) -> &'static str {
    match service.to_lowercase().as_str() {
        "doubao" => "豆包",
        "custom" => "自定义服务",
        _ => "千问",
    }
}

pub fn dock_icon_enabled() -> bool {
    env_flag("SHOW_DOCK_ICON", "true")
}

pub fn menu_bar_enabled() -> bool {
    !dock_icon_enabled() || env_flag("SHOW_MENU_BAR_ICON", "false")
}

fn env_flag(name: &str, default: &str) -> bool {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .to_lowercase()
        == "true"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_predicates_match_the_session_contract() {
        assert!(InputState::Recording.is_recording());
        assert!(InputState::Streaming.is_recording());
        assert!(!InputState::Processing.is_recording());
        assert!(InputState::Idle.can_start_recording());
        assert!(!InputState::Error.can_start_recording());
    }

    #[test]
    fn icon_titles_fall_back_to_emoji_per_state() {
        let idle = status_display(InputState::Idle, 0, false);
        assert_eq!(idle.title, "🎙️");
        assert_eq!(idle.icon_key, None);
        assert_eq!(idle.tooltip, "Veyqa Voice - 空闲（点击打开设置）");

        let streaming = status_display(InputState::Streaming, 0, false);
        assert_eq!(streaming.title, "🟢");
        assert_eq!(streaming.tooltip, "Veyqa Voice - 实时识别中（点击打开设置）");
    }

    #[test]
    fn custom_icon_replaces_the_title_and_keeps_the_shared_key() {
        let recording = status_display(InputState::Recording, 0, true);
        assert_eq!(recording.title, "");
        assert_eq!(recording.icon_key, Some("RECORDING"));

        let streaming = status_display(InputState::Streaming, 0, true);
        assert_eq!(streaming.icon_key, Some("RECORDING"));

        let error = status_display(InputState::Error, 0, true);
        assert_eq!(error.icon_key, Some("PROCESSING"));
    }

    #[test]
    fn queue_length_is_only_appended_when_pending() {
        let idle = status_display(InputState::Idle, 0, true);
        assert!(!idle.tooltip.contains("待处理任务"));
        let busy = status_display(InputState::Idle, 3, true);
        assert!(busy.tooltip.ends_with(" | 待处理任务 3"));
    }

    #[test]
    fn service_labels_match_user_facing_copy() {
        assert_eq!(service_label("qwen"), "千问");
        assert_eq!(service_label("QWEN"), "千问");
        assert_eq!(service_label("doubao"), "豆包");
        assert_eq!(service_label("custom"), "自定义服务");
        assert_eq!(service_label("unknown"), "千问");
    }
}
