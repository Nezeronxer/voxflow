"""Прогон WER-оценки «до/после» по готовому eval-набору.

Копирует clean+noisy5 наборы в %TEMP%/voxflow_eval_run/, прогоняет:
  RU -> voxflow.exe --gigaam-selftest   (после)
  EN -> voxflow.exe --parakeet-selftest (после)
  RU+EN -> whisper-cli.exe (CUDA, q5)   (до; те же флаги, что asr::transcribe_cli)
и пишет гипотезы <name>.<tag>.txt рядом с .ref.txt — формат tools/wer.py.

ВАЖНО: НЕ использует --selftest/--stream-selftest — они зовут db::open(), а
voxflow.db сейчас malformed, открытие квантировало бы базу юзера в .corrupt-*.
"""
import json
import shutil
import subprocess
import sys
from pathlib import Path
import os

EXE = Path(os.environ["TEMP"]) / "voxflow_eval.exe"
WHISPER_DIR = Path(r"C:\Моя папка\wispr flow\voxflow\src-tauri\resources\whisper-cuda\Release")
WHISPER_CLI = WHISPER_DIR / "whisper-cli.exe"
Q5_MODEL = Path(r"C:\Users\Nezeronxer\AppData\Local\VoxFlow\models\ggml-large-v3-turbo-q5_0.bin")
SRC = Path(r"C:\Users\Nezeronxer\AppData\Local\VoxFlow\eval")
RUN = Path(os.environ["TEMP"]) / "voxflow_eval_run"
THREADS = "6"  # effective_threads(): половина из 12 логических ядер

FAILURES: list[str] = []


def unescape_rust_debug(s: str) -> str:
    """Минимальный разбор Rust {:?}-строки: кавычки по краям + базовые эскейпы."""
    s = s.strip()
    if s.startswith('"') and s.endswith('"'):
        s = s[1:-1]
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c == "\\" and i + 1 < len(s):
            n = s[i + 1]
            if n == "u" and i + 2 < len(s) and s[i + 2] == "{":
                j = s.index("}", i + 3)
                out.append(chr(int(s[i + 3 : j], 16)))
                i = j + 1
                continue
            out.append({"n": "\n", "t": "\t", "r": "\r", '"': '"', "'": "'", "\\": "\\"}.get(n, n))
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def run_cmd(cmd: list[str], timeout: int = 300) -> tuple[int, str, str]:
    p = subprocess.run(
        cmd, capture_output=True, timeout=timeout,
        cwd=str(WHISPER_DIR) if cmd[0] == str(WHISPER_CLI) else None,
    )
    return p.returncode, p.stdout.decode("utf-8", "replace"), p.stderr.decode("utf-8", "replace")


def selftest(flag: str, wav: Path) -> str | None:
    rc, out, err = run_cmd([str(EXE), flag, str(wav)])
    text = None
    for line in out.splitlines():
        if line.startswith("TEXT  :"):
            text = unescape_rust_debug(line.split(":", 1)[1])
    if rc != 0 or text is None:
        FAILURES.append(f"{flag} {wav.name}: rc={rc}\nSTDOUT tail: {out[-400:]}\nSTDERR tail: {err[-400:]}")
        return None
    return text


def whisper(wav: Path, lang: str) -> str | None:
    # Зеркало asr::transcribe_cli: -m <model> -l <lang> -nt -t <threads> <wav>
    rc, out, err = run_cmd([str(WHISPER_CLI), "-m", str(Q5_MODEL), "-l", lang, "-nt", "-t", THREADS, str(wav)])
    if rc != 0:
        FAILURES.append(f"whisper {wav.name}: rc={rc}\nSTDERR tail: {err[-400:]}")
        return None
    return " ".join(x.strip() for x in out.splitlines() if x.strip())


def main() -> None:
    # 1. Копия набора (исходный eval-каталог не трогаем).
    for sub, src in [("clean", SRC), ("noisy5", SRC / "noisy5")]:
        d = RUN / sub
        d.mkdir(parents=True, exist_ok=True)
        for f in src.iterdir():
            if f.is_file() and f.suffix in (".wav", ".txt"):
                shutil.copy2(f, d / f.name)
    # 2. Прогоны.
    for sub in ("clean", "noisy5"):
        d = RUN / sub
        for wav in sorted(d.glob("*.wav")):
            name = wav.stem
            lang = name.split("_")[0]
            if lang == "ru":
                txt = selftest("--gigaam-selftest", wav)
                tag = "gigaam"
            else:
                txt = selftest("--parakeet-selftest", wav)
                tag = "parakeet"
            if txt is not None:
                (d / f"{name}.{tag}.txt").write_text(txt, encoding="utf-8")
            wtxt = whisper(wav, lang)
            if wtxt is not None:
                (d / f"{name}.whisperq5.txt").write_text(wtxt, encoding="utf-8")
            print(f"[{sub}] {name}: after={'OK' if txt is not None else 'FAIL'} whisper={'OK' if wtxt is not None else 'FAIL'}", flush=True)
    # 3. Итог.
    if FAILURES:
        print("\n===== FAILURES =====")
        for f in FAILURES:
            print(f, "\n---")
    print("DONE, failures:", len(FAILURES))


if __name__ == "__main__":
    main()
