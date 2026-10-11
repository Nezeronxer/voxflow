window.STATE =
{
  "slug": "voxflow-2-2-finish",
  "title": "VoxFlow: доделать 2.2",
  "mode": "semi",
  "depth": "deep",
  "polish": null,
  "tier": "T1",
  "briefFile": "2026-10-11-brief.md",
  "memoryFile": "CLAUDE.md",
  "startedAt": "2026-10-11T18:46:46+03:00",
  "updatedAt": "2026-10-11T19:04:07+03:00",
  "finishedAt": null,
  "stages": [
    { "id": "preflight", "status": "done", "startedAt": "2026-10-11T18:46:46+03:00", "finishedAt": "2026-10-11T18:52:10+03:00" },
    { "id": "manifest",  "status": "done", "startedAt": "2026-10-11T18:52:10+03:00", "finishedAt": "2026-10-11T18:55:02+03:00" },
    { "id": "briefing",  "status": "done", "startedAt": "2026-10-11T18:55:02+03:00", "finishedAt": "2026-10-11T19:03:40+03:00", "note": "1 вопрос: релиз — 2.2.1 в конце" },
    { "id": "spec",      "status": "done", "startedAt": "2026-10-11T19:03:40+03:00", "finishedAt": "2026-10-11T19:14:05+03:00" },
    { "id": "plan",      "status": "done", "startedAt": "2026-10-11T19:10:30+03:00", "finishedAt": "2026-10-11T18:51:25+03:00" },
    { "id": "build",     "status": "active", "startedAt": "2026-10-11T18:51:25+03:00", "note": "1 из 3 тасков · остановлено пользователем" },
    { "id": "review",    "status": "active", "startedAt": "2026-10-11T18:58:42+03:00" },
    { "id": "final",     "status": "pending" }
  ],
  "requirements": { "total": 9, "done": 1, "inTicket": 0, "inSpec": 0, "placeholder": 0, "deferred": 0, "dropped": 0 },
  "tickets": [
  {
    "id": "01",
    "title": "Живая печать: прошлая фраза не теряется",
    "requirements": [
      "R03i"
    ],
    "blockedBy": [],
    "wave": 1,
    "zone": [
      "src-tauri/src/engine.rs"
    ],
    "status": "pending", "note": "остановлено пользователем во время доработки по ревью; коммит f21dc51 в ветке autopilot/t01-live-typing, условия ревью в истории чата",
    "startedAt": "2026-10-11T18:51:25+03:00",
    "retries": 0,
    "repairs": 1
  },
  {
    "id": "02",
    "title": "Плашка поверх окон на Wayland",
    "requirements": [
      "R04i"
    ],
    "blockedBy": [],
    "wave": 1,
    "zone": [
      "src-tauri/src/lib.rs",
      "linux_overlay.rs",
      "Cargo.toml"
    ],
    "status": "done", "finishedAt": "2026-10-11T19:00:10+03:00", "tests": {"passed": 390, "failed": 0}, "commit": "c8fdff6", "concerns": ["Перетаскивание плашки на layer surface не работает (отступ фиксирован)", "Нужна libgtk-layer-shell 0.6+ (Ubuntu 22.04+)"],
    "startedAt": "2026-10-11T18:51:25+03:00",
    "retries": 0,
    "repairs": 2
  },
  {
    "id": "03",
    "title": "Релизная обвязка 2.2.1",
    "requirements": [
      "R02i"
    ],
    "blockedBy": [
      "01",
      "02"
    ],
    "wave": 2,
    "zone": [
      "script/",
      ".github/",
      "версии"
    ],
    "status": "pending",
    "retries": 0,
    "repairs": 0
  }
],
  "singlePass": null,
  "tests": null,
  "debt": { "placeholders": [], "assumptions": ["Новая зависимость gtk-layer-shell (Wayland-плашка)", "Новая фраза в живой печати ждёт вставки прошлой (задержка = время финала прошлой)", "Conflicted copies перенесены в ~/.cache, не удалены"], "emptyEnv": [] },
  "additions": [],
  "coverage": { "found": 15, "fixed": 11, "deferred": 4 },
  "blind": null
}
