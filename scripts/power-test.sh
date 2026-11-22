#!/usr/bin/env bash
# Battery power of Lexi on a phone over WIRELESS adb (a USB cable would charge it and spoil the readings):
#   adb tcpip 5555 (once, over USB) ; adb connect <phone-ip>:5555 ; unplug the cable ; scripts/power-test.sh
# Per scenario: 20 s settle, then the battery's current and voltage are sampled once a second ON the phone
# (/sys/class/power_supply/battery) and averaged: mW = |current| x voltage. The screen is set to a fixed brightness.
# The phone must stay untouched and unplugged for ~25 minutes. Settings are restored at the end.
set -uo pipefail
cd "$(dirname "$0")/.."
S=target/scratch; mkdir -p "$S"
SAMPLE_S="${SAMPLE_S:-120}"; SAMPLE_OFF_S="${SAMPLE_OFF_S:-180}"; SETTLE_S="${SETTLE_S:-20}"
OUT="${1:-$S/power-results.txt}"
# use the wireless connection (USB may still be attached while preparing)
export ANDROID_SERIAL="${ANDROID_SERIAL:-$(adb devices | awk '/:[0-9]+\tdevice/{print $1; exit}')}"
[ -n "$ANDROID_SERIAL" ] || { echo "no wireless adb device: adb tcpip 5555 over USB, then adb connect <ip>:5555" >&2; exit 1; }
need_tools() { true; }
powered() { adb shell dumpsys battery | grep -E "^\s*(AC|USB|Wireless) powered: true" | head -1; }
tap() { scripts/adb-tap.sh "$1" >/dev/null; }
status_line() { adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1; adb pull /sdcard/ui.xml "$S/ui.xml" >/dev/null 2>&1
  grep -o 'text="\(Talking with Lexi\|Reconnecting…\|No network\|Service unavailable[^"]*\)"' "$S/ui.xml" | head -1; }
muted() { adb shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1; adb pull /sdcard/ui.xml "$S/ui.xml" >/dev/null 2>&1; grep -q 'content-desc="Unmute' "$S/ui.xml" || tap "Mute"; }
set_provider() { tap "Provider"; sleep 1; scripts/adb-tap-text.sh "$1" >/dev/null; sleep 1; }
launch() { adb shell am force-stop app.lexi; adb shell am start -n app.lexi/.MainActivity "${@}" >/dev/null; sleep 3; }
screen_on() { adb shell input keyevent KEYCODE_WAKEUP; adb shell wm dismiss-keyguard >/dev/null 2>&1; sleep 1; }
# Screen off via the screen timeout, NOT the power-button sleep key: the latter locks the phone at once (PIN needed
# for the next scenario), a timeout only locks after the user's "automatically lock" delay (30 min here).
screen_off() { adb shell settings put system screen_off_timeout 8000; sleep 12; }
screen_timeout_long() { adb shell settings put system screen_off_timeout 1800000; }
# sample N seconds on the phone; prints "mean_mW samples mean_mA drop_uAh"
sample() { local n="$1" label="$2"
  local c0; c0=$(adb shell cat /sys/class/power_supply/battery/charge_counter | tr -d '\r')
  adb shell "rm -f /data/local/tmp/p.log; nohup sh -c 'i=0; while [ \$i -lt $n ]; do echo \$(cat /sys/class/power_supply/battery/current_now) \$(cat /sys/class/power_supply/battery/voltage_now); i=\$((i+1)); sleep 1; done' > /data/local/tmp/p.log 2>&1 &" >/dev/null
  sleep $((n + 3))
  # the screen may be off: wake it only to pull the file over adb when needed
  local tries=0; until adb pull /data/local/tmp/p.log "$S/p.log" >/dev/null 2>&1 || [ $tries -ge 10 ]; do sleep 2; tries=$((tries+1)); done
  local c1; c1=$(adb shell cat /sys/class/power_supply/battery/charge_counter | tr -d '\r')
  python3 - "$S/p.log" "$c0" "$c1" "$label" <<'P' | tee -a "$OUT"
import sys
rows=[l.split() for l in open(sys.argv[1]) if len(l.split())==2]
if not rows: print(f"{sys.argv[4]:<44} no samples"); sys.exit()
I=[abs(int(a)) for a,b in rows]; V=[int(b) for a,b in rows]
mw=sum(i*v for i,v in zip(I,V))/len(I)/1e9; ma=sum(I)/len(I)/1000
drop=(int(sys.argv[2])-int(sys.argv[3]))/1000
print(f"{sys.argv[4]:<44} {mw:7.0f} mW  {ma:6.0f} mA  ({len(rows)} samples, charge counter -{drop:.1f} mAh)")
P
}
scenario() { # label, provider|-, visuals(fluid|fractal|off), call(yes|no), screen(on|off), seconds
  local label="$1" prov="$2" vis="$3" call="$4" scr="$5" secs="$6"
  echo ">> $label" >&2
  screen_on
  if [ "$vis" = off ]; then launch --es visuals off; else launch; fi
  if [ "$vis" = fractal ]; then tap "Tap for fractal"; sleep 1; fi   # the button toggles to the other scene
  [ "$prov" != - ] && set_provider "$prov"
  if [ "$call" = yes ]; then muted; sleep 1; tap "call"; sleep 8; local s; s=$(status_line); [ -n "$s" ] || echo "   WARNING: call not live ($label)" >&2; fi
  [ "$scr" = off ] && screen_off
  sleep "$SETTLE_S"
  sample "$secs" "$label"
  screen_timeout_long
  screen_on
  if [ "$call" = yes ]; then tap "Hang up"; sleep 2; fi
}
need_tools
keyguard_showing() { adb shell dumpsys window policy | grep -q "mIsShowing=true"; }
if keyguard_showing; then
  echo "The phone is locked: unlock it once and leave it unlocked (waiting up to 10 minutes)..." >&2
  for _ in $(seq 1 120); do keyguard_showing || break; adb shell input keyevent KEYCODE_WAKEUP; sleep 5; done
  keyguard_showing && { echo "still locked, giving up" >&2; exit 1; }
