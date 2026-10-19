"""Check configured Qwen models using optional, explicitly supplied test audio.

No microphone, clipboard, application configuration writes, or credential output.
"""
import asyncio
import argparse
import os
from pathlib import Path
import sys
import wave

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from dotenv import dotenv_values
from src.transcription.qwen_streaming import QwenStreamingProcessor
from src.transcription.doubao_streaming import DoubaoStreamingProcessor


async def check(model, pcm, provider="qwen"):
    os.environ["QWEN_ASR_MODEL" if provider == "qwen" else "DOUBAO_ASR_RESOURCE_ID"] = model
    processor = QwenStreamingProcessor() if provider == "qwen" else DoubaoStreamingProcessor()
    # The probe sends only the supplied synthetic test phrase.
    processor._load_context = lambda: ""
    processor._inline_vocabulary = lambda: {}
    if pcm is None:
        if provider == "doubao":
            print(model, "test WAV required")
            return
        try:
            await processor._connect()
            await processor._send_json(processor._run_task_event(16000, "catalogue-probe"))
            await processor._wait_until_ready()
            print(model, "task-started")
        except Exception as exc:
            print(model, "FAILED", type(exc).__name__)
        finally:
            await processor._disconnect()
        return
    results, errors = [], []
    async def audio():
        for start in range(0, len(pcm), 3200):
            yield pcm[start:start + 3200]
            await asyncio.sleep(0.1)
    await processor.process_audio_stream(audio(), lambda _: None, results.append, lambda: None, errors.append)
    print(model, "OK" if results and not errors else "FAILED", repr(results))


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("env_file")
    parser.add_argument("test_wav", nargs="?")
    parser.add_argument("--provider", choices=("qwen", "doubao"), default="qwen")
    args = parser.parse_args()
    values = dotenv_values(args.env_file)
    for key in ("QWEN_API_KEY", "DASHSCOPE_API_KEY", "QWEN_API_HOST", "QWEN_REGION", "QWEN_WORKSPACE_ID", "DOUBAO_APP_KEY", "DOUBAO_ACCESS_KEY"):
        if values.get(key): os.environ[key] = values[key]
    pcm = None
    if args.test_wav:
        with wave.open(args.test_wav, "rb") as audio:
            assert (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) == (1, 2, 16000)
            pcm = audio.readframes(audio.getnframes())
    models = ("qwen-audio-3.0-asr-flash-streaming", "qwen-audio-3.1-asr-flash-streaming", "qwen-audio-3.1-asr-flash-message") if args.provider == "qwen" else (values.get("DOUBAO_ASR_RESOURCE_ID") or "volc.seedasr.sauc.duration",)
    for model in models:
        await check(model, pcm, args.provider)


if __name__ == "__main__":
    asyncio.run(main())
