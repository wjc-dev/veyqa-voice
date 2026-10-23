"""Recognize a standalone modifier without consuming macOS shortcuts."""
import threading


class ModifierGesture:
    HOLD_DELAY = 0.18

    def __init__(self, *, mode, start, stop, cancel, toggle, lock=None, hold_delay=None):
        self.mode = mode
        self.start = start
        self.stop = stop
        self.cancel = cancel
        self.toggle = toggle
        self.lock = lock or threading.RLock()
        self.hold_delay = self.HOLD_DELAY if hold_delay is None else hold_delay
        self.down = False
        self.blocked = False
        self.started = False
        self.timer = None
        self.generation = 0

    def _cancel_timer(self):
        self.generation += 1
        if self.timer is not None:
            self.timer.cancel()
            self.timer = None

    def press(self, *, blocked=False):
        with self.lock:
            if self.down:
                return
            self.down = True
            self.blocked = blocked
            self.started = False
            if blocked or self.mode != "hold":
                return
            self.generation += 1
            generation = self.generation

            def begin():
                with self.lock:
                    if generation != self.generation or not self.down or self.blocked:
                        return
                    self.timer = None
                    self.started = bool(self.start())

            self.timer = threading.Timer(self.hold_delay, begin)
            self.timer.daemon = True
            self.timer.start()

    def chord(self):
        with self.lock:
            if not self.down:
                return
            self.blocked = True
            self._cancel_timer()
            if self.started:
                self.started = False
                self.cancel()

    def release(self, *, confirmed=True):
        with self.lock:
            if not self.down:
                return
            self._cancel_timer()
            self.down = False
            if self.started:
                self.started = False
                self.stop()
            elif self.mode == "toggle" and not self.blocked and confirmed:
                self.toggle()
            self.blocked = False

    def reset(self):
        """Lost events, shortcut capture, and config changes never count as taps."""
        with self.lock:
            self._cancel_timer()
            self.down = False
            self.blocked = False
            if self.started:
                self.started = False
                self.cancel()
