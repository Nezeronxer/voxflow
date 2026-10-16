window.STATE =
{
  "slug": "voxflow-flow-bar-redesign",
  "title": "VoxFlow: новый кружок и текст, локальная модель, установка",
  "mode": "semi",
  "depth": "deep",
  "polish": null,
  "tier": "T1",
  "briefFile": "2026-10-16-brief.md",
  "memoryFile": "CLAUDE.md",
  "startedAt": "2026-10-16T18:42:57+03:00",
  "updatedAt": "2026-10-16T18:58:07+03:00",
  "finishedAt": "2026-10-16T18:58:07+03:00",
  "stages": [
    {
      "id": "preflight",
      "status": "done",
      "startedAt": "2026-10-16T18:42:57+03:00",
      "finishedAt": "2026-10-16T18:42:57+03:00"
    },
    {
      "id": "manifest",
      "status": "done",
      "startedAt": "2026-10-16T18:42:57+03:00",
      "finishedAt": "2026-10-16T18:42:57+03:00"
    },
    {
      "id": "briefing",
      "status": "done",
      "startedAt": "2026-10-16T18:42:57+03:00",
      "finishedAt": "2026-10-16T18:47:10+03:00"
    },
    {
      "id": "spec",
      "status": "done",
      "startedAt": "2026-10-16T18:47:10+03:00",
      "finishedAt": "2026-10-16T18:47:10+03:00"
    },
    {
      "id": "plan",
      "status": "done",
      "startedAt": "2026-10-16T18:47:10+03:00",
      "finishedAt": "2026-10-16T18:47:10+03:00"
    },
    {
      "id": "build",
      "status": "done",
      "startedAt": "2026-10-16T18:47:10+03:00",
      "finishedAt": "2026-10-16T18:57:59+03:00"
    },
    {
      "id": "review",
      "status": "done",
      "startedAt": "2026-10-16T18:57:59+03:00",
      "finishedAt": "2026-10-16T18:57:59+03:00"
    },
    {
      "id": "final",
      "status": "done",
      "finishedAt": "2026-10-16T18:58:07+03:00",
      "note": "v2.3.1 опубликован, установлен на мак, полная GigaAM загружена"
    }
  ],
  "requirements": {
    "total": 12,
    "done": 12,
    "inTicket": 0,
    "inSpec": 0,
    "placeholder": 0,
    "deferred": 0,
    "dropped": 0
  },
  "tickets": [
    {
      "id": "01",
      "title": "Капсула и живой текст",
      "status": "done",
      "startedAt": "2026-10-16T18:47:10+03:00",
      "reqs": [
        "R01",
        "R02",
        "R03",
        "R04",
        "R05"
      ],
      "finishedAt": "2026-10-16T18:52:43+03:00",
      "commit": "6c0863d",
      "tests": "40/40 фронт",
      "repairs": 1
    },
    {
      "id": "02",
      "title": "Самовыбор локальной модели",
      "status": "done",
      "startedAt": "2026-10-16T18:47:10+03:00",
      "reqs": [
        "R05"
      ],
      "finishedAt": "2026-10-16T18:57:59+03:00",
      "repairs": 2,
      "commit": "4563367",
      "tests": "436 Rust"
    },
    {
      "id": "03",
      "title": "Установка с той же подписью",
      "status": "in-progress",
      "reqs": [
        "R06",
        "R07"
      ],
      "startedAt": "2026-10-16T18:57:59+03:00"
    }
  ],
  "singlePass": null,
  "tests": {
    "passed": 463,
    "failed": 0
  },
  "debt": {
    "placeholders": [],
    "assumptions": [],
    "emptyEnv": []
  },
  "additions": [],
  "coverage": null,
  "blind": {
    "drift": [
      "R01 до решения G01 - боевая модель не менялась"
    ],
    "resolved": [
      "R01 → G01: полная GigaAM основная в 2.3.1",
      "R02/R03/R05: релиз опубликован, установлен, подпись подтверждена на артефакте"
    ],
    "open": [
      "R07: установка из .dmg двойным кликом - один шаг «Всё равно открыть» (без Apple Developer)"
    ]
  }
}
