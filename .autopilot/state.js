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
  "updatedAt": "2026-10-16T19:00:17+03:00",
  "finishedAt": "2026-10-16T19:00:17+03:00",
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
      "finishedAt": "2026-10-16T19:00:17+03:00",
      "note": "v2.3.1 опубликован, установлен на мак, полная GigaAM загружена",
      "startedAt": "2026-10-16T18:59:38+03:00"
    }
  ],
  "requirements": {
    "total": 12,
    "done": 7,
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
      "status": "done",
      "reqs": [
        "R06",
        "R07"
      ],
      "startedAt": "2026-10-16T18:57:59+03:00",
      "finishedAt": "2026-10-16T18:59:38+03:00",
      "commit": "-",
      "tests": "DR совпал, хоткей ловится"
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
    "verdict": "3 из 4 выполнены, поиск примеров - частично (следов в коде нет, есть в спеке)"
  }
}
