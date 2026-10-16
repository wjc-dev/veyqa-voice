"""Learn local ASR corrections from edits made immediately after insertion.

Only compact correction pairs are persisted.  The surrounding text read through
macOS Accessibility is kept in memory briefly to locate the inserted span and is
never written to disk.
"""

from __future__ import annotations

import difflib
import json
import os
import re
import tempfile
import threading
import time
import unicodedata
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

from .runtime_paths import CORRECTIONS_FILE
from .utils.logger import logger


DEFAULT_STORE_PATH = CORRECTIONS_FILE
STORE_VERSION = 2
MAX_TERM_LENGTH = 64
ANCHOR_LENGTH = 64
MAX_OBSERVED_SPAN = 2000
MAX_CORRECTION_EVENTS = 200
_UNSAFE_SHORT_FORMS = {
    "这个", "那个", "就是", "然后", "而且", "但是", "因为", "所以",
    "我们", "你们", "他们", "可以", "需要", "没有", "一个", "什么",
}


def experimental_correction_learning_enabled() -> bool:
    """Keep automatic edit observation behind an explicit developer gate.

    The production UI intentionally does not expose this experimental feature:
    Accessibility representations differ too much across native, web and
    Electron editors to infer user intent without a confirmation step.
    """
    return os.getenv("EXPERIMENTAL_CORRECTION_LEARNING", "false").lower() == "true"


def _now_iso() -> str:
    return datetime.now(timezone.utc).astimezone().isoformat(timespec="seconds")


def _is_only_space_or_punctuation(text: str) -> bool:
    return all(
        character.isspace() or unicodedata.category(character).startswith("P")
        for character in text
    )


def _trim_pair(wrong: str, correct: str) -> tuple[str, str]:
    """Drop matching whitespace around a replacement without changing its body."""
    while wrong and correct and wrong[0].isspace() and correct[0].isspace():
        wrong, correct = wrong[1:], correct[1:]
    while wrong and correct and wrong[-1].isspace() and correct[-1].isspace():
        wrong, correct = wrong[:-1], correct[:-1]
    return wrong.strip(), correct.strip()


def _trim_likely_continuation(wrong: str, correct: str) -> str:
    """Keep the English term when a Chinese homophone is corrected then typing continues."""
    contains_cjk = any("\u3400" <= character <= "\u9fff" for character in wrong)
    if contains_cjk and correct and correct[0].isascii() and correct[0].isalnum():
        match = re.match(
            r"[A-Za-z0-9]+(?:[ _./+-]+[A-Za-z0-9]+)*",
            correct,
        )
        if match:
            return match.group(0).rstrip()
    return correct


def extract_correction_pairs(original: str, edited: str) -> list[tuple[str, str]]:
    """Return conservative replacement pairs from an edited ASR span.

    Insertions and deletions are ignored because they commonly represent normal
    continued typing.  Only a direct replacement can become an automatic rule.
    """
    if not original or not edited or original == edited:
        return []

    matcher = difflib.SequenceMatcher(a=original, b=edited, autojunk=False)
    pairs: list[tuple[str, str]] = []
    seen: set[tuple[str, str]] = set()
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag != "replace":
            continue
        wrong, correct = _trim_pair(original[i1:i2], edited[j1:j2])
        correct = _trim_likely_continuation(wrong, correct)
        if not wrong or not correct or wrong == correct:
            continue
        if len(wrong) > MAX_TERM_LENGTH or len(correct) > MAX_TERM_LENGTH:
            continue
        if _is_only_space_or_punctuation(wrong) and _is_only_space_or_punctuation(correct):
            continue
        pair = (wrong, correct)
        if pair not in seen:
            seen.add(pair)
            pairs.append(pair)
    return pairs


