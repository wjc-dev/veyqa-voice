import subprocess
import sys
from unittest.mock import patch

from src.instance_lock import app_instance


def child_acquires(directory):
    script = ("from pathlib import Path; from src.instance_lock import app_instance; "
              "import sys;\nwith app_instance(Path(sys.argv[1])) as acquired: print(acquired)")
    return subprocess.check_output([sys.executable, "-c", script, str(directory)], text=True, timeout=5).strip()


def test_competing_process_is_rejected_and_next_launch_can_acquire(tmp_path):
    with app_instance(tmp_path) as acquired:
        assert acquired
        assert child_acquires(tmp_path) == "False"
    assert child_acquires(tmp_path) == "True"


def test_crash_releases_lock_without_deleting_the_file(tmp_path):
    script = ("from pathlib import Path; from src.instance_lock import app_instance; "
              "import sys;\nwith app_instance(Path(sys.argv[1])) as acquired:\n"
              " print(acquired, flush=True)\n sys.stdin.read()")
    process = subprocess.Popen([sys.executable, "-c", script, str(tmp_path)],
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        assert process.stdout.readline().strip() == "True"
        process.kill()
        process.wait(timeout=5)
        assert (tmp_path / ".app-instance.lock").exists()
        with app_instance(tmp_path) as acquired:
            assert acquired
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        process.stdin.close()
        process.stdout.close()


def test_duplicate_start_never_initializes_microphone_or_listener(tmp_path):
    import main
    with app_instance(tmp_path):
        with patch.object(main, "DATA_DIR", tmp_path), patch.object(main, "_run_application") as run:
            main.main()
    run.assert_not_called()
