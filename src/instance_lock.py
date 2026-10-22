"""Keep one microphone and hotkey owner for each runtime data directory."""
from contextlib import contextmanager
import fcntl
import os
from pathlib import Path


@contextmanager
def app_instance(directory: Path):
    # Keep the inode in place: unlinking a lock file can split concurrent owners.
    # Python opens non-inheritable descriptors, so audio workers do not retain it.
    descriptor = os.open(directory / ".app-instance.lock",
                         os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            yield False
            return
        yield True
    finally:
        # Closing also releases the lock after normal exit, exceptions or crashes.
        os.close(descriptor)
