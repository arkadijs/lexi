# Battery power of Lexi (Moto g42, release build 0.1.0, 2026-11-22)

## Method
`scripts/power-test.sh`: the phone is unplugged and driven over wireless adb. For each scenario the app is set up, settles
for 20 s, then the battery's `current_now` and `voltage_now` are read once a second on the phone for 120 s (180 s with the
screen off) and averaged (mW = |current| x voltage). Brightness fixed at 100/255, Wi-Fi on, calls muted (so no speech
is generated or played). Battery energy: 4968 mAh x 3.87 V nominal = 19.2 Wh.

## Results (mean power)

| Scenario | mW | Hours per full charge | Cost over the row above |
|---|---|---|---|
| Screen on, app open, visuals off, no call | 705 | 27 | (display + app + the adb Wi-Fi link) |
| Screen on, fluid visuals, no call | 1761 | 11 | **+1056 mW for the fluid** |
| Screen on, fractal visuals, no call | 1482 | 13 | +777 mW for the fractal |
| OpenAI call, visuals off | 1137 | 17 | **+432 mW for a call** |
| OpenAI call, fluid | 2212 | 8.7 | +451 mW for a call |
| Google call, fluid | 2114 | 9.1 | +353 mW for a call |
| OpenAI call + fractal (estimate, not measured) | ~1930 | ~10 | |
| Screen off, app open, no call (idle) | 736 | | see caveat |
| Screen off, OpenAI call | 718 | | see caveat |
| Screen off, Google call | 510 | | see caveat |

What the screen-on numbers say, which are solid (differences of 0.35-1.06 W against noise of about 0.1 W):
- **The visuals are the biggest cost**: the fluid scene costs about as much as the whole rest of the app plus a call
  (GPU ~75 % busy, see `android/README.md`); the fractal is about 25 % cheaper.
- **A muted call costs 0.35-0.45 W** on top (microphone capture, audio streams, the 20 ms supervisor ticks, network
  keep-alive and, for OpenAI, a silent-frame RTP stream); Google's WebSocket is about 0.1 W cheaper than OpenAI's WebRTC.
- A typical screen-on call with the fluid scene lasts about **9 hours** from a full battery, with visuals off about **17**.

## Caveat: the screen-off numbers cannot be trusted
Idle with the screen off (736 mW) came out *higher* than a call with the screen off (718 and 510 mW), and the sample
counts differ (55 to 176 of 180 possible): wireless adb keeps the Wi-Fi radio and the CPU awake and the one-second
sampling loop throttles unpredictably in doze, so this method has a floor of about 0.5-0.7 W and cannot resolve a
screen-off call. The screen-off call is therefore only bounded from above (**at most ~0.7 W, i.e. at least ~27 h**);
the real figure is probably far lower. Resolving it needs a run without adb: start a screen-off call, disconnect adb,
let it run 30-60 min, reconnect and compare the battery percentage (the charge counter only moves in 1 % steps here, about
49 mAh) with a 30-60 min idle control.

## Options to cut power (not done)
1. Cap the visuals at 30 fps (the GPU is ~75 % busy at 60 fps): likely the biggest single saving.
2. Lower the default fluid quality level or grid size.
3. Pause or dim the visuals when the user is muted and the bot is silent for a while.
4. Look at the 20 ms supervisor tick and the silent mic stream while muted (the muted call still costs ~0.4 W).
