# -*- mode: python ; coding: utf-8 -*-

from pathlib import Path

project = Path(SPEC).resolve().parents[1]

a = Analysis(
    [str(project / "main.py")],
    pathex=[str(project)],
    binaries=[],
    datas=[
        (str(project / "env.example"), "."),
        (str(project / "packaging" / "StatusBarIcon.png"), "."),
    ],
    hiddenimports=[
        "ApplicationServices",
        "AVFoundation",
        "CoreText",
        "Quartz",
        "ServiceManagement",
        "PyObjCTools",
        "truststore",
        "_sounddevice_data",
    ],
    excludes=[
        "PyQt5",
        "openai",
        "pydantic",
        "httpx",
        "torch",
        "transformers",
    ],
    noarchive=False,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name="VeyqaVoice",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=False,
    target_arch="arm64",
    codesign_identity=None,
    entitlements_file=str(project / "packaging" / "voice_input.entitlements"),
)
coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=False,
    name="VeyqaVoice",
)
app = BUNDLE(
    coll,
    name="Veyqa Voice.app",
    icon=str(project / "packaging" / "AppIcon.icns"),
    bundle_identifier="com.wjcdev.veyqa",
    version="0.2.6",
    info_plist={
        "CFBundleDisplayName": "Veyqa Voice",
        "CFBundleName": "Veyqa Voice",
        "CFBundleShortVersionString": "0.2.6",
        "CFBundleVersion": "210",
        "LSMinimumSystemVersion": "13.0",
        "LSUIElement": False,
        "NSHighResolutionCapable": True,
        "NSMicrophoneUsageDescription": "Veyqa Voice 需要麦克风权限，才能把你的语音实时转成文字。",
        "NSAppleEventsUsageDescription": "Veyqa Voice 需要辅助功能权限，才能在当前光标处实时写入转写文字。",
        "NSHumanReadableCopyright": "Veyqa Voice contributors",
    },
)
