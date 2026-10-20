"""Release-only probe: credentials stay in memory; output contains timings only."""
import argparse
import asyncio
import json
from pathlib import Path
import time
import uuid
import wave

import aiohttp

parser = argparse.ArgumentParser()
parser.add_argument("--key-file", type=Path, required=True)
parser.add_argument("--audio", type=Path, required=True)
parser.add_argument("--host", required=True)
parser.add_argument("--pairs", type=int, default=3)
args = parser.parse_args()
assert args.host.endswith(".cn-beijing.maas.aliyuncs.com")
key = args.key_file.read_text().strip()
with wave.open(str(args.audio)) as audio:
    assert (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) == (1, 2, 16000)
    pcm = audio.readframes(audio.getnframes())
model = "qwen-audio-3.0-asr-flash-streaming"


async def recognize(session, credential, model_id=model, negative=False):
    start = time.perf_counter()
    async with session.ws_connect("wss://"+args.host+"/api-ws/v1/inference",
                                  headers={"Authorization": "Bearer "+credential}) as ws:
        task_id = uuid.uuid4().hex
        await ws.send_json({"header": {"action": "run-task", "task_id": task_id, "streaming": "duplex"},
            "payload": {"task_group": "audio", "task": "asr", "function": "recognition",
                "model": model_id, "parameters": {"format": "pcm", "sample_rate": 16000}, "input": {}}})
        message = await ws.receive(timeout=12)
        event = json.loads(message.data)
        name = event.get("header", {}).get("event")
        if negative:
            return {"unauthorized_model_rejected": name == "task-failed"}
        if name != "task-started":
            return {"task_started": False, "event": name}
        ready = time.perf_counter()
        first_text, finished, characters = None, None, 0
        async def sender():
            for index in range(0, len(pcm), 2560):
                await ws.send_bytes(pcm[index:index+2560])
                await asyncio.sleep(0.08)
            nonlocal finished
            finished = time.perf_counter()
            await ws.send_json({"header": {"action": "finish-task", "task_id": task_id,
                "streaming": "duplex"}, "payload": {"input": {}}})
        sending = asyncio.create_task(sender())
        try:
            while True:
                msg = await ws.receive(timeout=20)
                if msg.type != aiohttp.WSMsgType.TEXT:
                    return {"task_started": True, "completed": False}
                body = json.loads(msg.data)
                name = body.get("header", {}).get("event")
                sentence = body.get("payload", {}).get("output", {}).get("sentence", {})
                text = sentence.get("text", "")
                if text and not sentence.get("heartbeat"):
                    first_text = first_text or time.perf_counter()
                    characters = max(characters, len(text))
                if name in {"task-failed", "task-finished"}:
                    await sending
                    return {"task_started": True, "completed": name == "task-finished",
                        "ready_ms": round((ready-start)*1000),
                        "first_text_ms": round((first_text-ready)*1000) if first_text else None,
                        "final_after_send_ms": round((time.perf_counter()-finished)*1000),
                        "recognized_characters": characters}
        finally:
            if not sending.done():
                sending.cancel()
                await asyncio.gather(sending, return_exceptions=True)


async def run():
    async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=None, sock_connect=10)) as session:
        start = time.perf_counter()
        async with session.post("https://dashscope.aliyuncs.com/api/v1/tokens?expire_in_seconds=300",
                                headers={"Authorization": "Bearer "+key}) as response:
            if response.status != 200:
                print(json.dumps({"token_http_status": response.status})); return
            payload = await response.json()
            temporary = payload["token"]
        print(json.dumps({"token_issue_ms": round((time.perf_counter()-start)*1000),
                          "audio_seconds": round(len(pcm)/32000, 3)}), flush=True)
        print(json.dumps(await recognize(session, temporary,
            "qwen-audio-3.1-asr-flash-streaming", negative=True)), flush=True)
        for pair in range(args.pairs):
            for route, credential in (("permanent_direct", key), ("prefetched_temporary_direct", temporary)):
                try:
                    result = await recognize(session, credential)
                except Exception as exc:
                    result = {"exception_type": type(exc).__name__}
                print(json.dumps({"pair": pair+1, "route": route, **result}), flush=True)

asyncio.run(run())
