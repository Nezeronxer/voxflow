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
  "updatedAt": "2026-10-16T18:52:43+03:00",
  "finishedAt": null,
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
      "status": "active",
      "startedAt": "2026-10-16T18:47:10+03:00"
    },
    {
      "id": "review",
      "status": "pending"
    },
    {
      "id": "final",
      "status": "pending"
    }
  ],
  "requirements": {
    "total": 7,
    "done": 0,
    "inTicket": 7,
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
      "commit": "692eccf",
      "tests": "40/40 фронт"
    },
    {
      "id": "02",
      "title": "Самовыбор локальной модели",
      "status": "in-progress",
      "startedAt": "2026-10-16T18:47:10+03:00",
      "reqs": [
        "R05"
      ]
    },
    {
      "id": "03",
      "title": "Установка с той же подписью",
      "status": "pending",
      "reqs": [
        "R06",
        "R07"
      ]
    }
  ],
  "singlePass": null,
  "tests": null,
  "debt": {
    "placeholders": [],
    "assumptions": [],
    "emptyEnv": []
  },
  "additions": [],
  "coverage": null,
  "blind": null
}
