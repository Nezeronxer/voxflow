window.STATE =
{
  "slug": "voxflow-crisp-install",
  "title": "VoxFlow: чёткое распознавание, простая установка на Mac, релиз",
  "mode": "semi",
  "depth": "deep",
  "polish": null,
  "tier": "T2",
  "briefFile": "2026-10-16-brief.md",
  "memoryFile": "CLAUDE.md",
  "startedAt": "2026-10-16T14:00:30+03:00",
  "updatedAt": "2026-10-16T14:30:36+03:00",
  "finishedAt": null,
  "stages": [
    {
      "id": "preflight",
      "status": "done",
      "startedAt": "2026-10-16T14:00:30+03:00",
      "finishedAt": "2026-10-16T14:08:23+03:00"
    },
    {
      "id": "manifest",
      "status": "done",
      "startedAt": "2026-10-16T14:08:23+03:00",
      "finishedAt": "2026-10-16T14:08:23+03:00"
    },
    {
      "id": "briefing",
      "status": "done",
      "startedAt": "2026-10-16T14:08:23+03:00",
      "finishedAt": "2026-10-16T14:12:02+03:00",
      "note": "2 вопроса: Apple Developer нет → самоподпись; точность - максимум с замером"
    },
    {
      "id": "spec",
      "status": "done",
      "startedAt": "2026-10-16T14:12:02+03:00",
      "finishedAt": "2026-10-16T14:15:37+03:00"
    },
    {
      "id": "plan",
      "status": "done",
      "startedAt": "2026-10-16T14:15:37+03:00",
      "finishedAt": "2026-10-16T14:15:37+03:00",
      "note": "5 тасков, ярус T2, 2 волны"
    },
    {
      "id": "build",
      "status": "active",
      "startedAt": "2026-10-16T14:15:37+03:00",
      "note": "1 из 5 тасков готов"
    },
    {
      "id": "review",
      "status": "active",
      "startedAt": "2026-10-16T14:30:36+03:00"
    },
    {
      "id": "final",
      "status": "pending"
    }
  ],
  "requirements": {
    "total": 11,
    "done": 1,
    "inTicket": 9,
    "inSpec": 0,
    "placeholder": 0,
    "deferred": 0,
    "dropped": 0
  },
  "tickets": [
    {
      "id": "01",
      "title": "Постоянная подпись macOS и Linux-релиз",
      "requirements": [
        "R05",
        "R05.1",
        "R05.2",
        "R09i",
        "R10i",
        "R03"
      ],
      "blockedBy": [],
      "wave": 1,
      "zone": [
        "script/build_macos_arm64_release.sh",
        "script/setup-macos-signing.sh",
        ".github/workflows/"
      ],
      "status": "review",
      "retries": 0,
      "repairs": 2,
      "startedAt": "2026-10-16T14:15:37+03:00"
    },
    {
      "id": "02",
      "title": "Установка на Mac одной командой и инструкция",
      "requirements": [
        "R04",
        "R04.1",
        "R04.2",
        "R04.3",
        "R07",
        "R07.1",
        "R08",
        "R11i"
      ],
      "blockedBy": [],
      "wave": 1,
      "zone": [
        "script/install-macos.sh",
        "README.md"
      ],
      "status": "repair",
      "retries": 0,
      "repairs": 2,
      "startedAt": "2026-10-16T14:21:13+03:00"
    },
    {
      "id": "03",
      "title": "Доступы macOS с одного экрана",
      "requirements": [
        "R06",
        "R06.1",
        "R06.2",
        "R06.3"
      ],
      "blockedBy": [],
      "wave": 1,
      "zone": [
        "voxflow/src-tauri/src/macos_permissions.rs",
        "voxflow/src-tauri/src/commands.rs",
        "voxflow/src-tauri/src/lib.rs",
        "voxflow/src/"
      ],
      "status": "done",
      "retries": 0,
      "repairs": 1,
      "startedAt": "2026-10-16T14:15:37+03:00",
      "finishedAt": "2026-10-16T14:30:36+03:00",
      "commit": "a8ab5f5",
      "tests": {
        "passed": 460,
        "failed": 0
      },
      "concerns": [
        "Системное окно микрофона и показ окна при автозапуске без доступов руками не проверены",
        "При автозапуске без доступов окно показывается при каждом входе, пока их не выдадут"
      ]
    },
    {
      "id": "04",
      "title": "Точнее распознавание: замер и принятие",
      "requirements": [
        "R01",
        "R01.1",
        "R01.2",
        "R01.3",
        "R01.4",
        "R01.5"
      ],
      "blockedBy": [],
      "wave": 1,
      "zone": [
        "voxflow/src-tauri/src/models.rs",
        "voxflow/src-tauri/src/gigaam.rs",
        "voxflow/src-tauri/src/final_eval.rs",
        "voxflow/src-tauri/src/engine.rs",
        "DECISIONS.md"
      ],
      "status": "in-progress",
      "retries": 0,
      "repairs": 0,
      "startedAt": "2026-10-16T14:15:37+03:00"
    },
    {
      "id": "05",
      "title": "Релиз 2.3.0: версии и заметки",
      "requirements": [
        "R03",
        "R03.1",
        "R10i.1"
      ],
      "blockedBy": [
        "01",
        "02",
        "03",
        "04"
      ],
      "wave": 2,
      "zone": [
        "версии во всех файлах",
        "RELEASE_NOTES.md",
        "DECISIONS.md"
      ],
      "status": "pending",
      "retries": 0,
      "repairs": 0
    }
  ],
  "singlePass": null,
  "tests": {
    "passed": 460,
    "failed": 0
  },
  "debt": {
    "placeholders": [],
    "assumptions": [
      "Версия 2.3.0 вместо перевешивания тега v2.2.1",
      "Linux-релиз на ubuntu-24.04 (glibc 2.38+), Ubuntu 22.04/Debian 12 не поддерживаются",
      "Неотслеживаемые PROMPT_OPUS5_TEXT_LOSS.md, .gitattributes и conflicted copy не публикуются"
    ],
    "emptyEnv": []
  },
  "additions": [],
  "coverage": {
    "found": 4,
    "fixed": 3,
    "deferred": 1,
    "note": "микрофон (R06.3), «всё остальное» на GitHub, рычаги точности расписаны; .dmg-предупреждение - вне рамок по ответу"
  },
  "blind": null
}
