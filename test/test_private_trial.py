import asyncio
import importlib.util
import json
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest
from unittest.mock import AsyncMock, MagicMock, patch
from types import SimpleNamespace
import aiohttp
import zipfile

from src.transcription.private_trial import PrivateTrialBroker, MODEL
from src.transcription.builtin import BuiltinStreamingProcessor
from src.transcription.promotion import MODEL as REMOTE_MODEL
from src.transcription.private_trial_payload import FILENAME, decode, encode

ROOT = Path(__file__).resolve().parents[1]
KEY = "sk-test-private-trial-never-a-real-key"
HOST = "ws-test.cn-beijing.maas.aliyuncs.com"


def load_packaging_module(name):
    sys.path.insert(0, str(ROOT / "packaging"))
    try:
        spec = importlib.util.spec_from_file_location(name, ROOT / "packaging" / f"{name}.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        sys.path.pop(0)


class PrivateTrialTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.payload = encode(KEY, HOST)
        (self.root / FILENAME).write_bytes(self.payload)

    def broker(self):
        broker = PrivateTrialBroker(directory=self.root, resource_root=self.root)
        self.addCleanup(broker._pool.shutdown)
        return broker

    def test_encoded_payload_has_no_plaintext_key_but_is_explicitly_recoverable(self):
        self.assertNotIn(KEY.encode(), self.payload)
        self.assertEqual(decode(self.payload).key, KEY)
        self.assertNotIn(KEY, repr(decode(self.payload)))
        self.assertNotEqual(self.payload, encode(KEY, HOST))

    def test_promotion_uses_message_model_without_changing_personal_or_remote_defaults(self):
        from src.transcription.speech_models import DEFAULT_MODELS
        from src.transcription.qwen_streaming import QwenStreamingProcessor
        import os

        with patch.dict(os.environ, {"QWEN_ASR_MODEL": "qwen-audio-3.1-asr-flash-streaming"}):
            processor = BuiltinStreamingProcessor(self.broker())
            personal = QwenStreamingProcessor()
        request = processor._run_task_event(16000, "test")["payload"]
        self.assertEqual(request["model"], "qwen-audio-3.1-asr-flash-message")
        self.assertEqual(processor.model_name, MODEL)
        self.assertTrue(request["parameters"]["intermediate_result_enabled"])
        self.assertFalse(request["parameters"]["disfluency_removal_enabled"])
        self.assertEqual(personal.model_name, "qwen-audio-3.1-asr-flash-streaming")
        self.assertEqual(REMOTE_MODEL, "qwen-audio-3.0-asr-flash-streaming")
        self.assertEqual(DEFAULT_MODELS["qwen"], REMOTE_MODEL)

    def test_payload_corruption_and_nonofficial_endpoint_rejected(self):
        for value in (b"", b"plain text", self.payload[:-1], self.payload[:-1] + b"x"):
            with self.assertRaises(ValueError):
                decode(value)
        with self.assertRaises(ValueError):
            encode(KEY, "attacker.example")

    def test_direct_get_needs_no_device_registration_network_or_env_edits(self):
        env = self.root / ".env"
        env.write_text("QWEN_API_KEY=keep-existing\n")
        broker = self.broker()
        with patch("src.transcription.promotion.CredentialBroker._http", side_effect=AssertionError("no server")):
            self.assertIsNone(broker.prefetch())
            self.assertEqual(broker.get().token, KEY)
            broker.report("test", 10, 0)
        self.assertEqual(env.read_text(), "QWEN_API_KEY=keep-existing\n")
        self.assertFalse((self.root / "promotion-usage.json").exists())
        self.assertNotIn(KEY, repr(broker.get()))

    def test_connection_check_never_shares_credential_with_settings(self):
        broker = self.broker()
        with patch.object(broker, "_probe", new=AsyncMock()):
            broker.request_check("request-1").result(timeout=2)
        text = (self.root / "promotion.json").read_text()
        self.assertNotIn(KEY, text)
        self.assertTrue(json.loads(text)["enabled"])
        self.assertEqual(json.loads(text)["request_id"], "request-1")
        self.assertEqual(json.loads(text)["model"], "qwen-audio-3.1-asr-flash-message")
        with patch.object(broker, "_probe", new=AsyncMock(side_effect=RuntimeError(KEY))):
            broker.request_check("request-2").result(timeout=2)
        text = (self.root / "promotion.json").read_text()
        self.assertNotIn(KEY, text)
        self.assertFalse(json.loads(text)["enabled"])

    def test_connection_check_waits_for_model_acceptance_after_task_started(self):
        broker = self.broker()
        for terminal, accepted in (("task-finished", True), ("task-failed", False)):
            socket = AsyncMock()
            socket.receive.side_effect = [SimpleNamespace(
                type=aiohttp.WSMsgType.TEXT,
                data=json.dumps({"header": {"event": event}}),
            ) for event in ("task-started", terminal)]
            connection = MagicMock()
            connection.__aenter__ = AsyncMock(return_value=socket)
            session = MagicMock()
            session.ws_connect.return_value = connection
            context = MagicMock()
            context.__aenter__ = AsyncMock(return_value=session)
            with patch("src.transcription.private_trial.aiohttp.ClientSession", return_value=context):
                if accepted:
                    asyncio.run(broker._probe())
                else:
                    with self.assertRaises(RuntimeError):
                        asyncio.run(broker._probe())
            request = socket.send_json.call_args_list[0].args[0]
            self.assertEqual(request["payload"]["model"], "qwen-audio-3.1-asr-flash-message")
            socket.send_bytes.assert_awaited_once_with(bytes(3200))

    def test_injection_separates_helper_and_disables_public_updates(self):
        configure = load_packaging_module("configure_private_trial")
        app = self.root / "Veyqa Voice.app"
        resources = app / "Contents/Resources"
        helper = app / "Contents/Helpers/VeyqaSettings.app/Contents/Resources"
        frameworks = app / "Contents/Frameworks"
        for path in (resources, helper, frameworks):
            path.mkdir(parents=True, exist_ok=True)
        info_path = app / "Contents/Info.plist"
        info_path.write_bytes(plistlib.dumps({"SUFeedURL": "https://example.test/appcast.xml"}))
        configure.inject(app, self.root / FILENAME)
        info = plistlib.loads(info_path.read_bytes())
        self.assertEqual(info["VeyqaDistribution"], "private-trial")
        self.assertNotIn("SUFeedURL", info)
        self.assertFalse(info["SUEnableAutomaticChecks"])
        self.assertTrue(json.loads((helper / "release-features.json").read_text())["private_trial"])
        self.assertFalse((helper / FILENAME).exists())
        self.assertEqual(decode((frameworks / FILENAME).read_bytes()).key, KEY)

    def test_public_feed_rejects_private_marker_or_payload_before_signing(self):
        module = load_packaging_module("generate_update_feed")
        for has_marker in (True, False):
            archive = self.root / f"trial-{has_marker}.zip"
            with zipfile.ZipFile(archive, "w") as output:
                info = {"VeyqaDistribution": "private-trial"} if has_marker else {}
                output.writestr("Veyqa Voice.app/Contents/Info.plist", plistlib.dumps(info))
                if not has_marker:
                    output.writestr("Veyqa Voice.app/Contents/Resources/" + FILENAME, self.payload)
            with patch.object(module, "prepare", side_effect=AssertionError("must reject first")):
                with self.assertRaisesRegex(ValueError, "Private trial"):
                    module.generate(archive, self.root / "appcast.xml", self.root / "notes.md")

    def test_explicit_public_promotion_keeps_signed_automatic_updates(self):
        configure = load_packaging_module("configure_private_trial")
        config = json.loads((ROOT / "packaging/sparkle.json").read_text())
        app = self.root / "Public.app"
        for path in ("Resources", "Helpers/VeyqaSettings.app/Contents/Resources", "Frameworks"):
            (app / "Contents" / path).mkdir(parents=True, exist_ok=True)
        info_path = app / "Contents/Info.plist"
        info_path.write_bytes(plistlib.dumps({
            "SUFeedURL": config["feed_url"], "SUPublicEDKey": config["public_key"],
            "SURequireSignedFeed": True, "SUVerifyUpdateBeforeExtraction": True,
            "SUEnableAutomaticChecks": True,
        }))
        configure.inject(app, self.root / FILENAME, public_promotion=True)
        info = plistlib.loads(info_path.read_bytes())
        self.assertEqual(info["VeyqaDistribution"], "public-promotion")
        self.assertEqual(info["SUFeedURL"], config["feed_url"])
        self.assertTrue(info["SUEnableAutomaticChecks"])

    def test_public_promotion_requires_explicit_feed_opt_in_and_valid_config(self):
        module = load_packaging_module("generate_update_feed")
        config = json.loads((ROOT / "packaging/sparkle.json").read_text())
        info = dict(VeyqaDistribution="public-promotion",
                    SUFeedURL=config["feed_url"], SUPublicEDKey=config["public_key"],
                    SURequireSignedFeed=True, SUVerifyUpdateBeforeExtraction=True,
                    SUEnableAutomaticChecks=True, VeyqaSigningProfile="local")
        archive = self.root / "public.zip"
        def write():
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("Veyqa Voice.app/Contents/Info.plist", plistlib.dumps(info))
                output.writestr("Veyqa Voice.app/Contents/Resources/" + FILENAME, self.payload)
        write()
        with self.assertRaisesRegex(ValueError, "explicit"):
            module.generate(archive, self.root / "feed", self.root / "notes")
        with patch.object(module, "prepare", side_effect=RuntimeError("reached signing")):
            with self.assertRaisesRegex(RuntimeError, "reached signing"):
                module.generate(archive, self.root / "feed", self.root / "notes", allow_public_promotion=True)
            info["SUFeedURL"] = "https://unrelated.example/feed"
            write()
            with self.assertRaises(ValueError):
                module.generate(archive, self.root / "feed", self.root / "notes", allow_public_promotion=True)
