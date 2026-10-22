"""Private release test: synthetic audio only, sanitized metrics, isolated data."""
import argparse
import asyncio
import json
import os
from pathlib import Path
import sys
import time
import wave

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--resources", type=Path, required=True)
parser.add_argument("--audio", type=Path, required=True)
parser.add_argument("--data", type=Path, required=True)
parser.add_argument("--report", type=Path, required=True)
args = parser.parse_args()
os.environ["VOICE_INPUT_DATA_DIR"] = str(args.data.resolve())
os.environ["QWEN_CONTEXT_ENABLED"] = "false"
os.environ["QWEN_RECENT_MEMORY_ENABLED"] = "false"
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from src.transcription.builtin import BuiltinStreamingProcessor
from src.transcription.private_trial import PrivateTrialBroker


async def run():
    with wave.open(str(args.audio)) as audio:
        if (audio.getframerate(), audio.getnchannels(), audio.getsampwidth()) != (16000, 1, 2):
            raise ValueError("Expected 16 kHz mono PCM16")
        pcm = audio.readframes(audio.getnframes())
    broker = PrivateTrialBroker(directory=args.data, resource_root=args.resources)
    processor = BuiltinStreamingProcessor(broker=broker)
    started = time.monotonic()
    first_text = None
    final = []
    completed = []
    errors = []
    sent_finished = None

    async def chunks():
        nonlocal sent_finished
        for index in range(0, len(pcm), 2560):
            yield pcm[index:index + 2560]
            await asyncio.sleep(0.08)
        sent_finished = time.monotonic()

    def preview(text):
        nonlocal first_text
        if text and first_text is None:
            first_text = time.monotonic()

    try:
        await asyncio.wait_for(processor.process_audio_stream(
            chunks(), preview, lambda text: final.append(text),
            lambda: completed.append(True), lambda error: errors.append(True)), timeout=45)
        result = {
            "model": processor.model_name,
            "completed": bool(completed) and not errors and bool(final),
            "audio_seconds": round(len(pcm) / 32000, 3),
            "first_text_ms": round((first_text - started) * 1000) if first_text else None,
            "final_after_send_ms": round((time.monotonic() - sent_finished) * 1000) if sent_finished else None,
            "recognized_characters": sum(len(text) for text in final),
            "contains_test_phrase": any("语音输入" in text for text in final),
            "instance_key_cleared": processor.api_key == "",
        }
    except Exception as error:
        result = {"completed": False, "exception_type": type(error).__name__}
    finally:
        broker._pool.shutdown()
    args.report.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(result, ensure_ascii=False))
    return 0 if result.get("completed") and result.get("contains_test_phrase") else 1


raise SystemExit(asyncio.run(run()))
