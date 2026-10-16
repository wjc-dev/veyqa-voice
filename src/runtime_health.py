"""Bounded functional recovery; never infer failure from a quiet keyboard."""

from __future__ import annotations

import json
import os
import threading
import time
from pathlib import Path

from .diagnostics import DiagnosticsStore
from .runtime_paths import DATA_DIR
from .utils.logger import logger


class RuntimeHealth:
    def __init__(self, probe, recover, *, path: Path | None = None, clock=time.monotonic):
        self.probe = probe
        self.recover = recover
        self.clock = clock
        self.path = path or DATA_DIR / "health.json"
        # Ephemeral heartbeat needs atomic visibility, not persistent disk flushes.
        self.store = DiagnosticsStore(self.path, durable=False)
        self.main_tick = clock()
        self._last_poll = clock()
        self._recovering = False
        self._last_write = float("-inf")
        self._last_fault = None
        self._stop = threading.Event()

    def tick_main(self):
        self.main_tick = self.clock()

    def poll(self):
        now = self.clock()
        # Give the UI and device services time to return after sleep/suspension.
        if now - self._last_poll > 10:
            self.main_tick = now
            self._last_poll = now
            return
        self._last_poll = now
        reason = self.probe(now)
        if now - self.main_tick > 20:
            reason = "应用事件循环无响应"
        if now - self._last_write >= 10 or reason != self._last_fault:
            self.store.update(pid=os.getpid(), timestamp=time.time(), fault=reason or "")
            self._last_write = now
            self._last_fault = reason
        if reason and not self._recovering:
            self._recovering = True
            self.recover(reason)

    def start(self):
        def run():
            while not self._stop.wait(2):
                try:
                    self.poll()
                except Exception:
                    logger.exception("后台健康检查失败")
        threading.Thread(target=run, name="runtime-health", daemon=True).start()


def allow_automatic_restart(path: Path | None = None, *, now: float | None = None) -> bool:
    """Persist a rolling budget across exec; never loop indefinitely on a bad device."""
    path = path or DATA_DIR / "automatic-restarts.json"
    now = time.time() if now is None else now
    try:
        entries = json.loads(path.read_text())
    except (OSError, ValueError):
        entries = []
    if not isinstance(entries, list):
        entries = []
    recent = [x for x in entries if isinstance(x, (int, float)) and 0 <= now - x < 300]
    if len(recent) >= 3:
        return False
    # Atomic private write; data contains timestamps only.
    DiagnosticsStore(path)._save(recent + [now])
    return True
