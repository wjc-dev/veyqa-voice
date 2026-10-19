"""Volcengine Doubao Seed ASR 2.0 streaming client.

The client mirrors the small callback contract used by QwenStreamingProcessor
so the voice-input state machine can switch engines without changing capture,
preview, insertion, or safety behavior.  Audio and recognized text stay in
memory unless the app's separate archive option is explicitly enabled.
"""

from __future__ import annotations

import asyncio
import gzip
import json
import os
import re
import struct
import uuid
from dataclasses import dataclass
from typing import AsyncGenerator, Callable, Optional
from pathlib import Path

import aiohttp

from ..utils.logger import logger
from ..correction_learning import (
    CorrectionStore,
    experimental_correction_learning_enabled,
)
from ..custom_vocabulary import load_custom_vocabulary
from ..runtime_paths import CUSTOM_VOCABULARY_FILE
from .speech_models import DEFAULT_MODELS, selected_model


DEFAULT_SAMPLE_RATE = 16000
CONNECT_TIMEOUT_SECONDS = 10
INITIAL_RESPONSE_TIMEOUT_SECONDS = 10
SEND_TIMEOUT_SECONDS = 5
FINAL_RESPONSE_TIMEOUT_SECONDS = 8
RECEIVE_POLL_TIMEOUT_SECONDS = 1
MAX_INLINE_VOCABULARY_TERMS = 1_000


class MessageType:
    CLIENT_FULL_REQUEST = 0b0001
    CLIENT_AUDIO_ONLY_REQUEST = 0b0010
    SERVER_FULL_RESPONSE = 0b1001
    SERVER_ERROR_RESPONSE = 0b1111


class MessageFlags:
    POS_SEQUENCE = 0b0001
    NEG_WITH_SEQUENCE = 0b0011


class Serialization:
    NONE = 0b0000
    JSON = 0b0001


class Compression:
    GZIP = 0b0001


@dataclass
class StreamingResult:
    definite_text: str = ""
    pending_text: str = ""
    is_final: bool = False
    error: Optional[str] = None


