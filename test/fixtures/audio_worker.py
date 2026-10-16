"""Deterministic private-pipe fixture: no microphone, GUI or cloud access."""
import base64
import json
import struct
import sys
import time

mode = sys.argv[1]
json.loads(sys.stdin.readline())
if mode == "start-stall":
    time.sleep(60)
else:
    for event in [
        {"type": "format", "rate": 16000, "device": "fixture"},
        {"type": "started"},
        {"type": "audio", "data": base64.b64encode(struct.pack("<1600f", *([0.2] * 1600))).decode()},
    ]:
        print(json.dumps(event), flush=True)
    sys.stdin.readline()
    if mode == "stop-stall":
        time.sleep(60)
    print(json.dumps({"type": "stopped"}), flush=True)
