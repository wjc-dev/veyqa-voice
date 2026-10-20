"""Qwen Audio Streaming/Message WebSocket processor.

Unlike Qwen3-ASR-Realtime's OpenAI-style ``/realtime`` protocol, Qwen Audio
3.0 uses DashScope's duplex task protocol on ``/inference``.  Audio is sent as
binary PCM frames after ``task-started`` and the task is explicitly finished
when the push-to-talk shortcut is released.
"""

from __future__ import annotations

import asyncio
import json
import os
import uuid
from pathlib import Path
from typing import AsyncGenerator, Callable, Optional
from urllib.parse import urlparse

import aiohttp

from ..utils.logger import logger
from ..correction_learning import (
    CorrectionStore,
    experimental_correction_learning_enabled,
)
from ..custom_vocabulary import load_custom_vocabulary
from ..runtime_paths import AUDIO_ARCHIVE_DIR, CONTEXT_FILE, CUSTOM_VOCABULARY_FILE
from .speech_models import DEFAULT_MODELS, selected_model


DEFAULT_SAMPLE_RATE = 16000
CONNECT_TIMEOUT_SECONDS = 10
SEND_TIMEOUT_SECONDS = 5
FINAL_RESPONSE_TIMEOUT_SECONDS = 20
CONTEXT_CHARACTER_LIMIT = 400
MAX_INLINE_VOCABULARY_TERMS = 2_000
MODEL_NAME = DEFAULT_MODELS["qwen"]


