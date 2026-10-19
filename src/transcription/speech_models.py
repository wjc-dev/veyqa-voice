"""Configured ASR identifiers, independent of the provider's model catalogue.

The catalogue supplies offline defaults. Protocol families, rather than model
versions, determine compatibility. A refresh must never change a saved choice.
"""
from __future__ import annotations

import json
import re

from ..runtime_paths import RESOURCE_ROOT, SOURCE_ROOT


def _catalogue() -> dict:
    for path in (RESOURCE_ROOT / "asr-models.json", SOURCE_ROOT / "config/asr-models.json"):
        if path.is_file():
            return json.loads(path.read_text(encoding="utf-8"))
    raise RuntimeError("语音模型目录缺失，请重新安装应用")


DEFAULT_MODELS = _catalogue()["defaults"]
QWEN_PATTERN = r"qwen-audio-\d+(?:\.\d+)*-asr-flash-(?:streaming|message)(?:-\d{4}-\d{2}-\d{2})?"
DOUBAO_PATTERN = r"volc\.[a-z0-9]+\.sauc\.(?:duration|concurrent)"


def selected_model(provider: str, value: str = "") -> str:
    model = value.strip() or DEFAULT_MODELS[provider]
    pattern = QWEN_PATTERN if provider == "qwen" else DOUBAO_PATTERN
    if re.fullmatch(pattern, model) is None:
        raise ValueError("请选择支持实时听写的语音模型")
    return model