class DoubaoStreamingProcessor:
    """Stream 16 kHz mono PCM to Doubao Seed ASR 2.0."""

    engine_id = "doubao"
    engine_label = "豆包"
    model_name = DEFAULT_MODELS["doubao"]

    def __init__(self) -> None:
        self.model_name = selected_model("doubao", os.getenv("DOUBAO_ASR_RESOURCE_ID", ""))
        self.app_key = os.getenv("DOUBAO_APP_KEY", "").strip()
        self.access_key = os.getenv("DOUBAO_ACCESS_KEY", "").strip()
        self.ws_url = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async"
        self.boosting_table_id = os.getenv("DOUBAO_BOOSTING_TABLE_ID", "").strip()
        self._session: Optional[aiohttp.ClientSession] = None
        self._ws: Optional[aiohttp.ClientWebSocketResponse] = None
        self._sequence = 1
        self._sample_rate = DEFAULT_SAMPLE_RATE
        self.correction_store = CorrectionStore()

    def is_available(self) -> bool:
        return bool(self.app_key and self.access_key)

    @staticmethod
    def recognition_context() -> str:
        """Doubao does not receive the app's Qwen corpus/context payload."""
        return ""

    @staticmethod
    def _header(message_type: int, flags: int, serialization: int) -> bytes:
        # Protocol v1, one 32-bit header word, gzip payload.
        return bytes(
            (
                (0b0001 << 4) | 1,
                (message_type << 4) | flags,
                (serialization << 4) | Compression.GZIP,
                0,
            )
        )

    def _full_request(self) -> bytes:
        configured_path = Path(
            os.getenv("CUSTOM_VOCABULARY_FILE", str(CUSTOM_VOCABULARY_FILE))
        )
        hotwords = load_custom_vocabulary(
            configured_path,
            limit=MAX_INLINE_VOCABULARY_TERMS,
        )
        request = {
            "model_name": "bigmodel",
            "enable_itn": True,
            "enable_punc": True,
            "enable_ddc": True,
            "show_utterances": True,
            "result_type": "full",
            # The server performs a second-pass sentence correction after
            # the utterance boundary, which is useful for quiet speech.
            "enable_nonstream": True,
        }
        context: dict[str, object] = {}
        if hotwords:
            # Seed ASR accepts lightweight request-level hotwords.  They bias
            # recognition without requiring a cloud-side table.
            context["hotwords"] = [{"word": term} for term in hotwords]

        # Learned corrections are stronger than a bare hotword: they tell the
        # service which exact spelling/casing should replace a known mistake.
        # Also provide safe aliases for Latin product/technical terms so forms
        # such as "lightgbm" and "trade off" retain the user's preferred form.
        correct_words: dict[str, str] = {}
        if (
            experimental_correction_learning_enabled()
            and os.getenv("CORRECTION_CONTEXT_ENABLED", "false").lower() == "true"
        ):
            try:
                minimum_count = max(
                    1,
                    int(os.getenv("CORRECTION_CONTEXT_MIN_COUNT", "2")),
                )
            except ValueError:
                minimum_count = 2
            for rule in self.correction_store.rules():
                if not rule.get("enabled", True):
                    continue
                if int(rule.get("count", 0)) < minimum_count:
                    continue
                wrong = str(rule.get("wrong") or "").strip()
                correct = str(rule.get("correct") or "").strip()
                if wrong and correct and wrong != correct:
                    correct_words[wrong] = correct

        for term in hotwords:
            if not re.search(r"[A-Za-z]", term):
                continue
            aliases = {
                term.lower(),
                re.sub(r"[-_]+", " ", term).lower(),
                re.sub(r"[-_\s]+", "", term).lower(),
            }
            for alias in aliases:
                alias = alias.strip()
                if alias and alias != term:
                    correct_words.setdefault(alias, term)

        if correct_words:
            context["correct_words"] = correct_words
        if context:
            request["context"] = json.dumps(context, ensure_ascii=False)

        # A provisioned Volcengine hotword table has the most predictable
        # effect. The ID is scoped to the same speech application/App ID.
        if self.boosting_table_id:
            request["corpus"] = {"boosting_table_id": self.boosting_table_id}
        payload = {
            "user": {"uid": "voice_input_next"},
            "audio": {
                "format": "pcm",
                "codec": "raw",
                "rate": self._sample_rate,
                "bits": 16,
                "channel": 1,
            },
            "request": request,
        }
        compressed = gzip.compress(json.dumps(payload).encode("utf-8"))
        packet = bytearray(
            self._header(
                MessageType.CLIENT_FULL_REQUEST,
                MessageFlags.POS_SEQUENCE,
                Serialization.JSON,
            )
        )
        packet.extend(struct.pack(">i", self._sequence))
        packet.extend(struct.pack(">I", len(compressed)))
        packet.extend(compressed)
        self._sequence += 1
        return bytes(packet)

    def _audio_request(self, chunk: bytes, *, is_last: bool) -> bytes:
        if is_last:
            flags = MessageFlags.NEG_WITH_SEQUENCE
            sequence = -self._sequence
        else:
            flags = MessageFlags.POS_SEQUENCE
            sequence = self._sequence
            self._sequence += 1
        compressed = gzip.compress(chunk)
        packet = bytearray(
            self._header(
                MessageType.CLIENT_AUDIO_ONLY_REQUEST,
                flags,
                Serialization.NONE,
            )
        )
        packet.extend(struct.pack(">i", sequence))
        packet.extend(struct.pack(">I", len(compressed)))
        packet.extend(compressed)
        return bytes(packet)

    @staticmethod
    def _text_result(data: dict) -> StreamingResult:
        response = data.get("result")
        if not isinstance(response, dict):
            return StreamingResult()
        utterances = response.get("utterances")
        if isinstance(utterances, list) and utterances:
            definite: list[str] = []
            pending: list[str] = []
            for utterance in utterances:
                if not isinstance(utterance, dict):
                    continue
                text = str(utterance.get("text") or "")
                (definite if utterance.get("definite") else pending).append(text)
            return StreamingResult("".join(definite), "".join(pending))
        return StreamingResult(pending_text=str(response.get("text") or ""))

    def _parse_response(self, message: bytes) -> StreamingResult:
        if len(message) < 4:
            return StreamingResult(error="豆包响应过短")
        header_size = message[0] & 0x0F
        message_type = message[1] >> 4
        flags = message[1] & 0x0F
        serialization = message[2] >> 4
        compression = message[2] & 0x0F
        payload = message[header_size * 4 :]
        if flags & 0x01:
            if len(payload) < 4:
                return StreamingResult(error="豆包响应缺少序号")
            payload = payload[4:]
        is_final = bool(flags & 0x02)

        if message_type == MessageType.SERVER_ERROR_RESPONSE:
            if len(payload) < 8:
                return StreamingResult(error="豆包返回未知服务器错误", is_final=True)
            code = struct.unpack(">i", payload[:4])[0]
            payload = payload[8:]
            if compression == Compression.GZIP and payload:
                try:
                    payload = gzip.decompress(payload)
                except OSError:
                    pass
            message_text = payload.decode("utf-8", errors="replace")
            return StreamingResult(
                error=f"豆包服务器错误 {code}: {message_text[:300]}",
                is_final=True,
            )

        if message_type == MessageType.SERVER_FULL_RESPONSE:
            if len(payload) < 4:
                return StreamingResult(error="豆包响应缺少载荷长度")
            payload = payload[4:]
        if not payload:
            return StreamingResult(is_final=is_final)
        if compression == Compression.GZIP:
            try:
                payload = gzip.decompress(payload)
            except OSError as exc:
                return StreamingResult(error=f"豆包响应解压失败: {exc}")
        if serialization != Serialization.JSON:
            return StreamingResult(is_final=is_final)
        try:
            result = self._text_result(json.loads(payload.decode("utf-8")))
        except (UnicodeError, json.JSONDecodeError) as exc:
            return StreamingResult(error=f"豆包响应解析失败: {exc}")
        result.is_final = is_final
        return result

    async def connect(self) -> None:
        if not self.is_available():
            raise RuntimeError("豆包 App ID 或 Access Token 未配置")
        self._sequence = 1
        self._session = aiohttp.ClientSession(
            timeout=aiohttp.ClientTimeout(total=None, sock_connect=CONNECT_TIMEOUT_SECONDS)
        )
        try:
            self._ws = await self._session.ws_connect(
                self.ws_url,
                headers={
                    "X-Api-Resource-Id": self.model_name,
                    "X-Api-Connect-Id": str(uuid.uuid4()),
                    "X-Api-Access-Key": self.access_key,
                    "X-Api-App-Key": self.app_key,
                },
                timeout=CONNECT_TIMEOUT_SECONDS,
                heartbeat=20,
            )
        except Exception:
            await self.disconnect()
            raise

    async def disconnect(self) -> None:
        try:
            if self._ws is not None and not self._ws.closed:
                await asyncio.wait_for(self._ws.close(), timeout=2)
        except Exception:
            pass
        try:
            if self._session is not None and not self._session.closed:
                await asyncio.wait_for(self._session.close(), timeout=2)
        except Exception:
            pass
        self._ws = None
        self._session = None

    async def _send_bytes(self, payload: bytes) -> None:
        if self._ws is None:
            raise RuntimeError("豆包连接尚未建立")
        await asyncio.wait_for(self._ws.send_bytes(payload), timeout=SEND_TIMEOUT_SECONDS)

    async def _receive(self, timeout: float) -> Optional[StreamingResult]:
        if self._ws is None:
            return StreamingResult(error="豆包连接尚未建立", is_final=True)
        try:
            message = await self._ws.receive(timeout=timeout)
        except asyncio.TimeoutError:
            return None
        if message.type == aiohttp.WSMsgType.BINARY:
            return self._parse_response(message.data)
        if message.type == aiohttp.WSMsgType.ERROR:
            return StreamingResult(error="豆包 WebSocket 错误", is_final=True)
        if message.type in {aiohttp.WSMsgType.CLOSE, aiohttp.WSMsgType.CLOSED}:
            return StreamingResult(error="豆包连接提前关闭", is_final=True)
        return None

    async def process_audio_stream(
        self,
        audio_chunk_generator: AsyncGenerator[bytes, None],
        on_preview_text: Callable[[str], None],
        on_final_text: Callable[[str], None],
        on_complete: Callable[[], None],
        on_error: Callable[[str], None],
        sample_rate: int = DEFAULT_SAMPLE_RATE,
    ) -> None:
        """Send one utterance and emit exactly one final callback."""
        self._sample_rate = sample_rate
        latest_text = ""
        sender_finished = asyncio.Event()

        try:
            await self.connect()
            await self._send_bytes(self._full_request())
            initial = await self._receive(INITIAL_RESPONSE_TIMEOUT_SECONDS)
            if initial is not None and initial.error:
                raise RuntimeError(initial.error)

            async def sender() -> None:
                async for chunk in audio_chunk_generator:
                    await self._send_bytes(self._audio_request(chunk, is_last=False))
                await self._send_bytes(self._audio_request(b"", is_last=True))
                sender_finished.set()

            async def receiver() -> None:
                nonlocal latest_text
                final_wait_started: Optional[float] = None
                while True:
                    result = await self._receive(RECEIVE_POLL_TIMEOUT_SECONDS)
                    if result is None:
                        if sender_finished.is_set():
                            loop = asyncio.get_running_loop()
                            final_wait_started = final_wait_started or loop.time()
                            if loop.time() - final_wait_started >= FINAL_RESPONSE_TIMEOUT_SECONDS:
                                raise TimeoutError("等待豆包最终识别结果超时")
                        continue
                    if result.error:
                        raise RuntimeError(result.error)
                    candidate = result.definite_text + result.pending_text
                    if candidate:
                        latest_text = candidate
                        on_preview_text(candidate)
                    if result.is_final:
                        return

            sender_task = asyncio.create_task(sender())
            receiver_task = asyncio.create_task(receiver())
            results = await asyncio.gather(sender_task, receiver_task, return_exceptions=True)
            error = next((item for item in results if isinstance(item, BaseException)), None)
            if error is not None:
                raise error
            if latest_text:
                on_final_text(latest_text)
            on_complete()
        except Exception as exc:  # noqa: BLE001
            logger.error("豆包流式识别失败: %s", exc)
            on_error(str(exc))
        finally:
            await self.disconnect()
