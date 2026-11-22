#!/usr/bin/env bash
# Tap the on-screen element whose visible text equals the given text (popup menu items have text, not a
# content-description): scripts/adb-tap-text.sh "Google"
set -euo pipefail
F="${TMPDIR:-/tmp}/lexi-ui.xml"
adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1
adb pull /sdcard/ui.xml "$F" >/dev/null 2>&1
UIXML="$F" python3 - "$1" <<'P'
import os,sys,re,subprocess
x=open(os.environ["UIXML"]).read()
for m in re.finditer(r'text="([^"]*)"[^>]*bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"',x):
    if m.group(1)==sys.argv[1]:
        a,b,c,d=map(int,m.groups()[1:]); subprocess.run(["adb","shell","input","tap",str((a+c)//2),str((b+d)//2)]); print("tapped text",m.group(1)); break
else:
    print("not found",sys.argv[1]); sys.exit(1)
P
