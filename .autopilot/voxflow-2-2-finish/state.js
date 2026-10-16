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
  "updatedAt": "2026-10-11T19:28:45+03:00",
  "finishedAt": "2026-10-16T14:08:00+03:00",
  "stages": [
    {
      "id": "preflight",
      "status": "done",
      "startedAt": "2026-10-11T18:46:46+03:00",
      "finishedAt": "2026-10-11T18:52:10+03:00"
    },
    {
      "id": "manifest",
      "status": "done",
      "startedAt": "2026-10-11T18:52:10+03:00",
      "finishedAt": "2026-10-11T18:55:02+03:00"
    },
    {
      "id": "briefing",
      "status": "done",
      "startedAt": "2026-10-11T18:55:02+03:00",
      "finishedAt": "2026-10-11T19:03:40+03:00",
      "note": "1 вопрос: релиз — 2.2.1 в конце"
    },
    {
      "id": "spec",
      "status": "done",
      "startedAt": "2026-10-11T19:03:40+03:00",
      "finishedAt": "2026-10-11T19:14:05+03:00"
    },
    {
      "id": "plan",
      "status": "done",
      "startedAt": "2026-10-11T19:10:30+03:00",
      "finishedAt": "2026-10-11T18:51:25+03:00"
    },
    {
      "id": "build",
      "status": "done",
      "finishedAt": "2026-10-11T19:22:00+03:00",
      "startedAt": "2026-10-11T18:51:25+03:00",
      "note": "3 из 3 тасков"
    },
    {
      "id": "review",
      "status": "done",
      "finishedAt": "2026-10-11T19:22:00+03:00",
      "startedAt": "2026-10-11T18:58:42+03:00"
    },
    {
      "id": "final",
      "status": "done",
      "startedAt": "2026-10-11T19:22:00+03:00",
      "note": "закрыт 16.10: тег v2.2.1 запушен, но релиз не вышел (Linux: линковка onnxruntime, glibc 2.38) — доделывается прогоном voxflow-crisp-install",
      "finishedAt": "2026-10-16T14:08:00+03:00"
    }
  ],
  "requirements": {
    "total": 9,
    "done": 1,
    "inTicket": 0,
    "inSpec": 0,
    "placeholder": 0,
    "deferred": 0,
    "dropped": 0
  },
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
      "status": "done",
      "finishedAt": "2026-10-11T19:22:00+03:00",
      "commit": "754c305",
      "tests": {
        "passed": 402,
        "failed": 0
      },
      "concerns": [
        "Переход Waiting/Stopped в петле превью не покрыт юнит-тестом (нужен AppHandle)",
        "Esc в never больше не отменяет финалы прошлых фраз (D01)"
      ],
      "note": "2 круга доработки по ревью",
      "startedAt": "2026-10-11T18:51:25+03:00",
      "retries": 0,
      "repairs": 2
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
      "status": "done",
      "finishedAt": "2026-10-11T19:00:10+03:00",
      "tests": {
        "passed": 390,
        "failed": 0
      },
      "commit": "c8fdff6",
      "concerns": [
        "Перетаскивание плашки на layer surface не работает (отступ фиксирован)",
        "Нужна libgtk-layer-shell 0.6+ (Ubuntu 22.04+)"
      ],
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
      "status": "done",
      "finishedAt": "2026-10-11T19:11:27+03:00",
      "commit": "26ef7d2",
      "tests": {
        "passed": 5,
        "failed": 0
      },
      "startedAt": "2026-10-11T19:09:58+03:00",
      "note": "в master 26ef7d2",
      "retries": 0,
      "repairs": 0
    }
  ],
  "singlePass": null,
  "tests": null,
  "debt": {
    "placeholders": [],
    "assumptions": [
      "AppImage локально на Arch не собирается (linuxdeploy, нет gdk-pixbuf-2.0); релизный AppImage соберёт CI на Ubuntu",
      "Новая зависимость gtk-layer-shell (Wayland-плашка)",
      "Новая фраза в живой печати ждёт вставки прошлой (задержка = время финала прошлой)",
      "Conflicted copies перенесены в ~/.cache, не удалены"
    ],
    "emptyEnv": []
  },
  "additions": [],
  "coverage": {
    "found": 15,
    "fixed": 11,
    "deferred": 4
  },
  "blind": null
}
