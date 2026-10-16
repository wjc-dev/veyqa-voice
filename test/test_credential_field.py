"""Source contracts for the native credential UI; actual clicks are a GUI check."""
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
SOURCE = (ROOT / "native_settings/VoiceInputSettings.swift").read_text()


class CredentialFieldTests(unittest.TestCase):
    def component(self):
        return SOURCE.split("struct CredentialField: View {", 1)[1].split(
            "struct EngineSettingsView: View {", 1
        )[0]

    def test_both_providers_share_eye_and_copy_control(self):
        self.assertIn('CredentialField("API Key", text: $model.qwenAPIKey)', SOURCE)
        self.assertIn('CredentialField("Access Token", text: $model.doubaoAccessKey)', SOURCE)
        self.assertNotIn('Toggle("显示 API Key"', SOURCE)
        self.assertNotIn('Toggle("显示 Access Token"', SOURCE)

    def test_masked_by_default_and_hidden_when_leaving(self):
        component = self.component()
        self.assertIn("@State private var isRevealed = false", component)
        self.assertIn('SecureField(title, text: $text)', component)
        self.assertIn('isRevealed ? "eye.slash" : "eye"', component)
        self.assertIn(".onDisappear { conceal() }", component)
        self.assertIn("NSApplication.didResignActiveNotification", component)

    def test_copy_only_on_explicit_action_without_exposing_value_in_feedback(self):
        component = self.component()
        self.assertIn("Button(action: copyCredential)", component)
        self.assertIn(".disabled(text.isEmpty)", component)
        self.assertIn("pasteboard.setString(text, forType: .string)", component)
        self.assertIn('"已复制" : "复制失败"', component)
        self.assertEqual(component.count("NSPasteboard.general"), 1)
        self.assertNotIn("print(", component)
        self.assertNotIn("logger", component)


if __name__ == "__main__":
    unittest.main()
