"""Build a Finder-ready drag-to-install window, without automating Finder."""
import sys
import subprocess
import tempfile
from pathlib import Path

import dmgbuild


def settings(app: Path, background: Path, guide: Path) -> dict:
    return {
        "format": "UDZO",
        "files": [str(app), (str(guide), "安装与权限说明.html")],
        "symlinks": {"Applications": "/Applications"},
        "background": str(background),
        "window_rect": ((160, 120), (720, 480)),
        "icon_size": 80,
        "text_size": 14,
        "icon_locations": {
            app.name: (180, 175), "Applications": (540, 175),
            "安装与权限说明.html": (360, 345),
        },
        # Never add FinderInfo to the signed bundle: codesign --strict rejects it.
        "hide_extensions": ["安装与权限说明.html"],
        "default_view": "icon-view",
        "show_toolbar": False,
        "show_status_bar": False,
        "show_pathbar": False,
        "show_sidebar": False,
        "include_icon_view_settings": True,
        "include_list_view_settings": False,
    }


if __name__ == "__main__":
    app, background, guide, output = map(Path, sys.argv[1:])
    for path in (app, background, guide):
        if not path.exists():
            raise SystemExit(f"Missing DMG input: {path}")
    dmgbuild.build_dmg(str(output), "Veyqa Voice", settings=settings(app, background, guide))
    # Validate the delivered image, not just the pre-packaging source bundle.
    with tempfile.TemporaryDirectory(prefix="veyqa-dmg-check-") as mount:
        subprocess.run(["hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", mount,
                        str(output)], check=True, stdout=subprocess.DEVNULL)
        try:
            subprocess.run(["codesign", "--verify", "--deep", "--strict",
                            str(Path(mount) / app.name)], check=True)
        finally:
            subprocess.run(["hdiutil", "detach", mount], check=True,
                           stdout=subprocess.DEVNULL)
