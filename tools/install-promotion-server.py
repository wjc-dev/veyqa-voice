"""Run on the authorized server from an SSH-uploaded private staging folder.

Starts fail-closed: no enabled/budget marker is created by deployment.
"""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

os.umask(0o077)
staging = Path(sys.argv[1]).resolve()
assert staging.parent == Path("/tmp") and staging.name.startswith("veyqa-deploy.")
services = ["kms-engine.service", "kms-web.service", "kms-ha-agent@HK.service", "nginx.service"]
def pids():
    return {s: subprocess.check_output(["systemctl", "show", "-p", "MainPID", "--value", s], text=True).strip() for s in services}
before = pids()
subprocess.run(["/usr/bin/python3", "-m", "py_compile", str(staging / "promotion.py")], check=True)
if subprocess.run(["id", "veyqa-voice"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
    subprocess.run(["useradd", "--system", "--no-create-home", "--shell", "/usr/sbin/nologin", "veyqa-voice"], check=True)
for path, mode in (("/opt/veyqa-voice", "755"), ("/etc/veyqa-voice", "750")):
    subprocess.run(["install", "-d", "-o", "root", "-g", "veyqa-voice", "-m", mode, path], check=True)
for source, destination, mode in (
    ("promotion.py", "/opt/veyqa-voice/promotion.py", "644"),
    ("bailian-key", "/etc/veyqa-voice/bailian-key", "640"),
    ("veyqa-credentials.service", "/etc/systemd/system/veyqa-credentials.service", "644"),
    ("nginx-location.conf", "/etc/nginx/veyqa-voice-location.conf", "644"),
):
    subprocess.run(["install", "-o", "root", "-g", "veyqa-voice", "-m", mode,
                    str(staging/source), destination], check=True)

nginx = Path("/etc/nginx/sites-enabled/rtcut.conf").resolve()
text = nginx.read_text()
include = "include /etc/nginx/veyqa-voice-location.conf;"
backup = None
if include not in text:
    positions = []
    for start in re.finditer(r"\bserver\s*\{", text):
        depth, end = 1, start.end()
        while end < len(text) and depth:
            depth += (text[end] == "{") - (text[end] == "}")
            end += 1
        block = text[start.end():end]
        name = re.search(r"server_name\s+rtcut\.org\s+www\.rtcut\.org\s*;", block)
        if name and re.search(r"listen\s+443\s+ssl", block):
            positions.append(start.end() + name.end())
    if len(positions) != 1:
        raise RuntimeError("Could not isolate the existing rtcut.org HTTPS server")
    position = positions[0]
    backup = nginx.with_name(nginx.name + ".veyqa-backup-" + str(int(time.time())))
    shutil.copy2(nginx, backup)
    nginx.write_text(text[:position] + "\n    " + include + text[position:])
    try:
        subprocess.run(["nginx", "-t"], check=True)
    except Exception:
        shutil.copy2(backup, nginx)
        raise
subprocess.run(["systemctl", "daemon-reload"], check=True)
subprocess.run(["systemctl", "enable", "--now", "veyqa-credentials.service"], check=True)
subprocess.run(["nginx", "-t"], check=True)
subprocess.run(["systemctl", "reload", "nginx"], check=True)
after = pids()
print(json.dumps({"existing_pids_unchanged": before == after,
    "nginx_backup": str(backup) if backup else None,
    "credential_broker_enabled_for_public_use": False}))
