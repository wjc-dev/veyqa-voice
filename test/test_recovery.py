import tempfile
import threading
import unittest
from unittest.mock import patch
from pathlib import Path

from src.recovery import RecoveryStore


class RecoveryStoreTests(unittest.TestCase):
    def test_invalid_existing_file_is_not_overwritten_by_add_or_delete(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "recovery.json"
            invalid = '{"interrupted":'
            path.write_text(invalid)
            store = RecoveryStore(path)
            for mutate in (lambda: store.add("新文字", "失败"), lambda: store.delete("old")):
                with self.assertRaises(ValueError):
                    mutate()
                self.assertEqual(path.read_text(), invalid)

    def test_keeps_only_the_latest_private_results(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "recovery.json"
            store = RecoveryStore(path, limit=2)
            store.add("第一条", "失焦")
            store.add("第二条", "失败")
            store.add("第三条", "失败")

            self.assertEqual([entry["text"] for entry in store.load()], ["第三条", "第二条"])
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_delete_and_clear(self):
        with tempfile.TemporaryDirectory() as directory:
            store = RecoveryStore(Path(directory) / "recovery.json")
            store.add("可恢复文字", "失焦")
            entry_id = store.load()[0]["id"]
            self.assertTrue(store.delete(entry_id))
            self.assertEqual(store.load(), [])
            store.add("再次失败", "失败")
            store.clear()
            self.assertEqual(store.load(), [])

    def test_delete_selected_ids_preserves_newer_unseen_text(self):
        with tempfile.TemporaryDirectory() as directory:
            store = RecoveryStore(Path(directory) / "recovery.json")
            store.add("旧文字", "失败")
            selected = {store.load()[0]["id"]}
            store.add("刚刚恢复的新文字", "失败")
            store.delete_many(selected)
            self.assertEqual([x["text"] for x in store.load()], ["刚刚恢复的新文字"])

    def test_separate_writers_serialize_read_modify_write(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "recovery.json"
            first, second = RecoveryStore(path), RecoveryStore(path)
            saving, release, second_done = threading.Event(), threading.Event(), threading.Event()
            original_save = first._save
            errors = []
            def delayed_save(entries):
                saving.set()
                release.wait(2)
                original_save(entries)
            def add(store, text, done=None):
                try:
                    store.add(text, "测试")
                except Exception as error:
                    errors.append(error)
                finally:
                    if done:
                        done.set()
            with patch.object(first, "_save", side_effect=delayed_save):
                a = threading.Thread(target=add, args=(first, "第一条"))
                b = threading.Thread(target=add, args=(second, "第二条", second_done))
                a.start()
                self.assertTrue(saving.wait(1))
                b.start()
                was_serialized = not second_done.wait(0.05)
                release.set()
                a.join(2)
                b.join(2)
            self.assertEqual(errors, [])
            self.assertTrue(was_serialized)
            self.assertEqual({x["text"] for x in first.load()}, {"第一条", "第二条"})


if __name__ == "__main__":
    unittest.main()
