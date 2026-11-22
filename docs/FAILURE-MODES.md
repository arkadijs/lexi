# Failure-mode tests (SPEC N5), run 2026-11-22 on the Moto g42, release build 0.1.0

How: the phone over adb, the app's status line read with `uiautomator dump`, and after every scenario a check for crashes
(`adb logcat -b crash`), leftover service (`dumpsys activity services app.lexi`) and wake lock (`dumpsys power`).
Both providers unless noted. "Clean" = no crash, no foreground service and no wake lock left behind.

| Scenario | How it was simulated | Result |
|---|---|---|
| No network before the call | `cmd connectivity airplane-mode enable` | "No network", clean; works once the network is back (it takes the phone ~10 s to reconnect) |
| Wi-Fi drops 5 s mid-call | `svc wifi disable/enable` | OpenAI: call never drops. Google: **was** ended with "No network" (3 quick retries used up 4 s); **fixed**: "Reconnecting…", back ~5 s after Wi-Fi |
| Outage 15 s / 30 s mid-call | same | OpenAI recovers (15 s: unnoticed; 30 s: "Reconnecting…" then live). Google "Reconnecting…" then live |
| Outage 35 s (longer than the 20 s reconnect window) | same | Google: "Reconnecting…" for ~20 s, then "No network", clean; a new call works |
| Airplane mode mid-call | `airplane-mode enable` for 20 s | clean end ("No network" before the fix) |
| App blocked from the network (OS still online) | `cmd connectivity set-chain3-enabled true` + `set-package-networking-enabled false app.lexi` | "No network", clean |
| Server down (503 from the ingress) | Service selector pointed at nothing for ~1 minute, then restored | "Service unavailable. Try again" for both providers, clean; recovered at once |
| Microphone permission revoked | `pm revoke ... RECORD_AUDIO` | system prompt; after a denial "Microphone permission needed"; after a second "...Enable it in Settings" and the next tap opens the app's settings page; clean |
| Rapid call/hang-up taps | 12 cycles at 0.4 s; 7 taps at 120 ms; hang-up then call after 0.2 / 0.8 / 2 s | no crash, always ends in a consistent state, clean |
| Rapid mute toggling in a call | 10 taps at 150 ms | call stays live |
| Force-stop mid-call | `am force-stop` | service and wake lock gone at once; relaunch and new call fine |
| Process crash mid-call | `am crash app.lexi` | notification, service and wake lock gone; relaunch and new call fine |
| Wrong phone clock (+10 min) | `cmd alarm set-time` | **was** "Not authorised. Update the app" (misleading); **fixed**: HTTP 412 (only for a valid signature) and "Check the date and time on your phone" |
| Panics at the JNI boundary | static check | all 12 JNI exports start with `jni_guard!` (`catch_unwind`), and `ffi` has no `unwrap`/`expect`. Note: release builds use `panic = "abort"`, so a panic there ends the app instead of unwinding; `catch_unwind` only helps in debug builds. Accepted (smaller library) |

Verified by the user afterwards (2026-11-22): calls with a wired headset and the transitions between output devices (earpiece, speaker, headset) all work as expected.

Not tested (no hardware, or not possible over adb): Bluetooth headsets connecting or disconnecting mid-call (not mentioned by the user),
audio focus taken by a phone call (no SIM), and another app taking audio focus (an intent only opened a chooser).
Covered by host tests: reconnect logic and its window, auth and rate-limit errors never retried, max-session guard,
attestation skew, per-install and per-IP rate limits.

Bugs found and fixed by these tests: (1) reconnecting gave up after ~4 s, ending Gemini calls on a short Wi-Fi drop (the
supervisor now keeps retrying for 20 s on transient errors); (2) a wrong phone clock was reported as an authorisation
failure.
