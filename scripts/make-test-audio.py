#!/usr/bin/env python3
"""Make a WAV file for listening tests of the PC → Mac audio stream.

- A quiet continuous 440 Hz tone: a dropout or click is easy to hear as a gap or pop.
- A short 1 kHz beep at the start of every second: a missing or uneven beep shows a
  problem, and the beeps can be used to measure latency.
- A voice reads the elapsed time every 10 seconds, so a problem can be located later.

Needs macOS (`say`) and only the Python standard library. Output: 48 kHz, 16-bit, mono.

    python3 scripts/make-test-audio.py --minutes 10 --out crossglide-test.wav
"""

import argparse
import array
import math
import os
import subprocess
import tempfile
import wave

RATE = 48_000
TONE_HZ = 440  # A whole number of cycles per second, so one-second blocks join seamlessly.
TONE_LEVEL = 0.08
BEEP_HZ = 1_000
BEEP_LEVEL = 0.35
BEEP_MS = 30
VOICE_LEVEL = 0.8
VOICE_DELAY_S = 0.3  # Start speaking after the beep so the beep stays crisp.


def one_second() -> array.array:
    """The tone for one second with the beep at its start."""
    beep_len = RATE * BEEP_MS // 1000
    ramp = RATE // 1000  # 1 ms fade in and out so the beep doesn't click.
    block = array.array("h", bytes(2 * RATE))
    for i in range(RATE):
        t = i / RATE
        v = TONE_LEVEL * math.sin(2 * math.pi * TONE_HZ * t)
        if i < beep_len:
            envelope = min(1.0, i / ramp, (beep_len - i) / ramp)
            v += BEEP_LEVEL * envelope * math.sin(2 * math.pi * BEEP_HZ * t)
        block[i] = int(v * 32767)
    return block


def label(seconds: int) -> str:
    minutes, secs = divmod(seconds, 60)
    if seconds == 0:
        return "开始"
    if minutes == 0:
        return f"{secs}秒"
    if secs == 0:
        return f"{minutes}分"
    return f"{minutes}分{secs}秒"


def speak(text: str, voice: str, workdir: str) -> array.array:
    path = os.path.join(workdir, "voice.wav")
    subprocess.run(
        ["say", "-v", voice, "-o", path, "--file-format=WAVE", f"--data-format=LEI16@{RATE}", text],
        check=True,
    )
    with wave.open(path) as w:
        assert (w.getframerate(), w.getsampwidth(), w.getnchannels()) == (RATE, 2, 1)
        return array.array("h", w.readframes(w.getnframes()))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--minutes", type=int, default=10)
    parser.add_argument("--out", default="crossglide-test.wav")
    parser.add_argument("--voice", default="Tingting", help="a `say -v '?'` voice")
    args = parser.parse_args()

    total = args.minutes * 60
    audio = one_second() * total
    with tempfile.TemporaryDirectory() as workdir:
        for at in range(0, total, 10):
            start = int((at + VOICE_DELAY_S) * RATE)
            for i, s in enumerate(speak(label(at), args.voice, workdir)):
                j = start + i
                if j >= len(audio):
                    break
                audio[j] = max(-32768, min(32767, audio[j] + int(s * VOICE_LEVEL)))

    with wave.open(args.out, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(audio.tobytes())
    print(f"wrote {args.out}: {args.minutes} min, {RATE} Hz, 16-bit mono")


if __name__ == "__main__":
    main()
