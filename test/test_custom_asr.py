import asyncio
import io
import os
import unittest
import wave
from unittest.mock import patch

from aiohttp import web

from src.transcription.custom_asr import CustomASRProcessor, transcription_url


class CustomASRTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.requests = []
        self.status = 200
        self.result = {"text": "今天测试 user_id。"}
        self.received = asyncio.Event()
        self.release = asyncio.Event()
        self.delay = False

        async def transcribe(request):
            self.received.set()
            reader = await request.multipart()
            fields = {}
            async for part in reader:
                fields[part.name] = bytes(await part.read())
            self.requests.append((request.headers.get("Authorization"), fields))
            if self.delay:
                await self.release.wait()
            return web.json_response(self.result, status=self.status,
                                     headers={"Location": "/should-not-follow"} if self.status == 307 else {})

        app = web.Application()
        app.router.add_post("/v1/audio/transcriptions", transcribe)
        self.runner = web.AppRunner(app)
        await self.runner.setup()
        site = web.TCPSite(self.runner, "127.0.0.1", 0)
        await site.start()
        port = site._server.sockets[0].getsockname()[1]
        with patch.dict(os.environ, {"CUSTOM_ASR_BASE_URL": f"http://127.0.0.1:{port}/v1",
                                     "CUSTOM_ASR_MODEL": "test-asr", "CUSTOM_ASR_API_KEY": "own-provider-key"}):
            self.processor = CustomASRProcessor()

    async def asyncTearDown(self):
        self.release.set()
        await self.runner.cleanup()

    async def run_audio(self, chunks=(b"\x01\x00" * 1600,)):
        async def audio():
            for chunk in chunks:
                yield chunk
        finals, done, errors = [], [], []
        await self.processor.process_audio_stream(audio(), lambda _: None, finals.append,
                                                  lambda: done.append(True), errors.append)
        return finals, done, errors

    async def test_multipart_wav_and_result(self):
        self.assertEqual(await self.run_audio(), (["今天测试 user_id。"], [True], []))
        key, fields = self.requests[0]
        self.assertEqual(key, "Bearer own-provider-key")
        self.assertEqual(fields["model"], b"test-asr")
        self.assertEqual(fields["response_format"], b"json")
        with wave.open(io.BytesIO(fields["file"]), "rb") as wav:
            self.assertEqual((wav.getnchannels(), wav.getsampwidth(), wav.getframerate()), (1, 2, 16000))
            self.assertEqual(wav.readframes(1600), b"\x01\x00" * 1600)

    async def test_empty_audio_does_not_call_provider(self):
        self.assertEqual(await self.run_audio(()), ([], [True], []))
        self.assertEqual(self.requests, [])

    async def test_auth_failure_does_not_expose_response(self):
        self.status, self.result = 401, {"secret": "do-not-display"}
        finals, done, errors = await self.run_audio()
        self.assertEqual((finals, done), ([], []))
        self.assertIn("鉴权失败", errors[0])
        self.assertNotIn("do-not-display", errors[0])

    async def test_redirect_is_not_followed(self):
        self.status = 307
        self.assertIn("307", (await self.run_audio())[2][0])
        self.assertEqual(len(self.requests), 1)

    async def test_malformed_response(self):
        self.result = {"not_text": "no"}
        self.assertIn("text", (await self.run_audio())[2][0])

    async def test_response_size_is_bounded(self):
        with patch("src.transcription.custom_asr.MAX_RESPONSE_BYTES", 8):
            self.assertIn("返回内容过大", (await self.run_audio())[2][0])

    async def test_cancelled_request_propagates(self):
        self.delay = True
        task = asyncio.create_task(self.run_audio())
        await asyncio.wait_for(self.received.wait(), 3)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task

    async def test_audio_size_is_bounded(self):
        with patch("src.transcription.custom_asr.MAX_AUDIO_BYTES", 8):
            self.assertIn("录音过长", (await self.run_audio())[2][0])
        self.assertEqual(self.requests, [])

    def test_url_validation(self):
        self.assertEqual(transcription_url("https://example.com/v1/"), "https://example.com/v1/audio/transcriptions")
        self.assertEqual(transcription_url("http://[::1]:8080/v1"), "http://[::1]:8080/v1/audio/transcriptions")
        for url in ("http://example.com/v1", "https://key@example.com", "https://example.com?token=x", "file:///tmp"):
            with self.assertRaises(ValueError):
                transcription_url(url)

    def test_custom_key_is_not_inherited(self):
        with patch.dict(os.environ, {"QWEN_API_KEY": "qwen-key", "DOUBAO_ACCESS_KEY": "doubao-key"}, clear=True):
            self.assertFalse(CustomASRProcessor().is_available())


if __name__ == "__main__":
    unittest.main()
