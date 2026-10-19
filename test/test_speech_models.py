import os
from unittest.mock import patch

import pytest

from src.transcription.speech_models import DEFAULT_MODELS, selected_model
from src.transcription.qwen_streaming import QwenStreamingProcessor
from src.transcription.doubao_streaming import DoubaoStreamingProcessor


def test_old_configuration_keeps_current_models():
    assert selected_model("qwen") == "qwen-audio-3.0-asr-flash-streaming"
    assert selected_model("doubao") == "volc.seedasr.sauc.duration"


@pytest.mark.parametrize("model", [
    "qwen-audio-3.1-asr-flash-streaming",
    "qwen-audio-3.1-asr-flash-message",
    "qwen-audio-9.2-asr-flash-streaming-2027-01-01",
])
def test_selected_qwen_model_reaches_wire_request(model):
    with patch.dict(os.environ, {"QWEN_ASR_MODEL": model}):
        processor = QwenStreamingProcessor()
    payload = processor._run_task_event(16000, "test")["payload"]
    assert payload["model"] == processor.model_name == model
    if "-message" in model:
        assert payload["parameters"]["intermediate_result_enabled"] is True
        assert payload["parameters"]["disfluency_removal_enabled"] is False


def test_selected_doubao_resource_is_retained():
    with patch.dict(os.environ, {"DOUBAO_ASR_RESOURCE_ID": "volc.seedasr.sauc.concurrent"}):
        processor = DoubaoStreamingProcessor()
    assert processor.model_name == "volc.seedasr.sauc.concurrent"


@pytest.mark.parametrize("model", ["qwen-plus", "qwen-audio-3.1-asr-flash-filetrans", "x\nKEY=value"])
def test_unrelated_protocols_rejected_before_network(model):
    with pytest.raises(ValueError):
        selected_model("qwen", model)
