"""OpenAI-compatible speech transcription, submitted when recording ends."""

from __future__ import annotations

import asyncio
import io
import json
import os
import wave
from urllib.parse import urlsplit, urlunsplit

import aiohttp


MAX_AUDIO_BYTES = 24 * 1024 * 1024
MAX_RESPONSE_BYTES = 1024 * 1024


def transcription_url(base: str) -> str:
    """Accept HTTPS providers and loopback HTTP, without embedded credentials."""
    try:
        normalized = base.strip()
        if any(char.isspace() or ord(char) < 32 for char in normalized):
            raise ValueError
        parsed = urlsplit(normalized)
        local = parsed.hostname in {"localhost", "127.0.0.1", "::1"}
        if (not parsed.hostname or parsed.username is not None or parsed.password is not None
                or parsed.query or parsed.fragment
                or not (parsed.scheme == "https" or (parsed.scheme == "http" and local))):
            raise ValueError
        if parsed.port is not None and not 1 <= parsed.port <= 65535:
            raise ValueError
        path = parsed.path.rstrip("/")
        if not path.endswith("/audio/transcriptions"):
            path += "/audio/transcriptions"
        return urlunsplit((parsed.scheme, parsed.netloc, path, "", ""))
    except ValueError:
        raise ValueError("请填写 HTTPS API 地址；本机服务可使用 HTTP") from None


class CustomASRProcessor:
    engine_id = "custom"
    engine_label = "自定义服务"

    def __init__(self):
        self.base_url = os.getenv("CUSTOM_ASR_BASE_URL", "").strip()
        self.api_key = os.getenv("CUSTOM_ASR_API_KEY", "").strip()
        self.model_name = os.getenv("CUSTOM_ASR_MODEL", "").strip()

    def is_available(self):
        try:
            url = transcription_url(self.base_url)
        except ValueError:
            return False
        return bool(self.model_name and not any(c.isspace() for c in self.model_name)
                    and (self.api_key or urlsplit(url).hostname in {"localhost", "127.0.0.1", "::1"})
                    and not any(c.isspace() for c in self.api_key))

    def recognition_context(self):
        return ""

    async def process_audio_stream(self, audio_stream, on_preview, on_final, on_complete,
                                   on_error, sample_rate=16000):
        try:
            if not self.is_available():
                raise ValueError("请检查自定义服务的地址、API Key 和模型")
            pcm = bytearray()
            async for chunk in audio_stream:
                if len(pcm) + len(chunk) > MAX_AUDIO_BYTES:
                    raise ValueError("本次录音过长，请分段输入")
                pcm.extend(chunk)
            if not pcm:
                on_complete()
                return
            if len(pcm) % 2 or not 8000 <= sample_rate <= 192000:
                raise ValueError("录音格式异常，请重新录制")
            output = io.BytesIO()
            with wave.open(output, "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(sample_rate)
                wav.writeframes(pcm)
            form = aiohttp.FormData()
            form.add_field("file", output.getvalue(), filename="speech.wav", content_type="audio/wav")
            form.add_field("model", self.model_name)
            form.add_field("response_format", "json")
            headers = {"Authorization": f"Bearer {self.api_key}"} if self.api_key else {}
            timeout = aiohttp.ClientTimeout(total=60, connect=10)
            async with aiohttp.ClientSession(timeout=timeout, trust_env=False) as session:
                async with session.post(transcription_url(self.base_url), data=form,
                                        headers=headers, allow_redirects=False) as response:
                    if response.status != 200:
                        if response.status in {401, 403}:
                            raise ValueError("自定义服务鉴权失败，请检查 API Key 和模型权限")
                        raise ValueError(f"自定义服务请求失败（HTTP {response.status}），请检查地址和模型")
                    payload = bytearray()
                    async for chunk in response.content.iter_chunked(65536):
                        if len(payload) + len(chunk) > MAX_RESPONSE_BYTES:
                            raise ValueError("自定义服务返回内容过大")
                        payload.extend(chunk)
                    try:
                        result = json.loads(payload)
                    except (ValueError, UnicodeDecodeError):
                        raise ValueError("自定义服务未返回有效的转写结果") from None
                    text = result.get("text") if isinstance(result, dict) else None
                    if not isinstance(text, str):
                        raise ValueError("自定义服务响应缺少 text 字段")
                    if text.strip():
                        on_final(text.strip())
                    on_complete()
        except asyncio.CancelledError:
            raise
        except ValueError as exc:
            on_error(str(exc))
        except (aiohttp.ClientError, asyncio.TimeoutError):
            on_error("自定义服务连接失败或超时，请检查网络和 API 地址")
