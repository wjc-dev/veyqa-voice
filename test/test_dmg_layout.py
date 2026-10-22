import importlib.util
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_dmg_is_drag_to_install_with_local_help():
    spec = importlib.util.spec_from_file_location("veyqa_dmg", ROOT / "packaging/build_dmg.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    settings = module.settings(Path("Veyqa Voice.app"), Path("guide.png"), Path("help.html"))
    assert settings["symlinks"] == {"Applications": "/Applications"}
    assert settings["default_view"] == "icon-view"
    assert settings["window_rect"][1] == (720, 480)
    locations = settings["icon_locations"]
    assert locations["Veyqa Voice.app"][0] < locations["Applications"][0]
    assert locations["Veyqa Voice.app"][1] == locations["Applications"][1]
    assert ("help.html", "安装与权限说明.html") in settings["files"]
    assert settings["show_sidebar"] is False
    assert "Veyqa Voice.app" not in settings["hide_extensions"]
    guide = (ROOT / "packaging/install-guide.html").read_text()
    for instruction in ("Applications", "隐私与安全性", "仍要打开", "推广语音", "无需填写 Key", "菜单栏"):
        assert instruction in guide
