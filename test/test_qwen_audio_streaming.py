import asyncio
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import aiohttp

from src.correction_learning import CorrectionStore
from src.transcription.qwen_streaming import (
    CONTEXT_CHARACTER_LIMIT,
    MODEL_NAME,
    QwenStreamingProcessor,
)


class _TextMessage:
    type = aiohttp.WSMsgType.TEXT

    def __init__(self, event):
        self.data = json.dumps(event, ensure_ascii=False)


class _ResultWebSocket:
    closed = False

    def __init__(self, events):
        self.events = list(events)

    async def receive(self):
        await asyncio.sleep(0)
        return _TextMessage(self.events.pop(0))


class QwenAudioProtocolTests(unittest.TestCase):
    def test_uses_new_inference_endpoint_and_model(self):
        with patch.dict(
            os.environ,
            {
                "QWEN_API_HOST": "",
                "QWEN_WORKSPACE_ID": "",
                "QWEN_REGION": "beijing",
            },
        ):
            processor = QwenStreamingProcessor()
        self.assertEqual(MODEL_NAME, "qwen-audio-3.0-asr-flash-streaming")
        self.assertEqual(
            processor._build_url(),
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference",
        )

    def test_run_task_carries_pcm_language_context_and_learned_hotword(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            context_file = root / "context.txt"
            vocabulary_file = root / "custom_vocabulary.txt"
            context_file.write_text("软件开发，常用 LightGBM、trade-off。" * 40, encoding="utf-8")
            vocabulary_file.write_text("项目代号\nLightGBM\n", encoding="utf-8")
            store = CorrectionStore(root / "corrections.json")
            store.record("锤道", "trade off")
            store.record("锤道", "trade off")
            with patch.dict(
                os.environ,
                {
                    "QWEN_CONTEXT_ENABLED": "true",
                    "QWEN_CONTEXT_FILE": str(context_file),
                    "CUSTOM_VOCABULARY_FILE": str(vocabulary_file),
                    "QWEN_LANGUAGE": "zh",
                    "CORRECTION_STORE_FILE": str(store.path),
                    "EXPERIMENTAL_CORRECTION_LEARNING": "true",
                    "CORRECTION_CONTEXT_ENABLED": "true",
                    "CORRECTION_CONTEXT_MIN_COUNT": "2",
                },
            ):
                processor = QwenStreamingProcessor()
                event = processor._run_task_event(16000, "task-id")

        self.assertEqual(event["header"]["action"], "run-task")
        self.assertEqual(event["payload"]["model"], MODEL_NAME)
        parameters = event["payload"]["parameters"]
        self.assertEqual(parameters["format"], "pcm")
        self.assertEqual(parameters["sample_rate"], 16000)
        self.assertEqual(parameters["language_hints"], ["zh", "en"])
        self.assertEqual(parameters["vocabulary"]["trade off"], 4)
        self.assertEqual(parameters["vocabulary"]["项目代号"], 4)
        self.assertEqual(parameters["vocabulary"]["LightGBM"], 4)
        context = event["payload"]["input"]["context"][0]["content"][0]["text"]
        self.assertLessEqual(len(context), CONTEXT_CHARACTER_LIMIT)
        self.assertIn("trade off", context)

    def test_parses_result_and_failure_events(self):
        preview, error, completed = QwenStreamingProcessor._event_text(
            {
                "header": {"event": "result-generated"},
                "payload": {
                    "output": {
                        "sentence": {"text": "你好。", "sentence_end": True}
                    }
                },
            }
        )
        self.assertEqual((preview, error, completed), ("你好。", None, True))

        preview, error, completed = QwenStreamingProcessor._event_text(
            {"header": {"event": "task-failed", "error_message": "bad request"}}
        )
        self.assertIsNone(preview)
        self.assertIn("bad request", error)
        self.assertFalse(completed)


class QwenAudioStreamingTests(unittest.IsolatedAsyncioTestCase):
    async def test_message_revised_sentence_replaces_previous_final(self):
        with patch.dict(os.environ, {"QWEN_ASR_MODEL": "qwen-audio-3.1-asr-flash-message"}):
            processor = QwenStreamingProcessor()
        processor.api_key = "test-key"
        events = []
        for number, text in [(1, "数据工作"), (1, "DataWorks。"), (2, "继续测试。")]:
            events.append({"header": {"event": "result-generated"}, "payload": {"output": {
                "sentence": {"sentence_id": number, "text": text, "sentence_end": True}
            }}})
        events.append({"header": {"event": "task-finished"}, "payload": {}})
        processor._ws = _ResultWebSocket(events)
        async def noop(*args): pass
        async def audio(): yield b"\0" * 3200
        processor._connect = processor._disconnect = processor._wait_until_ready = noop
        processor._send_json = processor._send_audio = noop
        finals, errors = [], []
        await processor.process_audio_stream(audio(), lambda _: None, finals.append, lambda: None, errors.append)
        self.assertEqual(errors, [])
        self.assertEqual(finals, ["DataWorks。继续测试。"])

    async def test_accumulates_finalized_sentences_without_duplicate_output(self):
        processor = QwenStreamingProcessor()
        processor.api_key = "test-key-present-only-in-memory"
        processor._ws = _ResultWebSocket(
            [
                {
                    "header": {"event": "result-generated"},
                    "payload": {
                        "output": {
                            "sentence": {
                                "text": "你好",
                                "sentence_end": False,
                                "begin_time": 0,
                            }
                        }
                    },
                },
                {
                    "header": {"event": "result-generated"},
                    "payload": {
                        "output": {
                            "sentence": {
                                "text": "你好。",
                                "sentence_end": True,
                                "begin_time": 0,
                                "end_time": 500,
                            }
                        }
                    },
                },
                {
                    "header": {"event": "result-generated"},
                    "payload": {
                        "output": {
                            "sentence": {
                                "text": "第二句。",
                                "sentence_end": True,
                                "begin_time": 500,
                                "end_time": 1000,
                            }
                        }
                    },
                },
                {"header": {"event": "task-finished"}, "payload": {}},
            ]
        )
        previews = []
        finals = []
        completed = []
        errors = []

        async def connect():
            return None

        async def disconnect():
            processor._ws = None

        async def wait_until_ready():
            return None

        async def send_json(_event):
            return None

        async def send_audio(_chunk):
            return None

        async def audio():
            yield b"\0" * 3200

        processor._connect = connect
        processor._disconnect = disconnect
        processor._wait_until_ready = wait_until_ready
        processor._send_json = send_json
        processor._send_audio = send_audio

        await processor.process_audio_stream(
            audio(),
            previews.append,
            finals.append,
            lambda: completed.append(True),
            errors.append,
        )

        self.assertEqual(errors, [])
        self.assertEqual(finals, ["你好。第二句。"])
        self.assertEqual(completed, [True])
        self.assertEqual(previews[-1], "你好。第二句。")


if __name__ == "__main__":
    unittest.main()
