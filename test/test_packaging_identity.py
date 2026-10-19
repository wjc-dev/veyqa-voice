import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class PackagingIdentityTests(unittest.TestCase):
    def test_release_version_is_consistent(self):
        main = (ROOT / "main.py").read_text(encoding="utf-8")
        spec = (ROOT / "packaging" / "VoiceInput.spec").read_text(encoding="utf-8")
        build = (ROOT / "build-internal-dmg.command").read_text(encoding="utf-8")
        source_package = (ROOT / "package-source.command").read_text(encoding="utf-8")
        settings_info = (
            ROOT / "native_settings" / "VoiceInputSettings-Info.plist"
        ).read_text(encoding="utf-8")

        version = re.search(r'__version__ = "([0-9.]+)"', main).group(1)
        self.assertIn(f'version="{version}"', spec)
        self.assertIn(f'"CFBundleShortVersionString": "{version}"', spec)
        self.assertIn(f'VERSION="{version}"', build)
        self.assertIn(f'PACKAGE_NAME="Veyqa-Voice-v{version}"', source_package)
        self.assertIn(f"<string>{version}</string>", settings_info)

    def test_main_app_is_a_regular_dock_application(self):
        spec = (ROOT / "packaging" / "VoiceInput.spec").read_text(encoding="utf-8")
        controller = (ROOT / "src" / "ui" / "status_bar.py").read_text(
            encoding="utf-8"
        )

        self.assertIn('"LSUIElement": False', spec)
        self.assertIn("NSApplicationActivationPolicyRegular", controller)
        self.assertNotIn("NSApplicationActivationPolicyAccessory", controller)
        self.assertIn('SHOW_MENU_BAR_ICON", "false"', controller)

    def test_current_app_and_login_item_use_stable_qwen_identity(self):
        spec = (ROOT / "packaging" / "VoiceInput.spec").read_text(encoding="utf-8")
        settings = (ROOT / "native_settings" / "VoiceInputSettings.swift").read_text(
            encoding="utf-8"
        )
        build = (ROOT / "build-internal-dmg.command").read_text(encoding="utf-8")

        self.assertIn('bundle_identifier="com.wjcdev.veyqa"', spec)
        agent = (ROOT / "packaging" / "com.wjcdev.veyqa.agent.plist").read_text(
            encoding="utf-8"
        )
        self.assertIn("com.wjcdev.veyqa.agent", agent)
        self.assertIn("Contents/Resources/VeyqaSupervisor", agent)
        self.assertIn('AGENT_PLIST_NAME = "com.wjcdev.veyqa.agent.plist"', (
            ROOT / "src" / "login_item.py"
        ).read_text(encoding="utf-8"))
        self.assertIn('--identifier "com.wjcdev.veyqa"', build)

    def test_old_identity_is_only_kept_for_login_item_migration(self):
        spec = (ROOT / "packaging" / "VoiceInput.spec").read_text(encoding="utf-8")
        build = (ROOT / "build-internal-dmg.command").read_text(encoding="utf-8")

        self.assertNotIn("com.voiceinputnext.app", spec)
        self.assertNotIn("com.voiceinputnext.app", build)
        self.assertNotIn("com.voiceinputnext.qwen", spec)
        self.assertNotIn("com.voiceinputnext.qwen", build)

    def test_packaging_cleans_nested_app_bundles_by_default(self):
        build = (ROOT / "build-internal-dmg.command").read_text(encoding="utf-8")

        self.assertIn("trap cleanup EXIT", build)
        self.assertIn('rm -rf "$WORK_DIR"', build)
        self.assertIn("VOICE_INPUT_KEEP_PACKAGE_BUILD", build)

    def test_permission_identity_survives_main_process_restart(self):
        settings = (ROOT / "native_settings" / "VoiceInputSettings.swift").read_text(
            encoding="utf-8"
        )

        self.assertNotIn("permissionPID == Int(parentPID)", settings)
        self.assertIn("Darwin.kill(Int32(permissionPID), 0) == 0", settings)
        self.assertIn('appendingPathComponent("Contents/MacOS/VeyqaVoice")', settings)

    def test_packaging_embeds_modern_service_management_agent(self):
        build = (ROOT / "build-internal-dmg.command").read_text(encoding="utf-8")
        native_build = (ROOT / "build-native-settings.command").read_text(
            encoding="utf-8"
        )
        spec = (ROOT / "packaging" / "VoiceInput.spec").read_text(encoding="utf-8")
        agent = ROOT / "packaging" / "com.wjcdev.veyqa.agent.plist"

        self.assertTrue(agent.exists())
        self.assertIn("Contents/Library/LaunchAgents", build)
        self.assertIn("VeyqaSupervisor", build)
        supervisor = (ROOT / "native_settings" / "VeyqaSupervisor.swift").read_text(
            encoding="utf-8"
        )
        settings = (ROOT / "native_settings" / "VoiceInputSettings.swift").read_text(
            encoding="utf-8"
        )
        self.assertIn(".supervisor-paused", supervisor)
        self.assertIn("paused-by-user", settings)
        self.assertIn("NSRunningApplication.runningApplications", supervisor)
        self.assertIn("_NSGetExecutablePath", supervisor)
        self.assertIn("urlForApplication", supervisor)
        self.assertIn('URL(fileURLWithPath: "/Applications/Veyqa Voice.app")', supervisor)
        self.assertIn("Bundle(url: resolved)?.bundleIdentifier", supervisor)
        self.assertNotIn('executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")', supervisor)
        self.assertIn("native_settings/SettingsCore.swift", native_build)
        self.assertIn("Contents/Helpers/VeyqaSettings.app", build)
        self.assertNotIn("VoiceInputSettings.appbin", build)
        self.assertNotIn("VoiceInputSettings.appbin", spec)

    def test_uv_created_environment_does_not_require_bundled_pip(self):
        build = (ROOT / "build-internal-dmg.command").read_text(encoding="utf-8")

        self.assertIn("uv pip install --python", build)
        self.assertIn("-m ensurepip --upgrade", build)
        self.assertNotIn("-m pip show pyinstaller", build)

    def test_release_verifier_is_isolated_from_installed_app(self):
        verifier = (
            ROOT / "tools" / "verify-release-candidate.command"
        ).read_text(encoding="utf-8")

        self.assertIn("VOICE_INPUT_DISABLE_LOGIN_SYNC=true", verifier)
        self.assertIn("系统注册型录音快捷键已启用", verifier)
        self.assertIn("Stressing the bundled settings helper startup five times", verifier)
        self.assertIn("Contents/Helpers/VeyqaSettings.app", verifier)
        self.assertIn("Triggering two registered hotkeys in the frozen app", verifier)
        self.assertIn("post-hotkey-event.py", verifier)
        self.assertIn('pgrep -f -x "$SETTINGS_EXECUTABLE"', verifier)
        self.assertIn("Hotkey cycle settings helper did not finish startup", verifier)
        self.assertIn("dock entry ready; menu bar disabled", verifier)
        self.assertIn("mktemp -d", verifier)
        self.assertNotIn("/Applications/Veyqa Voice.app", verifier)
        self.assertNotIn("SMAppService", verifier)

    def test_dead_permission_process_does_not_trap_the_user(self):
        settings = (ROOT / "native_settings" / "VoiceInputSettings.swift").read_text(
            encoding="utf-8"
        )

        self.assertIn('Button("关闭设置窗口")', settings)
        self.assertIn('"重新打开 App"', settings)
        self.assertIn('return "当前主程序已退出"', settings)
        self.assertIn("继续当前设置", settings)


if __name__ == "__main__":
    unittest.main()
