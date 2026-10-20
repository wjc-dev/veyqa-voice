import asyncio
import unittest
from unittest.mock import AsyncMock, Mock, patch

import aiohttp

from src.transcription.builtin import BuiltinStreamingProcessor
from src.transcription.promotion import Credential, MODEL
from src.transcription.qwen_streaming import QwenStreamingProcessor


class BuiltinStreamingTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.broker = Mock()
        self.broker.get.return_value = Credential(
            "st-test-only", 9999999999, "ws-test.cn-beijing.maas.aliyuncs.com", 1, "2026-10")
        with patch.object(QwenStreamingProcessor, "__init__", return_value=None):
            self.processor = BuiltinStreamingProcessor(self.broker)

    async def test_temporary_key_is_used_directly_and_401_retries_once(self):
        async def connect():
            self.assertEqual(self.processor.api_key, "st-test-only")
            self.assertEqual(self.processor.api_host, "ws-test.cn-beijing.maas.aliyuncs.com")
        unauthorized = aiohttp.WSServerHandshakeError(None, (), status=401)
        with patch.object(QwenStreamingProcessor, "_connect", new=AsyncMock(side_effect=[unauthorized, None])) as method:
            await self.processor._connect()
        self.assertEqual(method.await_count, 2)
        self.broker.invalidate.assert_called_once()
        await connect()

    async def test_non_auth_failure_does_not_retry_or_expose_token(self):
        forbidden = aiohttp.WSServerHandshakeError(None, (), status=403)
        with patch.object(QwenStreamingProcessor, "_connect", new=AsyncMock(side_effect=forbidden)) as method:
            with self.assertRaisesRegex(RuntimeError, "鉴权失败") as caught:
                await self.processor._connect()
        self.assertNotIn("st-test-only", str(caught.exception))
        self.assertEqual(method.await_count, 1)
        self.broker.invalidate.assert_not_called()

    async def exercise_stream(self, *, cancel=False):
        sent = []
        async def send(chunk):
            sent.append(chunk)
        self.processor._send_audio = send
        async def audio():
            yield b"x" * 24000
            yield b"y" * 24000
        async def base_stream(instance, chunks, *args):
            instance._remaining_seconds = 1
            instance.api_key = "st-test-only"
            async for chunk in chunks:
                await instance._send_audio(chunk)
                if cancel:
                    raise asyncio.CancelledError()
        with patch.object(QwenStreamingProcessor, "process_audio_stream", new=base_stream):
            task = self.processor.process_audio_stream(audio(), Mock(), Mock(), Mock(), Mock())
            if cancel:
                with self.assertRaises(asyncio.CancelledError):
                    await task
            else:
                await task
        self.assertEqual(self.processor.api_key, "")
        self.assertIs(self.processor._send_audio, send)
        self.broker.report.assert_called_once()
        self.assertEqual(self.broker.report.call_args.args[1], sum(map(len, sent)) / 32000)
        return sent

    async def test_stream_caps_audio_to_allowance_without_buffering_entire_recording(self):
        sent = await self.exercise_stream()
        self.assertEqual([len(chunk) for chunk in sent], [24000, 8000])

    async def test_cancellation_reports_only_sent_audio_and_clears_key(self):
        sent = await self.exercise_stream(cancel=True)
        self.assertEqual([len(chunk) for chunk in sent], [24000])
