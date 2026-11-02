#!/usr/bin/env python3
"""Phone-side Canopus queries through the AstroBox bridge: query.py KEY METHOD [JSON]"""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import mirage_session as ms

key, method = sys.argv[1], sys.argv[2]
arg = sys.argv[3] if len(sys.argv) > 3 else "{}"
s = ms.Session(key, Path(ms.ROOT / "build/mirage/shots"))
out = s.phone(f"{method}={arg}")
for line in out.splitlines():
    if method in line or '"result"' in line and '"id":3' in line:
        print(line[:3000])
