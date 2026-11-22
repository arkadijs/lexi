#!/usr/bin/env bash
# Tap the on-screen element whose content-description contains the given text (case-insensitive), on the connected
# phone: scripts/adb-tap.sh "Call Lexi". Blind coordinates are fragile; this reads the real UI (uiautomator).
set -euo pipefail
F="${TMPDIR:-/tmp}/lexi-ui.xml"
adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1
adb pull /sdcard/ui.xml "$F" >/dev/null 2>&1
UIXML="$F" python3 - "$1" <<'P'
import os,sys,re,subprocess
x=open(os.environ["UIXML"]).read()
for m in re.finditer(r'content-desc="([^"]*)"[^>]*bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"',x):
    if sys.argv[1].lower() in m.group(1).lower():
        a,b,c,d=map(int,m.groups()[1:]); subprocess.run(["adb","shell","input","tap",str((a+c)//2),str((b+d)//2)]); print("tapped",m.group(1)); break
else:
    print("not found",sys.argv[1]); print(re.findall(r'content-desc="([^"]+)"',x)); sys.exit(1)
P
