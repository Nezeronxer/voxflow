"""Make noisy eval variants: python tools/add_noise.py <eval_dir> <snr_db>

For every <lang>_<NN>.wav writes <lang>_<NN>.noisy<snr>.wav with additive white
gaussian noise at the given SNR, plus a matching .ref.txt copy, so wer.py can
treat the noisy set as independent files (names: ru_01noisy5 etc. via suffix
before extension is awkward — we use a subdirectory instead).
Output goes to <eval_dir>/noisy<snr>/ with the SAME file names.
"""
import sys
import wave
from pathlib import Path

import numpy as np

def main() -> None:
    eval_dir, snr_db = Path(sys.argv[1]), float(sys.argv[2])
    out_dir = eval_dir / f"noisy{int(snr_db)}"
    out_dir.mkdir(exist_ok=True)
    rng = np.random.default_rng(42)  # фиксированный seed — воспроизводимость до/после
    for wav_path in sorted(eval_dir.glob("*.wav")):
        with wave.open(str(wav_path), "rb") as r:
            assert r.getsampwidth() == 2 and r.getnchannels() == 1
            rate = r.getframerate()
            x = np.frombuffer(r.readframes(r.getnframes()), dtype=np.int16).astype(np.float64)
        sig_pow = np.mean(x**2)
        noise = rng.standard_normal(len(x))
        noise *= np.sqrt(sig_pow / (10 ** (snr_db / 10)) / np.mean(noise**2))
        y = np.clip(x + noise, -32768, 32767).astype(np.int16)
        out = out_dir / wav_path.name
        with wave.open(str(out), "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(rate)
            w.writeframes(y.tobytes())
        ref = wav_path.with_name(wav_path.stem + ".ref.txt")
        if ref.exists():
            (out_dir / ref.name).write_bytes(ref.read_bytes())
        print("noisy:", out.name)

if __name__ == "__main__":
    main()
