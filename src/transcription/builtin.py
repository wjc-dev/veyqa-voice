"""Promotional Qwen adapter. Only authentication differs from direct BYOK."""
import asyncio
import time
import uuid

import aiohttp

from .promotion import ERRORS, MODEL, get_broker
from .qwen_streaming import QwenStreamingProcessor


class BuiltinStreamingProcessor(QwenStreamingProcessor):
    engine_label = "免费语音"
    builtin = True

    def __init__(self, broker=None):
        super().__init__(model_name=MODEL)
        self.broker = broker or get_broker()
        self.api_key = ""
        self.workspace_id = ""
        self.broker.prefetch()
        self._remaining_seconds = 0

    def is_available(self):
        return True  # No permanent Key is required; network failures remain actionable.

    async def _connect(self):
        for attempt in range(2):
            credential = await asyncio.to_thread(self.broker.get)
            if credential.remaining_seconds <= 0:
                raise RuntimeError(ERRORS["monthly_allowance_used"])
            self.api_key = credential.token
            self.api_host = credential.api_host
            self._remaining_seconds = credential.remaining_seconds
            try:
                await super()._connect()
                return
            except aiohttp.WSServerHandshakeError as exc:
                if exc.status != 401 or attempt:
                    raise RuntimeError("免费语音服务鉴权失败，请稍后重试或使用自己的 Key。") from None
                self.broker.invalidate()

    async def process_audio_stream(self, audio_chunk_generator, on_preview_text, on_final_text,
                                 on_complete, on_error, sample_rate=16000):
        session_id, started = uuid.uuid4().hex, time.time()
        sent_bytes = 0
        original_send = self._send_audio

        async def counted_send(chunk):
            nonlocal sent_bytes
            await original_send(chunk)
            sent_bytes += len(chunk)

        async def limited_audio():
            # _connect completes before Qwen requests the first audio chunk.
            remaining = self._remaining_seconds * sample_rate * 2
            async for chunk in audio_chunk_generator:
                if remaining <= 0:
                    break
                chunk = chunk[:remaining]
                remaining -= len(chunk)
                yield chunk

        self._send_audio = counted_send
        try:
            await super().process_audio_stream(limited_audio(), on_preview_text,
                on_final_text, on_complete, on_error, sample_rate)
        finally:
            self._send_audio = original_send
            self.api_key = ""  # Shared in-memory cache owns credential lifetime.
            self.broker.report(session_id, sent_bytes / (sample_rate * 2), started)
