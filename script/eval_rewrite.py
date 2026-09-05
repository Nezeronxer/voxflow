#!/usr/bin/env python3
"""Прогон реальных диктовок из истории VoxFlow через системный промпт и модель.

Использование:
  EVAL_KEY=sk-... python3 script/eval_rewrite.py --base https://api.openai.com/v1 --model gpt-4.1-nano [--n 40] [--ids 1104,1097]
  (база по умолчанию — профиль VoxFlow этой машины, переопределить: VOXFLOW_DB=/path/voxflow.db)

Печатает: для каждой диктовки вход → выход, вердикт гарда (реплика rewrite_grounding из engine.rs),
латентность и токены; в конце сводку по модели.
"""
import argparse, json, os, sqlite3, sys, time, urllib.request, pathlib, re

ROOT = pathlib.Path(__file__).resolve().parents[1]
PROMPT = (ROOT / "voxflow/src-tauri/prompts/voiceflow_ru.txt").read_text(encoding="utf-8")
# База приложения: macOS — Application Support, Windows — %APPDATA%\VoxFlow.
DB = pathlib.Path(os.environ.get("VOXFLOW_DB") or (
    pathlib.Path(os.environ["APPDATA"]) / "VoxFlow/voxflow.db" if sys.platform == "win32"
    else pathlib.Path.home() / "Library/Application Support/VoxFlow/voxflow.db"))

FILLERS = {"ну", "вот", "значит", "типа", "короче", "ага", "э", "эм", "ээ", "эмм", "ммм", "как", "бы", "пожалуйста", "please", "a", "an", "the"}

def stem(tok: str) -> str:
    t = tok.lower().replace("ё", "е")
    if t.isdigit():
        return t
    keep = 6 if re.fullmatch(r"[a-z]+", t) else 4
    return t[:keep]

def structural(tok: str) -> bool:
    if tok in FILLERS:
        return True
    if len(tok) == 1 and not tok.isdigit():
        return True
    letters = [c for c in tok if c != "-"]
    if len(letters) >= 2 and letters[0] in "аэоумы" and all(c == letters[0] for c in letters):
        return True
    return False

def stems(text: str):
    out = []
    for tok in re.split(r"[^\w]+", text, flags=re.UNICODE):
        tok = tok.replace("_", "")
        if not tok:
            continue
        low = tok.lower()
        if structural(low):
            continue
        out.append(stem(low))
    return out

def grounding(inp: str, out: str, min_recall=0.9, max_exp=2):
    ic, oc = len(inp), len(out)
    if oc > ic * max_exp + 32:
        return False, 0.0, ["<длиннее>"]
    uniq = []
    for s in stems(inp):
        if s not in uniq:
            uniq.append(s)
    if not uniq:
        return True, 1.0, []
    present = set(stems(out))
    lost = [s for s in uniq if s not in present]
    comparable = oc <= ic * 2 + 32
    novel = len([s for s in present if s not in uniq]) if (comparable and len(lost) * 2 <= len(uniq)) else 0
    numeric_lost = len([s for s in lost if s.isdigit()])
    net = numeric_lost + max(0, (len(lost) - numeric_lost) - novel)
    recall = 1 - net / len(uniq)
    return recall + 1e-9 >= min_recall, recall, lost

APP_LABEL = {
    "kitty": "терминал (kitty)",
    "ghostty": "терминал (ghostty)",
    "claude": "AI prompt (Claude)",
    "telegram": "Telegram (telegram)",
    "chrome": "chrome",
    "zoom.us": "zoom.us",
}

def payload(app: str, text: str) -> str:
    return f"[ПРИЛОЖЕНИЕ]: {APP_LABEL.get(app, app)}\n[ДИКТОВКА]: {text}"

def call(base, model, key, system, user, timeout=60, extra=None):
    body = {
        "model": model,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
        "temperature": 0.2,
        "max_tokens": 1024,
    }
    if extra:
        body.update(extra)
    req = urllib.request.Request(
        base.rstrip("/") + "/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json",
                 "HTTP-Referer": "https://voxflow.local", "X-Title": "VoxFlow eval"},
    )
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=timeout) as r:
        data = json.loads(r.read())
    dt = time.time() - t0
    choice = data["choices"][0]
    content = (choice.get("message") or {}).get("content") or ""
    content = re.sub(r"<think>.*?</think>", "", content, flags=re.S).strip()
    usage = data.get("usage") or {}
    return content, dt, choice.get("finish_reason"), usage

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--n", type=int, default=40)
    ap.add_argument("--ids", default="")
    ap.add_argument("--min-words", type=int, default=4)
    ap.add_argument("--extra", default="", help="JSON с доп. полями тела запроса")
    ap.add_argument("--quiet", action="store_true")
    a = ap.parse_args()
    key = os.environ.get("EVAL_KEY", "")
    if not key:
        sys.exit("EVAL_KEY не задан")
    extra = json.loads(a.extra) if a.extra else None

    conn = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    if a.ids:
        ids = [int(x) for x in a.ids.split(",")]
        rows = conn.execute(f"select id, app, text from history where id in ({','.join('?'*len(ids))}) order by id desc", ids).fetchall()
    else:
        rows = conn.execute("select id, app, text from history where words >= ? order by id desc limit ?", (a.min_words, a.n)).fetchall()

    total_in = total_out = 0
    rejected = 0
    changed = 0
    lat = []
    for rid, app, text in rows:
        text = text.strip()
        try:
            out, dt, fin, usage = call(a.base, a.model, key, PROMPT, payload(app, text), extra=extra)
        except Exception as e:  # noqa
            print(f"#{rid} [{app}] ОШИБКА: {e}")
            continue
        ok, recall, lost = grounding(text, out)
        lat.append(dt)
        total_in += usage.get("prompt_tokens", 0)
        total_out += usage.get("completion_tokens", 0)
        if not ok:
            rejected += 1
        if out.strip() != text:
            changed += 1
        flag = "OK " if ok else "REJ"
        if not a.quiet or not ok:
            print(f"\n#{rid} [{app}] {flag} recall={recall:.2f} {dt:.1f}s fin={fin}" + (f" lost={lost}" if lost else ""))
            print(f"  ВХОД : {text}")
            print(f"  ВЫХОД: {out}")
    n = len(lat) or 1
    print("\n=== СВОДКА", a.model, "===")
    print(f"диктовок: {len(lat)}  отклонено гардом: {rejected}  изменено: {changed}")
    print(f"латентность: med={sorted(lat)[len(lat)//2]:.2f}s  max={max(lat):.2f}s" if lat else "нет ответов")
    print(f"токены: in={total_in} out={total_out}  (на диктовку in≈{total_in//n} out≈{total_out//n})")

if __name__ == "__main__":
    main()
