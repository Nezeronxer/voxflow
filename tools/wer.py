"""WER calculator: python tools/wer.py <eval_dir> <hyp_tag>

Compares <name>.<hyp_tag>.txt against <name>.ref.txt for every ref in eval_dir.
Normalization: lowercase, yo->ye, punctuation stripped. Prints per-file and
per-language aggregate WER (sum of edit distances / sum of ref lengths).
"""
import re
import sys
from pathlib import Path


def norm(s: str) -> list[str]:
    s = s.lower().replace("ё", "е")  # ё → е
    s = re.sub(r"[^\w\s]", " ", s, flags=re.UNICODE)
    return s.split()


def lev(a: list[str], b: list[str]) -> int:
    dp = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        prev, dp[0] = dp[0], i
        for j, y in enumerate(b, 1):
            cur = dp[j]
            dp[j] = min(dp[j] + 1, dp[j - 1] + 1, prev + (x != y))
            prev = cur
    return dp[-1]


def main() -> None:
    eval_dir, tag = Path(sys.argv[1]), sys.argv[2]
    agg: dict[str, list[int]] = {}
    for ref_path in sorted(eval_dir.glob("*.ref.txt")):
        name = ref_path.name[: -len(".ref.txt")]
        hyp_path = eval_dir / f"{name}.{tag}.txt"
        if not hyp_path.exists():
            print(f"{name}: NO HYP ({hyp_path.name})")
            continue
        ref = norm(ref_path.read_text(encoding="utf-8"))
        hyp = norm(hyp_path.read_text(encoding="utf-8"))
        d = lev(ref, hyp)
        lang = name.split("_")[0]
        agg.setdefault(lang, [0, 0])
        agg[lang][0] += d
        agg[lang][1] += len(ref)
        print(f"{name}: wer={d / max(len(ref), 1):.3f} ({d}/{len(ref)})")
    for lang, (d, n) in sorted(agg.items()):
        print(f"TOTAL {lang} [{tag}]: WER={d / max(n, 1):.3f} ({d}/{n})")


if __name__ == "__main__":
    main()
