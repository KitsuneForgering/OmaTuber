"""Classify the clips recorded by `sound-probe record` with YAMNet (ONNX) and
print the same leave-one-out style confusion table `sound-probe train` prints,
so both classifiers are judged on the same recordings.

usage: compare.py <data-dir> <yamnet.onnx> <yamnet_class_map.csv> [reference_logmel.npy test.wav]

<data-dir> is the folder holding sounds/<class>/*.wav (default
~/.local/share/omatuber). Each recorded class is mapped to a group of YAMNet
classes below; a clip goes to the group with the highest score, or to
"outros" when no group reaches MIN_SCORE. Recorded classes with no group of
their own (ambient noise, typing) count as "outros". YAMNet is not trained on
this person's voice: that is the point of the comparison.

With the two optional arguments, the NumPy log-mel is first checked against
the reference written by export.py. Needs numpy and onnxruntime.
"""
import csv
import sys
import wave
from pathlib import Path

import numpy as np
import onnxruntime as ort

GROUPS = {
    "fala": ["Speech", "Conversation", "Narration, monologue"],
    "risada": ["Laughter", "Giggle", "Snicker", "Belly laugh", "Chuckle, chortle"],
    "susto": ["Gasp", "Screaming"],
    "grito": ["Shout", "Bellow", "Yell"],
    "suspiro": ["Sigh", "Groan"],
}
MIN_SCORE = 0.2

SR, FRAME, HOP, FFT, MELS = 16000, 400, 160, 512, 64
PATCH, PATCH_HOP = 96, 48


def hz_to_mel(f):
    return 1127.0 * np.log1p(f / 700.0)


def mel_matrix():
    """tf.signal.linear_to_mel_weight_matrix(64, 257, 16000, 125, 7500)."""
    bins = FFT // 2 + 1
    freqs = np.linspace(0, SR / 2, bins)[1:]
    spec_mel = hz_to_mel(freqs)[:, None]
    edges = np.linspace(hz_to_mel(125.0), hz_to_mel(7500.0), MELS + 2)
    lower, center, upper = edges[:-2], edges[1:-1], edges[2:]
    w = np.maximum(0, np.minimum((spec_mel - lower) / (center - lower), (upper - spec_mel) / (upper - center)))
    return np.vstack([np.zeros((1, MELS)), w]).astype(np.float32)


MEL = mel_matrix()
HANN = (0.5 - 0.5 * np.cos(2 * np.pi * np.arange(FRAME) / FRAME)).astype(np.float32)


def log_mel(x):
    n = 1 + (len(x) - FRAME) // HOP
    frames = np.stack([x[i * HOP : i * HOP + FRAME] for i in range(n)]) * HANN
    mag = np.abs(np.fft.rfft(frames, FFT)).astype(np.float32)
    return np.log(mag @ MEL + 0.001)


def pad(x):
    """features.pad_waveform: room for at least one patch, then whole hops."""
    min_n = int((0.96 + 0.025 - 0.010) * SR)
    hop = int(0.48 * SR)
    n = max(len(x), min_n)
    n = min_n + int(np.ceil((n - min_n) / hop)) * hop
    return np.pad(x, (0, n - len(x)))


def read_wav(path):
    with wave.open(str(path)) as w:
        if w.getframerate() != SR or w.getnchannels() != 1 or w.getsampwidth() != 2:
            sys.exit(f"{path}: need 16 kHz mono 16-bit")
        return np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float32) / 32768.0


def scores(sess, x):
    lm = log_mel(pad(x))
    patches = np.stack([lm[i : i + PATCH] for i in range(0, len(lm) - PATCH + 1, PATCH_HOP)])
    return sess.run(None, {"patches": patches})[0]


def main():
    if len(sys.argv) not in (4, 6):
        sys.exit(__doc__)
    data, onnx_path, class_map = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
    with open(class_map) as f:
        names = [row["display_name"] for row in csv.DictReader(f)]
    index = {n: i for i, n in enumerate(names)}
    groups = {g: [index[n] for n in members] for g, members in GROUPS.items()}

    if len(sys.argv) == 6:
        ref = np.load(sys.argv[4])
        mine = log_mel(pad(read_wav(sys.argv[5])))
        n = min(len(ref), len(mine))
        print(f"log-mel check: max abs diff vs TensorFlow {np.abs(ref[:n] - mine[:n]).max():.2e} over {n} frames")

    sess = ort.InferenceSession(onnx_path)
    recorded = sorted(p.name for p in (data / "sounds").iterdir() if p.is_dir())
    labels = recorded + (["outros"] if "outros" not in recorded else [])
    truth_of = {c: (c if c in GROUPS or c == "outros" else "outros") for c in recorded}
    confusion = {c: {l: 0 for l in labels} for c in recorded}

    for c in recorded:
        for wav in sorted((data / "sounds" / c).glob("*.wav")):
            s = scores(sess, read_wav(wav)).mean(axis=0)
            present = {g: float(s[idx].max()) for g, idx in groups.items() if g in labels}
            best = max(present, key=present.get, default="outros")
            guess = best if present.get(best, 0) >= MIN_SCORE else "outros"
            confusion[c][guess] += 1
            top = ", ".join(f"{names[i]} {s[i]:.2f}" for i in np.argsort(-s)[:3])
            print(f"{c:>10}/{wav.name}: {guess:<8} | {top}")

    print("\nYAMNet (rows: recorded as, columns: classified as)")
    print(" " * 10 + "".join(f" {l[:8]:>8}" for l in labels))
    right = total = 0
    for c in recorded:
        print(f"{c[:10]:>10}" + "".join(f" {confusion[c][l]:>8}" for l in labels))
        total += sum(confusion[c].values())
        right += confusion[c][truth_of[c]]
    print(f"accuracy: {right}/{total} clips ({100 * right / max(total, 1):.0f}%)")


if __name__ == "__main__":
    main()
