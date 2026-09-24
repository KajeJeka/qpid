#!/usr/bin/env python3
"""Generate test audio for Q-pid development (architecture.md section 15,
Phase 0 step 4).

Produces a 10 minute stereo WAV: a slow frequency sweep plus periodic pulses
standing in for speech rhythm, so seeks and speed changes have something
audibly distinct to land on. If ffmpeg is on PATH, also emits mp3/m4a/flac/
ogg copies for format-coverage testing (acceptance test 1/8, section 16).

Usage:
    python tools/gen_test_audio.py [--outdir test_audio] [--minutes 10]
"""
import argparse
import math
import shutil
import struct
import subprocess
import wave
from pathlib import Path

SAMPLE_RATE = 44100
CHANNELS = 2


def generate_wav(path: Path, minutes: float) -> None:
    total_frames = int(SAMPLE_RATE * minutes * 60)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(CHANNELS)
        w.setsampwidth(2)  # 16-bit PCM
        w.setframerate(SAMPLE_RATE)

        chunk_frames = SAMPLE_RATE  # write one second at a time
        buf = bytearray()
        for i in range(total_frames):
            t = i / SAMPLE_RATE

            # Slow sweep from 220 Hz to 880 Hz over the whole file, useful
            # for hearing pitch shift (or absence of it) at each speed.
            sweep_hz = 220.0 + (660.0 * (i / total_frames))
            sweep = math.sin(2 * math.pi * sweep_hz * t) * 0.15

            # A short pulse twice a second, standing in for speech cadence,
            # useful for judging seek accuracy by ear.
            pulse_phase = (t * 2.0) % 1.0
            pulse = 0.3 * math.exp(-pulse_phase * 40.0) if pulse_phase < 0.25 else 0.0

            sample = max(-1.0, min(1.0, sweep + pulse))
            value = int(sample * 32767)
            buf += struct.pack("<hh", value, value)

            if len(buf) >= chunk_frames * 4:
                w.writeframesraw(bytes(buf))
                buf.clear()

        if buf:
            w.writeframesraw(bytes(buf))

    print(f"wrote {path} ({minutes} min, {SAMPLE_RATE} Hz, stereo)")


def make_format_copies(wav_path: Path, outdir: Path) -> None:
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        print("ffmpeg not found on PATH; skipping mp3/m4a/flac/ogg copies")
        print("(WAV alone is enough to exercise the engine; add ffmpeg later")
        print(" for full format-coverage testing per acceptance test 1/8)")
        return

    targets = {
        "test.mp3": ["-codec:a", "libmp3lame", "-b:a", "128k"],
        "test.m4a": ["-codec:a", "aac", "-b:a", "128k"],
        "test.flac": ["-codec:a", "flac"],
        "test.ogg": ["-codec:a", "libvorbis", "-q:a", "4"],
    }

    for filename, codec_args in targets.items():
        out_path = outdir / filename
        cmd = [ffmpeg, "-y", "-i", str(wav_path), *codec_args, str(out_path)]
        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode == 0:
            print(f"wrote {out_path}")
        else:
            print(f"failed to write {out_path}: {result.stderr.strip()[-300:]}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--outdir", default="test_audio", help="output directory")
    parser.add_argument("--minutes", type=float, default=10.0, help="duration in minutes")
    args = parser.parse_args()

    outdir = Path(args.outdir)
    outdir.mkdir(parents=True, exist_ok=True)

    wav_path = outdir / "test.wav"
    generate_wav(wav_path, args.minutes)
    make_format_copies(wav_path, outdir)


if __name__ == "__main__":
    main()