class CorrectionStore:
    """Thread-safe, private, frequency-ranked local correction store."""

    def __init__(self, path: Optional[Path] = None) -> None:
        configured = os.getenv("CORRECTION_STORE_FILE", "").strip()
        self.path = path or (Path(configured).expanduser() if configured else DEFAULT_STORE_PATH)
        self._lock = threading.RLock()

    def _empty(self) -> dict[str, Any]:
        return {"version": STORE_VERSION, "rules": [], "events": []}

    def _load_unlocked(self) -> dict[str, Any]:
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError):
            return self._empty()
        if not isinstance(data, dict) or not isinstance(data.get("rules"), list):
            return self._empty()
        if not isinstance(data.get("events"), list):
            data["events"] = []
        data["version"] = STORE_VERSION
        return data

    def load(self) -> dict[str, Any]:
        with self._lock:
            return self._load_unlocked()

    def _save_unlocked(self, data: dict[str, Any]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary_name = tempfile.mkstemp(prefix=f".{self.path.name}.", dir=self.path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(data, handle, ensure_ascii=False, indent=2)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.chmod(temporary_name, 0o600)
            os.replace(temporary_name, self.path)
        finally:
            if os.path.exists(temporary_name):
                os.unlink(temporary_name)

    def record(self, wrong: str, correct: str) -> None:
        wrong, correct = _trim_pair(wrong, correct)
        if not wrong or not correct or wrong == correct:
            return
        now = _now_iso()
        with self._lock:
            data = self._load_unlocked()
            rules = data["rules"]
            for rule in rules:
                if rule.get("wrong") == wrong and rule.get("correct") == correct:
                    rule["count"] = max(0, int(rule.get("count", 0))) + 1
                    rule["last_seen"] = now
                    rule["enabled"] = bool(rule.get("enabled", True))
                    break
            else:
                rules.append(
                    {
                        "wrong": wrong,
                        "correct": correct,
                        "count": 1,
                        "first_seen": now,
                        "last_seen": now,
                        "enabled": True,
                    }
                )
            events = data.setdefault("events", [])
            events.append({"wrong": wrong, "correct": correct, "timestamp": now})
            if len(events) > MAX_CORRECTION_EVENTS:
                del events[:-MAX_CORRECTION_EVENTS]
            rules.sort(
                key=lambda rule: (
                    -int(rule.get("count", 0)),
                    str(rule.get("last_seen", "")),
                    str(rule.get("wrong", "")),
                )
            )
            self._save_unlocked(data)

    def undo_last_record(self) -> Optional[tuple[str, str]]:
        """Undo the most recent automatically learned correction event."""
        with self._lock:
            data = self._load_unlocked()
            events = data.setdefault("events", [])
            if not events:
                return None
            event = events.pop()
            wrong = str(event.get("wrong") or "")
            correct = str(event.get("correct") or "")
            for index, rule in enumerate(data["rules"]):
                if rule.get("wrong") != wrong or rule.get("correct") != correct:
                    continue
                count = max(0, int(rule.get("count", 0)) - 1)
                if count == 0:
                    del data["rules"][index]
                else:
                    rule["count"] = count
                    matching_events = [
                        item for item in events
                        if item.get("wrong") == wrong and item.get("correct") == correct
                    ]
                    rule["last_seen"] = (
                        matching_events[-1].get("timestamp") if matching_events
                        else rule.get("first_seen")
                    )
                break
            self._save_unlocked(data)
            return wrong, correct

    def rules(self) -> list[dict[str, Any]]:
        data = self.load()
        return [dict(rule) for rule in data.get("rules", []) if isinstance(rule, dict)]

    def delete(self, wrong: str, correct: str) -> bool:
        with self._lock:
            data = self._load_unlocked()
            before = len(data["rules"])
            data["rules"] = [
                rule
                for rule in data["rules"]
                if not (rule.get("wrong") == wrong and rule.get("correct") == correct)
            ]
            changed = len(data["rules"]) != before
            if changed:
                data["events"] = [
                    event for event in data.get("events", [])
                    if not (
                        event.get("wrong") == wrong
                        and event.get("correct") == correct
                    )
                ]
                self._save_unlocked(data)
            return changed

    def clear(self) -> None:
        with self._lock:
            self._save_unlocked(self._empty())

    def apply(self, text: str) -> str:
        """Apply confirmed rules once, without cascading replacements."""
        if (
            not text
            or not experimental_correction_learning_enabled()
            or os.getenv("CORRECTION_AUTO_REPLACE", "false").lower() != "true"
        ):
            return text
        try:
            minimum = max(2, int(os.getenv("CORRECTION_REPLACE_MIN_COUNT", "2")))
        except ValueError:
            minimum = 2

        grouped: dict[str, list[dict[str, Any]]] = {}
        for rule in self.rules():
            wrong = str(rule.get("wrong") or "")
            correct = str(rule.get("correct") or "")
            if wrong and correct and rule.get("enabled", True):
                grouped.setdefault(wrong, []).append(rule)

        selected: list[tuple[str, str]] = []
        for wrong, candidates in grouped.items():
            candidates.sort(key=lambda rule: int(rule.get("count", 0)), reverse=True)
            best = candidates[0]
            best_count = int(best.get("count", 0))
            second_count = int(candidates[1].get("count", 0)) if len(candidates) > 1 else 0
            if (
                best_count >= minimum
                and best_count > second_count
                and self._safe_for_automatic_replacement(wrong)
            ):
                selected.append((wrong, str(best["correct"])))

        if not selected:
            return text
        matches: list[tuple[int, int, str]] = []
        for wrong, correct in selected:
            if wrong.isascii() and any(character.isalnum() for character in wrong):
                pattern = re.compile(
                    rf"(?<![A-Za-z0-9_]){re.escape(wrong)}(?![A-Za-z0-9_])",
                    re.IGNORECASE,
                )
                occurrences = pattern.finditer(text)
            else:
                occurrences = re.finditer(re.escape(wrong), text)
            matches.extend((match.start(), match.end(), correct) for match in occurrences)

        # Longest match wins at a position; edits are generated from the
        # original string so one correction can never trigger another rule.
        matches.sort(key=lambda item: (item[0], -(item[1] - item[0])))
        accepted: list[tuple[int, int, str]] = []
        cursor = 0
        for start, end, correct in matches:
            if start < cursor:
                continue
            accepted.append((start, end, correct))
            cursor = end
        if not accepted:
            return text
        parts = []
        cursor = 0
        for start, end, correct in accepted:
            parts.extend((text[cursor:start], correct))
            cursor = end
        parts.append(text[cursor:])
        return "".join(parts)

    @staticmethod
    def _safe_for_automatic_replacement(wrong: str) -> bool:
        normalized = wrong.strip().casefold()
        if normalized in _UNSAFE_SHORT_FORMS or len(normalized) < 2:
            return False
        if normalized.isascii():
            compact = re.sub(r"[^a-z0-9]", "", normalized)
            return len(compact) >= 3
        return True

    def context_lines(self, minimum_count: Optional[int] = None, limit: int = 100) -> list[str]:
        if (
            not experimental_correction_learning_enabled()
            or os.getenv("CORRECTION_CONTEXT_ENABLED", "false").lower() != "true"
        ):
            return []
        if minimum_count is None:
            try:
                minimum_count = max(1, int(os.getenv("CORRECTION_CONTEXT_MIN_COUNT", "2")))
            except ValueError:
                minimum_count = 2
        result = []
        for rule in self.rules():
            if not rule.get("enabled", True) or int(rule.get("count", 0)) < minimum_count:
                continue
            result.append(
                f"{rule.get('wrong', '')} → {rule.get('correct', '')}"
                f"（已人工纠正 {int(rule.get('count', 0))} 次）"
            )
            if len(result) >= limit:
                break
        return result


@dataclass
class _AXTarget:
    element: Any
    pid: int


class CorrectionLearner:
    """Short-lived observer for the text inserted by one transcription."""

    def __init__(self, store: Optional[CorrectionStore] = None) -> None:
        self.store = store or CorrectionStore()
        self.enabled = (
            experimental_correction_learning_enabled()
            and os.getenv("CORRECTION_LEARNING_ENABLED", "false").lower() == "true"
        )
        self._generation = 0
        self._generation_lock = threading.Lock()

    @staticmethod
    def _ax_modules():
        from AppKit import NSWorkspace
        from ApplicationServices import (
            AXUIElementCopyAttributeValue,
            AXUIElementCreateApplication,
            AXUIElementSetMessagingTimeout,
            AXValueGetValue,
            kAXFocusedUIElementAttribute,
            kAXSelectedTextRangeAttribute,
            kAXValueAttribute,
            kAXValueTypeCFRange,
        )

        return {
            "workspace": NSWorkspace,
            "copy": AXUIElementCopyAttributeValue,
            "create": AXUIElementCreateApplication,
            "timeout": AXUIElementSetMessagingTimeout,
            "get_value": AXValueGetValue,
            "focused": kAXFocusedUIElementAttribute,
            "selection": kAXSelectedTextRangeAttribute,
            "value": kAXValueAttribute,
            "range_type": kAXValueTypeCFRange,
        }

    def capture_target(self) -> Optional[_AXTarget]:
        if sys_platform() != "darwin":
            return None
        try:
            ax = self._ax_modules()
            front = ax["workspace"].sharedWorkspace().frontmostApplication()
            if front is None:
                return None
            pid = int(front.processIdentifier())
            app = ax["create"](pid)
            if "timeout" in ax:
                ax["timeout"](app, 0.5)
            error, element = ax["copy"](app, ax["focused"], None)
            if error != 0 or element is None:
                return None
            if "timeout" in ax:
                ax["timeout"](element, 0.5)
            return _AXTarget(element=element, pid=pid)
        except Exception as exc:  # noqa: BLE001
            logger.debug("无法准备自动纠错观察: %s", exc)
            return None

    def frontmost_pid(self) -> Optional[int]:
        """Return the active app pid without requiring AX access to its editor."""
        if sys_platform() != "darwin":
            return None
        try:
            ax = self._ax_modules()
            front = ax["workspace"].sharedWorkspace().frontmostApplication()
            return int(front.processIdentifier()) if front is not None else None
        except Exception:
            return None

    def target_is_focused(self, target: Optional[_AXTarget]) -> bool:
        if target is None:
            return False
        current = self.capture_target()
        if current is None or current.pid != target.pid:
            return False
        try:
            from CoreFoundation import CFEqual

            return bool(CFEqual(current.element, target.element))
        except Exception:
            try:
                return bool(current.element == target.element)
            except Exception:
                return False

    @staticmethod
    def same_target(
        first: Optional[_AXTarget],
        second: Optional[_AXTarget],
    ) -> bool:
        """Return whether two captures refer to the same editor element."""
        if first is None or second is None or first.pid != second.pid:
            return False
        try:
            from CoreFoundation import CFEqual

            return bool(CFEqual(first.element, second.element))
        except Exception:
            try:
                return bool(first.element == second.element)
            except Exception:
                return False

    def snapshot(self, target: Optional[_AXTarget]) -> Optional[tuple[str, int, int]]:
        if target is None:
            return None
        return self._snapshot(target)

    def target_diagnostics(self, target: Optional[_AXTarget]) -> str:
        """Return non-content AX metadata for insertion troubleshooting."""
        if target is None:
            return "target=none"
        try:
            from ApplicationServices import (
                AXUIElementCopyAttributeValue,
                AXUIElementIsAttributeSettable,
                kAXRoleAttribute,
                kAXSelectedTextAttribute,
                kAXValueAttribute,
            )

            role_error, role = AXUIElementCopyAttributeValue(
                target.element, kAXRoleAttribute, None
            )

            def settable(attribute):
                error, value = AXUIElementIsAttributeSettable(
                    target.element, attribute, None
                )
                return bool(value) if error == 0 else False

            return (
                f"pid={target.pid}, role={role if role_error == 0 else 'unknown'}, "
                f"selectedTextSettable={settable(kAXSelectedTextAttribute)}, "
                f"valueSettable={settable(kAXValueAttribute)}, "
                f"readable={self._snapshot(target) is not None}"
            )
        except Exception:
            return f"pid={target.pid}, role=unknown"

    def replace_text_range(
        self,
        target: Optional[_AXTarget],
        start: int,
        length: int,
        replacement: str,
    ) -> bool:
        """Replace a focused editor range through macOS Accessibility.

        This avoids synthesizing Command+V for every streaming update. The
        indices accepted here are Python Unicode indices; AX ranges use UTF-16
        code units, so they are converted against the current editor value.
        """
        if target is None or start < 0 or length < 0:
            return False
        snapshot = self._snapshot(target)
        if snapshot is None:
            return False
        current, _selection_start, _selection_end = snapshot
        if start + length > len(current):
            return False
        try:
            from ApplicationServices import (
                AXUIElementSetAttributeValue,
                AXValueCreate,
                kAXSelectedTextAttribute,
                kAXSelectedTextRangeAttribute,
                kAXValueTypeCFRange,
            )

            utf16_location = len(current[:start].encode("utf-16-le")) // 2
            utf16_length = len(current[start:start + length].encode("utf-16-le")) // 2
            range_value = AXValueCreate(
                kAXValueTypeCFRange,
                (utf16_location, utf16_length),
            )
            if range_value is None:
                return False
            range_error = AXUIElementSetAttributeValue(
                target.element,
                kAXSelectedTextRangeAttribute,
                range_value,
            )
            if range_error != 0:
                return False
            text_error = AXUIElementSetAttributeValue(
                target.element,
                kAXSelectedTextAttribute,
                replacement,
            )
            return text_error == 0
        except Exception as exc:  # noqa: BLE001
            logger.debug("通过辅助功能替换流式文字失败: %s", exc)
            return False

    def replace_value_range(
        self,
        target: Optional[_AXTarget],
        start: int,
        length: int,
        replacement: str,
    ) -> bool:
        """Fallback for editors that reject ``AXSelectedText``.

        Some Electron text areas claim that setting AXSelectedText succeeded
        but discard the edit. When AXValue itself is writable, replace the
        complete value while preserving the requested range and caret.
        """
        if target is None or start < 0 or length < 0:
            return False
        snapshot = self._snapshot(target)
        if snapshot is None:
            return False
        current, _selection_start, _selection_end = snapshot
        if start + length > len(current):
            return False
        updated = current[:start] + replacement + current[start + length:]
        try:
            from ApplicationServices import (
                AXUIElementIsAttributeSettable,
                AXUIElementSetAttributeValue,
                AXValueCreate,
                kAXSelectedTextRangeAttribute,
                kAXValueAttribute,
                kAXValueTypeCFRange,
            )

            error, settable = AXUIElementIsAttributeSettable(
                target.element, kAXValueAttribute, None
            )
            if error != 0 or not settable:
                return False
            if AXUIElementSetAttributeValue(
                target.element, kAXValueAttribute, updated
            ) != 0:
                return False

            caret_utf16 = len(
                updated[:start + len(replacement)].encode("utf-16-le")
            ) // 2
            range_value = AXValueCreate(kAXValueTypeCFRange, (caret_utf16, 0))
            if range_value is not None:
                AXUIElementSetAttributeValue(
                    target.element,
                    kAXSelectedTextRangeAttribute,
                    range_value,
                )
            return True
        except Exception as exc:  # noqa: BLE001
            logger.debug("通过辅助功能写回完整输入值失败: %s", exc)
            return False

    def set_caret(self, target: Optional[_AXTarget], python_index: int) -> bool:
        """Place a collapsed caret at a Python-string index."""
        if target is None or python_index < 0:
            return False
        snapshot = self._snapshot(target)
        if snapshot is None:
            return False
        current, _start, _end = snapshot
        if python_index > len(current):
            return False
        try:
            from ApplicationServices import (
                AXUIElementSetAttributeValue,
                AXValueCreate,
                kAXSelectedTextRangeAttribute,
                kAXValueTypeCFRange,
            )

            utf16_index = len(
                current[:python_index].encode("utf-16-le")
            ) // 2
            range_value = AXValueCreate(kAXValueTypeCFRange, (utf16_index, 0))
            if range_value is None:
                return False
            return AXUIElementSetAttributeValue(
                target.element,
                kAXSelectedTextRangeAttribute,
                range_value,
            ) == 0
        except Exception as exc:  # noqa: BLE001
            logger.debug("恢复语音输入后的光标位置失败: %s", exc)
            return False

    @staticmethod
    def _remove_visual_placeholder(
        current: str,
        placeholder: str,
        start: int,
        length: int,
    ) -> tuple[str, int, int]:
        if (
            placeholder
            and current == placeholder
            and start in {0, len(current)}
            and length == 0
        ):
            return "", 0, 0
        return current, start, length

    def _visual_placeholder(
        self,
        target: _AXTarget,
        current: str,
        start: int,
        end: int,
    ) -> str:
        """Return a visual placeholder without relying on its language.

        Standards-compliant editors expose ``AXPlaceholderValue``. Some
        Electron editors instead surface the visual prompt as ``AXValue`` while
        their actual character count or parameterized text range is empty. Only
        treat a value as visual when the selection is collapsed at an empty
        boundary; real selected or mid-caret text is never discarded.
        """
        if not current or start != end or start not in {0, len(current)}:
            return ""
        try:
            from ApplicationServices import AXUIElementCopyAttributeValue

            for attribute in (
                "AXPlaceholderValue",
                "AXTitle",
                "AXDescription",
                "AXHelp",
            ):
                error, value = AXUIElementCopyAttributeValue(
                    target.element, attribute, None
                )
                if error == 0 and isinstance(value, str) and value == current:
                    return current

            # Chromium/Electron may omit AXPlaceholderValue but still report
            # zero real characters for the focused empty editor.
            error, count = AXUIElementCopyAttributeValue(
                target.element, "AXNumberOfCharacters", None
            )
            if error == 0 and isinstance(count, (int, float)) and int(count) == 0:
                return current
        except Exception:
            pass

        # Ask the editor for its actual text range. Chromium/Electron often
        # returns a visual placeholder through AXValue, while AXStringForRange
        # correctly reports an empty document. This metadata check is safe for
        # every app: non-empty real text is never classified as a placeholder.
        try:
            from ApplicationServices import (
                AXUIElementCopyParameterizedAttributeValue,
                AXValueCreate,
                kAXValueTypeCFRange,
            )

            full_range = AXValueCreate(
                kAXValueTypeCFRange,
                (0, len(current.encode("utf-16-le")) // 2),
            )
            if full_range is not None:
                error, actual = AXUIElementCopyParameterizedAttributeValue(
                    target.element,
                    "AXStringForRange",
                    full_range,
                    None,
                )
                if error == 0 and isinstance(actual, str) and not actual:
                    return current
        except Exception:
            pass

        if self._is_openai_desktop_target(target):
            # Last-resort compatibility for OpenAI builds that expose neither
            # placeholder nor text-range metadata. This is intentionally scoped
            # to an empty-boundary OpenAI editor.
            if current.strip().casefold() in {
                "do anything",
                "ask anything",
                "message chatgpt",
                "随心输入",
            }:
                return current
        if self._is_qoder_desktop_target(target):
            # Qoder 1.21.x exposes this localized visual prompt as AXValue and
            # may omit both AXPlaceholderValue and parameterized text ranges.
            # Match its structural prompt markers rather than all empty-boundary
            # strings, so genuine user text is never discarded.
            normalized = current.strip().casefold()
            if (
                ("@ 添加上下文" in current and "/ 使用命令" in current)
                or ("@ add context" in normalized and "/ use commands" in normalized)
                or normalized in {
                    "追加需求或提问",
                    "add requirements or ask questions",
                    "ask a follow-up question",
                }
            ):
                return current
        return ""

    @staticmethod
    def _target_bundle_identifier(target: _AXTarget) -> str:
        try:
            from AppKit import NSRunningApplication

            application = NSRunningApplication.runningApplicationWithProcessIdentifier_(
                target.pid
            )
            return str(application.bundleIdentifier() or "") if application else ""
        except Exception:
            return ""

    @classmethod
    def target_bundle_identifier(cls, target: _AXTarget) -> str:
        """Expose the captured app identity to the final insertion path."""
        return cls._target_bundle_identifier(target)

    @classmethod
    def _is_openai_desktop_target(cls, target: _AXTarget) -> bool:
        return cls._target_bundle_identifier(target).startswith("com.openai.")

    @classmethod
    def _is_qoder_desktop_target(cls, target: _AXTarget) -> bool:
        return cls._target_bundle_identifier(target).startswith("com.qoder.ide")

    @classmethod
    def prefers_value_write(cls, target: _AXTarget) -> bool:
        """Use committed AXValue writes for Electron chat composers.

        AXSelectedText can leave text in a marked/composition state in these
        apps, which looks inserted but cannot be edited until another key is
        pressed. AXValue commits the complete value immediately.
        """
        return cls._is_openai_desktop_target(target) or cls._is_qoder_desktop_target(
            target
        )

    @classmethod
    def uses_event_text_input(cls, target: _AXTarget) -> bool:
        """Use login-session text events for Chromium/Electron editors."""
        bundle_id = cls._target_bundle_identifier(target)
        return bundle_id.startswith(
            (
                "com.qoder.ide",
                "com.openai.",
                "com.microsoft.VSCode",
                "com.todesktop.",
            )
        )

    def insert_text_event(self, target: _AXTarget, replacement: str) -> bool:
        """Insert Unicode through the current login session event pipeline.

        Unlike AXValue, this produces the real input events Chromium needs to
        update its JavaScript editor model. Space is only a carrier key. If an
        app ignores the Unicode payload, the literal carrier is detected and
        removed instead of leaking an ``a`` or corrupting the editor.
        """
        if not replacement or not self.target_is_focused(target):
            return False
        before = self._snapshot(target)
        if before is None:
            return False
        before_text, before_start, before_end = before
        if before_start != before_end:
            return False
        try:
            from Quartz import (
                CGEventCreateKeyboardEvent,
                CGEventKeyboardSetUnicodeString,
                CGEventPost,
                CGEventSourceCreate,
                kCGEventSourceStateCombinedSessionState,
                kCGSessionEventTap,
            )

            source = CGEventSourceCreate(kCGEventSourceStateCombinedSessionState)
            if source is None:
                return False
            key_down = CGEventCreateKeyboardEvent(source, 49, True)
            key_up = CGEventCreateKeyboardEvent(source, 49, False)
            if key_down is None or key_up is None:
                return False
            utf16_length = len(replacement.encode("utf-16-le")) // 2
            CGEventKeyboardSetUnicodeString(key_down, utf16_length, replacement)
            CGEventPost(kCGSessionEventTap, key_down)
            CGEventPost(kCGSessionEventTap, key_up)
            time.sleep(0.08)
        except Exception as exc:  # noqa: BLE001
            logger.debug("Electron Unicode 文字事件写入失败: %s", exc)
            return False

        after = self._snapshot(target)
        if after is None:
            return False
        after_text, after_start, after_end = after
        expected = before_text[:before_start] + replacement + before_text[before_end:]
        expected_caret = before_start + len(replacement)
        if (
            after_text == expected
            and after_start == after_end
            and after_start == expected_caret
        ):
            return True

        raw_carrier = before_text[:before_start] + " " + before_text[before_end:]
        if (
            after_text != raw_carrier
            or after_start != after_end
            or after_start != before_start + 1
            or not self.target_is_focused(target)
        ):
            return False

        # Unicode was ignored and only the harmless carrier appeared. Restore
        # the exact prior value with one ordinary Backspace.
        try:
            delete_down = CGEventCreateKeyboardEvent(source, 51, True)
            delete_up = CGEventCreateKeyboardEvent(source, 51, False)
            if delete_down is None or delete_up is None:
                return False
            CGEventPost(kCGSessionEventTap, delete_down)
            CGEventPost(kCGSessionEventTap, delete_up)
            time.sleep(0.05)
        except Exception as exc:  # noqa: BLE001
            logger.debug("恢复 Electron 载体字符失败: %s", exc)
            return False

        restored = self._snapshot(target)
        if restored is None or restored[0] != before_text:
            logger.warning("Electron 载体字符未能完整恢复")
        # The requested text was not accepted. Even when the harmless carrier
        # was removed successfully, the caller must retain the transcript for
        # recovery instead of treating cleanup as a successful insertion.
        return False

    @staticmethod
    def _utf16_to_python_index(text: str, utf16_index: int) -> int:
        if utf16_index <= 0:
            return 0
        encoded = text.encode("utf-16-le")[: utf16_index * 2]
        return len(encoded.decode("utf-16-le", errors="ignore"))

    def _snapshot(self, target: _AXTarget) -> Optional[tuple[str, int, int]]:
        try:
            ax = self._ax_modules()
            error, value = ax["copy"](target.element, ax["value"], None)
            if error != 0 or not isinstance(value, str):
                return None
            error, range_value = ax["copy"](target.element, ax["selection"], None)
            if error != 0 or range_value is None:
                return None
            success, selected_range = ax["get_value"](
                range_value, ax["range_type"], None
            )
            if not success:
                return None
            location, length = int(selected_range[0]), int(selected_range[1])
            start = self._utf16_to_python_index(value, location)
            end = self._utf16_to_python_index(value, location + length)
            current = str(value)
            placeholder = self._visual_placeholder(target, current, start, end)
            current, start, length = self._remove_visual_placeholder(
                current,
                placeholder,
                start,
                end - start,
            )
            return current, start, start + length
        except Exception as exc:  # noqa: BLE001
            logger.debug("读取自动纠错观察区域失败: %s", exc)
            return None

    def observe_after_paste(self, inserted_text: str, target: Optional[_AXTarget]) -> None:
        if not self.enabled or not inserted_text or target is None:
            return
        with self._generation_lock:
            self._generation += 1
            generation = self._generation
        threading.Thread(
            target=self._observe,
            args=(generation, target, inserted_text),
            name="correction-observer",
            daemon=True,
        ).start()

    def _current_generation(self) -> int:
        with self._generation_lock:
            return self._generation

    def _extract_candidate(
        self,
        current: str,
        selection_start: int,
        prefix: str,
        suffix: str,
        original_start: int,
    ) -> Optional[str]:
        if prefix:
            lower = max(0, original_start - ANCHOR_LENGTH * 3)
            upper = min(len(current), original_start + MAX_OBSERVED_SPAN)
            prefix_position = current.rfind(prefix, lower, upper)
            if prefix_position < 0:
                return None
            start = prefix_position + len(prefix)
        else:
            start = 0

        if suffix:
            end = current.find(suffix, start)
            if end < 0:
                return None
        else:
            end = selection_start
            if end < start:
                return None
        if end - start > MAX_OBSERVED_SPAN:
            return None
        return current[start:end]

    def _observe(self, generation: int, target: _AXTarget, inserted_text: str) -> None:
        # Give paste and the target application time to update its AX value.
        time.sleep(0.25)
        baseline = self._snapshot(target)
        if baseline is None:
            logger.debug("当前输入框不支持读取文本范围，已跳过本次自动纠错学习")
            return
        baseline_text, caret_start, _ = baseline
        inserted_start = caret_start - len(inserted_text)
        if inserted_start < 0 or baseline_text[inserted_start:caret_start] != inserted_text:
            # Some web views expose a container rather than the actual editable text.
            nearby_start = max(0, caret_start - len(inserted_text) - 8)
            located = baseline_text.rfind(inserted_text, nearby_start, caret_start + 1)
            if located < 0:
                logger.debug("无法在输入框内定位刚插入的文本，已跳过本次学习")
                return
            inserted_start = located
            caret_start = located + len(inserted_text)

        prefix = baseline_text[max(0, inserted_start - ANCHOR_LENGTH):inserted_start]
        suffix = baseline_text[caret_start:caret_start + ANCHOR_LENGTH]
        last_candidate = inserted_text
        last_change = time.monotonic()
        observed_change = False
        deadline = time.monotonic() + 45.0

        while time.monotonic() < deadline and generation == self._current_generation():
            time.sleep(0.5)
            snapshot = self._snapshot(target)
            if snapshot is None:
                break
            current, selection_start, _ = snapshot
            candidate = self._extract_candidate(
                current, selection_start, prefix, suffix, inserted_start
            )
            if candidate is None:
                continue
            if candidate != last_candidate:
                last_candidate = candidate
                last_change = time.monotonic()
                observed_change = candidate != inserted_text
            if observed_change and time.monotonic() - last_change >= 2.0:
                break

        if not observed_change or last_candidate == inserted_text:
            return
        pairs = extract_correction_pairs(inserted_text, last_candidate)
        for wrong, correct in pairs:
            self.store.record(wrong, correct)
            logger.info("已自动学习一条语音纠错（具体词汇仅保存在本机纠错词库）")


def sys_platform() -> str:
    # Wrapped for small, platform-independent unit tests.
    import sys

    return sys.platform