fi
[ -z "$(powered)" ] || { echo "the phone is powered/charging: unplug it (and use wireless adb)" >&2; exit 1; }
LVL=$(adb shell dumpsys battery | grep -m1 "level:" | tr -dc 0-9)
{ echo "# $(date '+%F %T')  battery ${LVL}%  model $(adb shell getprop ro.product.model | tr -d '\r')  brightness fixed 100/255  Wi-Fi on  (mean power over ${SAMPLE_S} s / ${SAMPLE_OFF_S} s screen-off)"; } | tee "$OUT"
OLD_BR=$(adb shell settings get system screen_brightness | tr -d '\r'); OLD_MODE=$(adb shell settings get system screen_brightness_mode | tr -d '\r'); OLD_TO=$(adb shell settings get system screen_off_timeout | tr -d '\r')
restore() { adb shell settings put system screen_brightness "$OLD_BR"; adb shell settings put system screen_brightness_mode "$OLD_MODE"; adb shell settings put system screen_off_timeout "$OLD_TO"; adb shell am force-stop app.lexi; }
trap restore EXIT
adb shell settings put system screen_brightness_mode 0; adb shell settings put system screen_brightness 100; adb shell settings put system screen_off_timeout 1800000
scenario "screen on, app open, visuals off, no call"  -      off  no  on  "$SAMPLE_S"
scenario "screen on, fluid visuals, no call"          -      fluid no on  "$SAMPLE_S"
scenario "screen on, fractal visuals, no call"        -      fractal no on "$SAMPLE_S"
scenario "OpenAI call (muted), visuals off"           OpenAI off  yes on  "$SAMPLE_S"
scenario "OpenAI call (muted), fluid"                 OpenAI fluid yes on "$SAMPLE_S"
scenario "Google call (muted), fluid"                 Google fluid yes on "$SAMPLE_S"
scenario "OpenAI call (muted), screen OFF"            OpenAI fluid yes off "$SAMPLE_OFF_S"
scenario "Google call (muted), screen OFF"            Google fluid yes off "$SAMPLE_OFF_S"
scenario "screen off, app open, no call (idle)"          -      off  no  off "$SAMPLE_OFF_S"
echo "done; results in $OUT"
