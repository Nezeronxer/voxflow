window.STATE =
{
  "version": 2,
  "slug": "voxflow-memory-diet",
  "dir": "2026-10-22-voxflow-memory-diet--wip",
  "title": "VoxFlow: меньше оперативной памяти, релиз для всех ОС",
  "mode": "semi",
  "depth": "normal",
  "tier": "T1",
  "briefFile": "2026-10-22-brief.md",
  "memoryFile": "CLAUDE.md",
  "memoryOwner": "autopilot",
  "skillDir": "/Users/nezeronxer/.claude/skills/autopilot",
  "baseCommit": null,
  "startedAt": "2026-10-22T20:11:56+03:00",
  "updatedAt": "2026-10-22T20:27:03+03:00",
  "finishedAt": null,
  "stages": [
    {
      "id": "preflight",
      "status": "done",
      "startedAt": "2026-10-22T20:11:56+03:00",
      "finishedAt": "2026-10-22T20:12:30+03:00"
    },
    {
      "id": "manifest",
      "status": "done",
      "startedAt": "2026-10-22T20:12:30+03:00",
      "finishedAt": "2026-10-22T20:14:07+03:00"
    },
    {
      "id": "briefing",
      "status": "done",
      "startedAt": "2026-10-22T20:14:07+03:00",
      "finishedAt": "2026-10-22T20:18:25+03:00"
    },
    {
      "id": "spec",
      "status": "done",
      "startedAt": "2026-10-22T20:18:25+03:00",
      "finishedAt": "2026-10-22T20:26:38+03:00"
    },
    {
      "id": "plan",
      "status": "active",
      "startedAt": "2026-10-22T20:26:38+03:00"
    },
    {
      "id": "build",
      "status": "pending"
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
    "total": 8,
    "done": 0,
    "inTicket": 8,
    "inSpec": 0,
    "placeholder": 0,
    "deferred": 0,
    "dropped": 0
  },
  "tickets": [
    {
      "id": "01",
      "title": "Модели занимают меньше памяти и выгружаются в простое",
      "requirements": [
        "R01",
        "R02",
        "R03",
        "R07i"
      ],
      "blockedBy": [],
      "wave": 1,
      "zone": [
        "voxflow/src-tauri/src/{engine,gigaam,parakeet,vad,settings,local_llm,commands,models,lib,final_eval}.rs",
        "voxflow/src-tauri/Cargo.toml",
        "voxflow/src/",
        "voxflow/tests/",
        "DECISIONS.md"
      ],
      "review": "да — потеря текста (выгрузка модели рядом с горячим путём диктовки, D-024)",
      "model": "сильная",
      "status": "pending",
      "retries": 0,
      "repairs": 0
    },
    {
      "id": "02",
      "title": "Пакет для Arch в релизе и обновление на Arch",
      "requirements": [
        "R05",
        "R06",
        "R08i"
      ],
      "blockedBy": [],
      "wave": 1,
      "zone": [
        ".github/workflows/release-linux.yml",
        ".github/workflows/release.yml",
        "voxflow/src-tauri/src/updater.rs",
        "voxflow/src-tauri/src/linux_env.rs",
        "script/",
        "install",
        "README.md"
      ],
      "review": "нет",
      "model": "сильная",
      "status": "pending",
      "retries": 0,
      "repairs": 0
    },
    {
      "id": "03",
      "title": "Подготовка релиза 2.4.0",
      "requirements": [
        "R04"
      ],
      "blockedBy": [
        "01",
        "02"
      ],
      "wave": 2,
      "zone": [
        "script/check_versions.py",
        "DECISIONS.md",
        "RELEASE_NOTES.md"
      ],
      "review": "нет",
      "model": "обычная",
      "status": "pending",
      "retries": 0,
      "repairs": 0
    }
  ],
  "tests": null,
  "debt": {
    "placeholders": [],
    "assumptions": [],
    "emptyEnv": []
  },
  "additions": [],
  "coverage": {
    "found": 3,
    "fixed": 1,
    "deferred": 2,
    "items": [
      "R05 - дистрибутивы с glibc < 2.38: вне рамок (onnxruntime)",
      "R04 - выпуск ждёт «да» на пуш тега: правило необратимых действий"
    ]
  },
  "concerns": [],
  "blind": null,
  "beats": [
    "2026-10-22T20:11:56+03:00",
    "2026-10-22T20:12:30+03:00",
    "2026-10-22T20:14:07+03:00",
    "2026-10-22T20:17:49+03:00",
    "2026-10-22T20:18:25+03:00",
    "2026-10-22T20:26:38+03:00",
    "2026-10-22T20:26:56+03:00",
    "2026-10-22T20:26:57+03:00",
    "2026-10-22T20:27:03+03:00"
  ],
  "rev": 15,
  "waiting": null,
  "waits": [
    {
      "kind": "briefing",
      "from": "2026-10-22T20:14:07+03:00",
      "to": "2026-10-22T20:17:49+03:00"
    }
  ],
  "log": [
    {
      "t": "2026-10-22T20:11:56+03:00",
      "k": "init"
    },
    {
      "t": "2026-10-22T20:12:30+03:00",
      "k": "stage",
      "id": "manifest",
      "act": "start"
    },
    {
      "t": "2026-10-22T20:14:07+03:00",
      "k": "stage",
      "id": "briefing",
      "act": "start"
    },
    {
      "t": "2026-10-22T20:14:07+03:00",
      "k": "ask",
      "kind": "briefing",
      "note": "4 вопроса — выгрузка в простое, полная GigaAM, диск, пакет под Arch"
    },
    {
      "t": "2026-10-22T20:17:49+03:00",
      "k": "answered",
      "kind": "briefing"
    },
    {
      "t": "2026-10-22T20:18:25+03:00",
      "k": "gate",
      "g": "G1",
      "ok": false,
      "open": 8,
      "total": 8
    },
    {
      "t": "2026-10-22T20:18:25+03:00",
      "k": "stage",
      "id": "spec",
      "act": "start"
    },
    {
      "t": "2026-10-22T20:26:38+03:00",
      "k": "stage",
      "id": "plan",
      "act": "start"
    },
    {
      "t": "2026-10-22T20:26:38+03:00",
      "k": "tier",
      "v": "T1"
    },
    {
      "t": "2026-10-22T20:26:56+03:00",
      "k": "gate",
      "g": "G2",
      "ok": true,
      "found": 3,
      "fixed": 1,
      "deferred": 2
    },
    {
      "t": "2026-10-22T20:26:56+03:00",
      "k": "tickets",
      "n": 3
    },
    {
      "t": "2026-10-22T20:26:57+03:00",
      "k": "gate",
      "g": "G3",
      "ok": false,
      "problems": 2
    },
    {
      "t": "2026-10-22T20:27:03+03:00",
      "k": "tickets",
      "n": 3
    },
    {
      "t": "2026-10-22T20:27:03+03:00",
      "k": "gate",
      "g": "G3",
      "ok": true,
      "problems": 0
    }
  ],
  "reqList": [
    {
      "id": "R01",
      "text": "«оптимизируй, пожалуйста, потребление оперативной памяти»",
      "status": "in-ticket"
    },
    {
      "id": "R02",
      "text": "«и вообще памяти»",
      "status": "in-ticket"
    },
    {
      "id": "R03",
      "text": "«оно потребляет 2 ГБ, это очень много»",
      "status": "in-ticket"
    },
    {
      "id": "R04",
      "text": "«выпусти обновления для всех операционных систем Linux, MC и Windows»",
      "status": "in-ticket"
    },
    {
      "id": "R05",
      "text": "«в основном под все дистрибутивы»",
      "status": "in-ticket"
    },
    {
      "id": "R06",
      "text": "«под арч»",
      "status": "in-ticket"
    },
    {
      "id": "R07i",
      "text": "(подразумевается) распознавание не становится хуже и заметно медленнее",
      "status": "in-ticket"
    },
    {
      "id": "R08i",
      "text": "(подразумевается) обновление доходит до уже установленных копий через встроенное обновление",
      "status": "in-ticket"
    }
  ],
  "gates": {
    "G1": {
      "ok": false,
      "at": "2026-10-22T20:18:25+03:00",
      "open": 8,
      "total": 8
    },
    "G2": {
      "ok": true,
      "at": "2026-10-22T20:26:56+03:00",
      "found": 3,
      "fixed": 1,
      "deferred": 2
    },
    "G3": {
      "ok": true,
      "at": "2026-10-22T20:27:03+03:00",
      "problems": 0
    }
  },
  "result": null
}
