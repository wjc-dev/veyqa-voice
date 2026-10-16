"""Actionable feedback without guessing that every empty result is a mic fault."""


def empty_result_notice(audio_ms, voiced_ms, mode, hotkey):
    if audio_ms < 500:
        action = (f"请按住{hotkey}说完再松开" if mode == "hold"
                  else f"请按一下{hotkey}开始，说完再按一下结束")
        return "too_short", f"录音过短，{action}"
    if voiced_ms < 100:
        return "silence", "没有检测到声音，请检查系统选中的麦克风和输入音量"
    return "empty_result", "已收到声音，但语音服务没有返回文字，请重试"