class QwenStreamingProcessor:
    """Stream PCM audio to Alibaba Cloud Model Studio Qwen ASR."""

    engine_id = "qwen"
    engine_label = "千问"
    model_name = MODEL_NAME

    def __init__(self, *, model_name: str | None = None) -> None:
        self.model_name = selected_model("qwen", model_name if model_name is not None else os.getenv("QWEN_ASR_MODEL", ""))
        self.api_key = os.getenv("QWEN_API_KEY") or os.getenv("DASHSCOPE_API_KEY", "")
        self.workspace_id = os.getenv("QWEN_WORKSPACE_ID", "").strip()
        self.api_host = os.getenv("QWEN_API_HOST", "").strip()
        self.region = os.getenv("QWEN_REGION", "beijing").strip().lower()
        self.language = os.getenv("QWEN_LANGUAGE", "zh").strip().lower()
        self.context_enabled = os.getenv("QWEN_CONTEXT_ENABLED", "false").lower() == "true"
        self.recent_memory_enabled = (
            os.getenv("QWEN_RECENT_MEMORY_ENABLED", "false").lower() == "true"
        )
        try:
            recent_memory_count = int(os.getenv("QWEN_RECENT_MEMORY_COUNT", "20"))
        except ValueError:
            recent_memory_count = 20
        self.recent_memory_count = max(1, min(50, recent_memory_count))
        self.context_file = Path(
            os.getenv(
                "QWEN_CONTEXT_FILE",
                str(CONTEXT_FILE),
            )
        )
        self._session: Optional[aiohttp.ClientSession] = None
        self._ws: Optional[aiohttp.ClientWebSocketResponse] = None
        self.correction_store = CorrectionStore()

        if not self.api_key:
            logger.info("千问 API Key 未配置，千问 ASR 暂不可用")

    def is_available(self) -> bool:
        return bool(self.api_key)

    def _build_url(self) -> str:
        if self.api_host:
            candidate = self.api_host
            if "://" not in candidate:
                candidate = "https://" + candidate
            parsed = urlparse(candidate)
            host = parsed.hostname or ""
            if not host.endswith(".aliyuncs.com"):
                raise ValueError("千问 API Host 必须是 aliyuncs.com 官方域名")
        elif self.region == "singapore":
            host = (
                f"{self.workspace_id}.ap-southeast-1.maas.aliyuncs.com"
                if self.workspace_id
                else "dashscope-intl.aliyuncs.com"
            )
        else:
            host = (
                f"{self.workspace_id}.cn-beijing.maas.aliyuncs.com"
                if self.workspace_id
                else "dashscope.aliyuncs.com"
            )
        return f"wss://{host}/api-ws/v1/inference"

    def _load_recent_memory(self) -> str:
        """Load only this tool's own recent transcripts when explicitly enabled."""
        if not self.recent_memory_enabled:
            return ""
        cache_path = AUDIO_ARCHIVE_DIR / "cache.json"
        try:
            cache = json.loads(cache_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError, UnicodeDecodeError):
            return ""
        if not isinstance(cache, dict):
            return ""

        records = [value for value in cache.values() if isinstance(value, dict)]
        records.sort(key=lambda value: str(value.get("timestamp") or ""), reverse=True)
        texts: list[str] = []
        seen: set[str] = set()
        for record in records:
            text = str(record.get("transcription") or "").strip()
            if not text or text in seen:
                continue
            seen.add(text)
            texts.append(text)
            if len(texts) >= self.recent_memory_count:
                break
        if not texts:
            return ""
        return "近期由本语音工具转写的主题（仅用于识别上下文）：\n" + "\n".join(texts)

    def _load_context(self) -> str:
        sections: list[str] = []

        # Put confirmed corrections first.  Qwen Audio 3.0 keeps at most 400
        # characters per context turn, so the most actionable terms must not
        # be pushed out by a long background paragraph.
        learned_lines = self.correction_store.context_lines()
        if learned_lines:
            sections.append(
                "用户反复人工纠正过的语音词汇（优先按右侧形式识别）：\n"
                + "\n".join(learned_lines)
            )

        if self.context_enabled and self.context_file.exists():
            try:
                personal_text = self.context_file.read_text(encoding="utf-8").strip()
            except OSError as exc:
                logger.warning("读取千问个性化上下文失败: %s", exc)
            else:
                if personal_text:
                    sections.append("用户主动配置的背景与词汇：\n" + personal_text)

        recent_memory = self._load_recent_memory()
        if recent_memory:
            sections.append(recent_memory)

        text = "\n\n".join(sections)
        if len(text) > CONTEXT_CHARACTER_LIMIT:
            logger.warning(
                "千问个性化上下文超过 %d 字符，已截断发送",
                CONTEXT_CHARACTER_LIMIT,
            )
            text = text[:CONTEXT_CHARACTER_LIMIT]
        return text

    def recognition_context(self) -> str:
        """Return the exact local context that will bias this ASR session."""
        return self._load_context()

    def _inline_vocabulary(self) -> dict[str, int]:
        """Return explicit and learned terms as request-level ASR hotwords."""
        configured_path = Path(
            os.getenv("CUSTOM_VOCABULARY_FILE", str(CUSTOM_VOCABULARY_FILE))
        )
        vocabulary: dict[str, int] = {
            term: 4
            for term in load_custom_vocabulary(
                configured_path,
                limit=MAX_INLINE_VOCABULARY_TERMS,
            )
        }
        if (
            not experimental_correction_learning_enabled()
            or os.getenv("CORRECTION_CONTEXT_ENABLED", "false").lower() != "true"
        ):
            return vocabulary
        try:
            minimum_count = max(1, int(os.getenv("CORRECTION_CONTEXT_MIN_COUNT", "2")))
        except ValueError:
            minimum_count = 2

        for rule in self.correction_store.rules():
            if not rule.get("enabled", True) or int(rule.get("count", 0)) < minimum_count:
                continue
            term = str(rule.get("correct") or "").strip()
            if not term or len(term) > 64:
                continue
            vocabulary.setdefault(term, 4)
            if len(vocabulary) >= MAX_INLINE_VOCABULARY_TERMS:
                break
        return vocabulary

    def _language_hints(self) -> list[str]:
        if self.language == "zh":
            # The setting is labelled "Chinese first", not "Chinese only".
            # Keeping English in the hints preserves mixed terms such as
            # LightGBM and trade-off.
            return ["zh", "en"]
        if self.language and self.language != "auto":
            return [self.language]
        return []

    def _run_task_event(self, sample_rate: int, task_id: str) -> dict:
        parameters: dict[str, object] = {
            "format": "pcm",
            "sample_rate": sample_rate,
            "heartbeat": True,
        }
        if "-message" in self.model_name:
            parameters["intermediate_result_enabled"] = True
            parameters["disfluency_removal_enabled"] = False
            parameters["keep_dialect"] = True
        language_hints = self._language_hints()
        if language_hints:
            parameters["language_hints"] = language_hints
        vocabulary = self._inline_vocabulary()
        if vocabulary:
            parameters["vocabulary"] = vocabulary

        task_input: dict[str, object] = {}
        context = self._load_context()
        if context:
            task_input["context"] = [
                {
                    "role": "user",
                    "content": [{"type": "input_text", "text": context}],
                }
            ]

        return {
            "header": {
                "action": "run-task",
                "task_id": task_id,
                "streaming": "duplex",
            },
            "payload": {
                "task_group": "audio",
                "task": "asr",
                "function": "recognition",
                "model": self.model_name,
                "parameters": parameters,
                "input": task_input,
            },
        }

    @staticmethod
    def _finish_task_event(task_id: str) -> dict:
        return {
            "header": {
                "action": "finish-task",
                "task_id": task_id,
                "streaming": "duplex",
            },
            "payload": {"input": {}},
        }

    @staticmethod
    def _event_text(event: dict) -> tuple[Optional[str], Optional[str], bool]:
        """Return (preview, error, is_completed) for a server event."""
        header = event.get("header") or {}
        dashscope_event = header.get("event", "") if isinstance(header, dict) else ""
        if dashscope_event == "result-generated":
            payload = event.get("payload") or {}
            output = payload.get("output") or {} if isinstance(payload, dict) else {}
            sentence = output.get("sentence") or {} if isinstance(output, dict) else {}
            if not isinstance(sentence, dict):
                sentence = {}
            if sentence.get("heartbeat"):
                return None, None, False
            text = str(sentence.get("text") or "").strip()
            return text or None, None, bool(sentence.get("sentence_end", False))

        if dashscope_event == "task-failed":
            message = (
                header.get("error_message")
                or header.get("error_code")
                or "未知服务端错误"
            )
            return None, f"千问 ASR 错误: {message}", False

        # Retain parsing support for recorded Qwen3 events in diagnostics and
        # for a clean transition from the previous implementation.
        event_type = event.get("type", "")
        if event_type in {
            "conversation.item.input_audio_transcription.text",
            "conversation.item.input_audio_transcription.delta",
        }:
            preview = f"{event.get('text', '')}{event.get('stash', '')}".strip()
            if not preview:
                preview = str(event.get("transcript") or "").strip()
            return preview or None, None, False

        if event_type == "conversation.item.input_audio_transcription.completed":
            return str(event.get("transcript") or "").strip() or None, None, True

        if event_type in {
            "conversation.item.input_audio_transcription.failed",
            "error",
        }:
            error = event.get("error") or {}
            if isinstance(error, dict):
                message = error.get("message") or error.get("code") or str(error)
            else:
                message = str(error)
            return None, f"千问 ASR 错误: {message}", False

        return None, None, False

    async def _connect(self) -> None:
        if not self.is_available():
            raise RuntimeError("千问 API Key 未配置")

        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "User-Agent": "Whisper-Input-Next/Qwen-ASR",
        }
        # A workspace-specific API Host already identifies the workspace.
        # Sending a separately entered (and possibly confused with API Key ID)
        # workspace header can make an otherwise valid request fail.
        if self.workspace_id and not self.api_host:
            headers["X-DashScope-WorkSpace"] = self.workspace_id

        self._session = aiohttp.ClientSession(
            timeout=aiohttp.ClientTimeout(total=None, sock_connect=CONNECT_TIMEOUT_SECONDS)
        )
        try:
            self._ws = await self._session.ws_connect(
                self._build_url(),
                headers=headers,
                timeout=CONNECT_TIMEOUT_SECONDS,
                heartbeat=20,
            )
        except Exception:
            await self._disconnect()
            raise

    async def _disconnect(self) -> None:
        try:
            if self._ws and not self._ws.closed:
                await asyncio.wait_for(self._ws.close(), timeout=2)
        except Exception:
            pass
        try:
            if self._session and not self._session.closed:
                await asyncio.wait_for(self._session.close(), timeout=2)
        except Exception:
            pass
        self._ws = None
        self._session = None

    async def _send_json(self, event: dict) -> None:
        if not self._ws or self._ws.closed:
            raise RuntimeError("千问 WebSocket 未连接")
        await asyncio.wait_for(
            self._ws.send_str(json.dumps(event, ensure_ascii=False)),
            timeout=SEND_TIMEOUT_SECONDS,
        )

    async def _send_audio(self, chunk: bytes) -> None:
        if not self._ws or self._ws.closed:
            raise RuntimeError("千问 WebSocket 未连接")
        await asyncio.wait_for(
            self._ws.send_bytes(chunk),
            timeout=SEND_TIMEOUT_SECONDS,
        )

    async def _wait_until_ready(self) -> None:
        if not self._ws:
            raise RuntimeError("千问 WebSocket 未连接")
        deadline = asyncio.get_running_loop().time() + CONNECT_TIMEOUT_SECONDS
        while asyncio.get_running_loop().time() < deadline:
            msg = await self._ws.receive(timeout=CONNECT_TIMEOUT_SECONDS)
            if msg.type != aiohttp.WSMsgType.TEXT:
                if msg.type in {aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR}:
                    raise RuntimeError("千问连接在会话初始化时关闭")
                continue
            event = json.loads(msg.data)
            _, error, _ = self._event_text(event)
            if error:
                raise RuntimeError(error)
            header = event.get("header") or {}
            if isinstance(header, dict) and header.get("event") == "task-started":
                return
        raise TimeoutError("等待千问 task-started 超时")

    async def process_audio_stream(
        self,
        audio_chunk_generator: AsyncGenerator[bytes, None],
        on_preview_text: Callable[[str], None],
        on_final_text: Callable[[str], None],
        on_complete: Callable[[], None],
        on_error: Callable[[str], None],
        sample_rate: int = DEFAULT_SAMPLE_RATE,
    ) -> None:
        """Stream one push-to-talk utterance and emit its final transcript."""
        final_segments: list[str] = []
        finalized_segment_keys: set[tuple[object, object, str]] = set()
        finalized_sentence_indices: dict[object, int] = {}
        current_partial = ""
        latest_preview = ""
        task_finished = False
        task_id = uuid.uuid4().hex[:32]

        def combined_text() -> str:
            return "".join([*final_segments, current_partial]).strip()

        async def sender() -> int:
            chunk_count = 0
            async for chunk in audio_chunk_generator:
                chunk_count += 1
                await self._send_audio(chunk)
            logger.info("千问音频发送完成，共 %d 个音频块", chunk_count)
            if chunk_count == 0:
                # A very short tap can finish before PortAudio produces its
                # first callback. Committing an empty buffer makes Qwen return
                # an error and, historically, could encourage corpus echo.
                # End locally instead; there is nothing useful to recognize.
                return 0
            await self._send_json(self._finish_task_event(task_id))
            return chunk_count

        async def receiver() -> Optional[str]:
            nonlocal current_partial, latest_preview, task_finished
            if not self._ws:
                return "千问 WebSocket 未连接"
            while True:
                msg = await self._ws.receive()
                if msg.type == aiohttp.WSMsgType.TEXT:
                    event = json.loads(msg.data)
                    preview, error, completed = self._event_text(event)
                    if error:
                        return error
                    if preview:
                        header = event.get("header") or {}
                        payload = event.get("payload") or {}
                        output = payload.get("output") or {} if isinstance(payload, dict) else {}
                        sentence = output.get("sentence") or {} if isinstance(output, dict) else {}
                        if completed and isinstance(sentence, dict):
                            key = (
                                sentence.get("begin_time"),
                                sentence.get("end_time"),
                                preview,
                            )
                            sentence_id = sentence.get("sentence_id")
                            if isinstance(sentence_id, int) and sentence_id > 0:
                                if sentence_id in finalized_sentence_indices:
                                    final_segments[finalized_sentence_indices[sentence_id]] = preview
                                else:
                                    finalized_sentence_indices[sentence_id] = len(final_segments)
                                    final_segments.append(preview)
                            elif key not in finalized_segment_keys:
                                finalized_segment_keys.add(key)
                                final_segments.append(preview)
                            current_partial = ""
                        else:
                            current_partial = preview
                        latest_preview = combined_text()
                        if latest_preview:
                            on_preview_text(latest_preview)
                    header = event.get("header") or {}
                    if isinstance(header, dict) and header.get("event") == "task-finished":
                        task_finished = True
                        return None
                elif msg.type in {aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.CLOSE}:
                    return None if task_finished else "千问连接提前关闭"
                elif msg.type == aiohttp.WSMsgType.ERROR:
                    return f"千问 WebSocket 错误: {msg.data}"

        sender_task: Optional[asyncio.Task] = None
        receiver_task: Optional[asyncio.Task] = None
        try:
            await self._connect()
            logger.info("千问 ASR 实时连接成功：%s", self.model_name)
            await self._send_json(self._run_task_event(sample_rate, task_id))
            await self._wait_until_ready()

            sender_task = asyncio.create_task(sender())
            receiver_task = asyncio.create_task(receiver())

            done, _ = await asyncio.wait(
                {sender_task, receiver_task},
                return_when=asyncio.FIRST_COMPLETED,
            )
            if receiver_task in done and not sender_task.done():
                early_error = receiver_task.result()
                if early_error:
                    sender_task.cancel()
                    await asyncio.gather(sender_task, return_exceptions=True)
                    on_error(early_error)
                    return

            chunk_count = await sender_task
            if chunk_count == 0:
                if receiver_task and not receiver_task.done():
                    receiver_task.cancel()
                    await asyncio.gather(receiver_task, return_exceptions=True)
                logger.info("极短按下未产生音频块，已在本地静默忽略")
                on_complete()
                return
            try:
                receive_error = await asyncio.wait_for(
                    receiver_task,
                    timeout=FINAL_RESPONSE_TIMEOUT_SECONDS,
                )
            except asyncio.TimeoutError:
                receive_error = "等待千问最终识别结果超时"

            if receive_error:
                on_error(receive_error)
                return

            output = combined_text() or latest_preview
            if output:
                on_final_text(output)
            on_complete()
        except aiohttp.WSServerHandshakeError as exc:
            on_error(f"千问鉴权失败（HTTP {exc.status}），请检查 API Key、地域和 Workspace ID")
        except Exception as exc:  # noqa: BLE001
            on_error(f"千问流式识别失败: {exc}")
        finally:
            for task in (sender_task, receiver_task):
                if task and not task.done():
                    task.cancel()
            await self._disconnect()
