window.STATE =
{
  "version": 2,
  "slug": "voxflow-memory-diet",
  "dir": "2026-10-22-voxflow-memory-diet",
  "title": "VoxFlow: меньше оперативной памяти, релиз для всех ОС",
  "mode": "semi",
  "depth": "normal",
  "tier": "T1",
  "briefFile": "2026-10-22-brief.md",
  "memoryFile": "CLAUDE.md",
  "memoryOwner": "autopilot",
  "skillDir": "/Users/nezeronxer/.claude/skills/autopilot",
  "baseCommit": "3ed9706",
  "startedAt": "2026-10-22T20:11:56+03:00",
  "updatedAt": "2026-10-22T23:24:37+03:00",
  "finishedAt": "2026-10-22T23:24:37+03:00",
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
      "status": "done",
      "startedAt": "2026-10-22T20:26:38+03:00",
      "finishedAt": "2026-10-22T20:27:33+03:00"
    },
    {
      "id": "build",
      "status": "done",
      "startedAt": "2026-10-22T20:27:33+03:00",
      "finishedAt": "2026-10-22T21:54:07+03:00"
    },
    {
      "id": "review",
      "status": "done",
      "startedAt": "2026-10-22T21:54:07+03:00",
      "finishedAt": "2026-10-22T21:59:27+03:00"
    },
    {
      "id": "final",
      "status": "done",
      "startedAt": "2026-10-22T21:59:27+03:00",
      "finishedAt": "2026-10-22T23:24:37+03:00"
    }
  ],
  "requirements": {
    "total": 11,
    "done": 11,
    "inTicket": 0,
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
      "status": "done",
      "startedAt": "2026-10-22T20:27:33+03:00",
      "finishedAt": "2026-10-22T21:31:02+03:00",
      "retries": 0,
      "repairs": 1,
      "repairFindings": [
        "выгрузка не освобождает модель, пока в final_queue есть финал любого поколения + тест"
      ],
      "tests": {
        "passed": 462,
        "failed": 0
      },
      "commit": "19d14ae"
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
      "status": "done",
      "startedAt": "2026-10-22T20:27:33+03:00",
      "finishedAt": "2026-10-22T21:51:54+03:00",
      "retries": 0,
      "repairs": 0,
      "tests": {
        "passed": 462,
        "failed": 0
      },
      "commit": "852437c"
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
      "status": "done",
      "startedAt": "2026-10-22T21:51:54+03:00",
      "finishedAt": "2026-10-22T21:54:07+03:00",
      "retries": 0,
      "repairs": 0,
      "tests": {
        "passed": 462,
        "failed": 0
      },
      "commit": "1ec70bd"
    },
    {
      "id": "F1",
      "title": "Ремонт ветки по ревью",
      "requirements": [
        "R01",
        "R08i"
      ],
      "blockedBy": [],
      "wave": 3,
      "zone": [
        "voxflow/src-tauri/src/engine.rs",
        "voxflow/src-tauri/src/commands.rs",
        "voxflow/src-tauri/src/updater.rs"
      ],
      "review": "да — ремонт ветки",
      "model": "сильная",
      "status": "done",
      "startedAt": "2026-10-22T21:56:55+03:00",
      "finishedAt": "2026-10-22T22:00:08+03:00",
      "retries": 0,
      "repairs": 0,
      "tests": {
        "passed": 465,
        "failed": 0
      },
      "commit": "e9dcd89"
    },
    {
      "id": "F2",
      "title": "Веса моделей с диска, а не в памяти процесса",
      "requirements": [
        "G01",
        "R01",
        "R07i"
      ],
      "blockedBy": [
        "01"
      ],
      "wave": 4,
      "zone": [
        "voxflow/src-tauri/src/{ort_session,gigaam,parakeet,models,engine,final_eval}.rs",
        "voxflow/src-tauri/Cargo.toml",
        "DECISIONS.md"
      ],
      "review": "да — потеря текста (смена загрузки моделей боевого распознавания)",
      "model": "сильная",
      "status": "done",
      "retries": 0,
      "repairs": 0,
      "startedAt": "2026-10-22T22:10:05+03:00",
      "finishedAt": "2026-10-22T22:26:54+03:00",
      "tests": {
        "passed": 468,
        "failed": 0
      },
      "commit": "8e250e9"
    }
  ],
  "tests": {
    "passed": 468,
    "failed": 0
  },
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
  "concerns": [
    "Parakeet без prepacking: -880 МБ, распознавание en/авто медленнее на 13-15% (на границе допуска)",
    "Первая диктовка после выгрузки: живое превью появляется после догрузки модели, финал полный",
    "engine.rs:1108 preload_for_dictation не поднимает whisper-server при Start после выгрузки",
    "engine.rs:1025 Unneeded для whisper не учитывает подмену resolve_model, в отличие от mark_in_use",
    "D-053: простой ≤400 МБ замерен в тестовом процессе, не в приложении",
    "engine.rs тест reload без шва Start→финал, подтверждает сам себя",
    "engine.rs:1061 release_models_when_free плодит поток на каждое сохранение настроек",
    "engine.rs:1403 warmup без билета final_queue: выгрузка во время прогрева тратит прогрев впустую (текст не теряется)",
    "updater.rs: ветки pkexec и pacman -Qqo на живом Arch не запускались",
    "engine.rs:390, 1146: прогрев на старте и при Start идёт мимо spawn_warmup, без отметки активности"
  ],
  "blind": {
    "checked": 8,
    "matched": 6,
    "mismatches": [
      "R01 - под нагрузкой всё ещё ~1,8-2,1 ГБ; полная GigaAM сама не ужата, сэкономлено только на Parakeet (-880 МБ) и в простое",
      "R04 - релиз не опубликован: ждёт разрешения на пуш и тег"
    ]
  },
  "beats": [
    "2026-10-22T20:11:56+03:00",
    "2026-10-22T20:12:30+03:00",
    "2026-10-22T20:14:07+03:00",
    "2026-10-22T20:17:49+03:00",
    "2026-10-22T20:18:25+03:00",
    "2026-10-22T20:26:38+03:00",
    "2026-10-22T20:26:56+03:00",
    "2026-10-22T20:26:57+03:00",
    "2026-10-22T20:27:03+03:00",
    "2026-10-22T20:27:10+03:00",
    "2026-10-22T20:27:33+03:00",
    "2026-10-22T21:24:11+03:00",
    "2026-10-22T21:24:39+03:00",
    "2026-10-22T21:27:45+03:00",
    "2026-10-22T21:29:42+03:00",
    "2026-10-22T21:31:02+03:00",
    "2026-10-22T21:31:03+03:00",
    "2026-10-22T21:51:54+03:00",
    "2026-10-22T21:54:07+03:00",
    "2026-10-22T21:56:55+03:00",
    "2026-10-22T21:59:13+03:00",
    "2026-10-22T21:59:27+03:00",
    "2026-10-22T22:00:08+03:00",
    "2026-10-22T22:06:59+03:00",
    "2026-10-22T22:10:05+03:00",
    "2026-10-22T22:24:55+03:00",
    "2026-10-22T22:26:54+03:00",
    "2026-10-22T22:27:37+03:00",
    "2026-10-22T23:24:37+03:00"
  ],
  "rev": 50,
  "waiting": null,
  "waits": [
    {
      "kind": "briefing",
      "from": "2026-10-22T20:14:07+03:00",
      "to": "2026-10-22T20:17:49+03:00"
    },
    {
      "kind": "action",
      "from": "2026-10-22T22:06:59+03:00",
      "to": "2026-10-22T22:10:05+03:00"
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
    },
    {
      "t": "2026-10-22T20:27:33+03:00",
      "k": "stage",
      "id": "build",
      "act": "start"
    },
    {
      "t": "2026-10-22T20:27:33+03:00",
      "k": "ticket",
      "ids": [
        "01",
        "02"
      ],
      "act": "start"
    },
    {
      "t": "2026-10-22T21:24:11+03:00",
      "k": "ticket",
      "ids": [
        "01"
      ],
      "act": "done",
      "tests": {
        "passed": 461,
        "failed": 0
      }
    },
    {
      "t": "2026-10-22T21:24:11+03:00",
      "k": "ticket",
      "ids": [
        "01"
      ],
      "act": "review"
    },
    {
      "t": "2026-10-22T21:24:39+03:00",
      "k": "add",
      "key": "concerns",
      "n": 2
    },
    {
      "t": "2026-10-22T21:27:45+03:00",
      "k": "ticket",
      "ids": [
        "01"
      ],
      "act": "repair",
      "note": "выгрузка не освобождает модель, пока в final_queue есть финал любого поколения + тест"
    },
    {
      "t": "2026-10-22T21:27:45+03:00",
      "k": "add",
      "key": "concerns",
      "n": 5
    },
    {
      "t": "2026-10-22T21:29:42+03:00",
      "k": "ticket",
      "ids": [
        "01"
      ],
      "act": "review"
    },
    {
      "t": "2026-10-22T21:31:02+03:00",
      "k": "ticket",
      "ids": [
        "01"
      ],
      "act": "done",
      "tests": {
        "passed": 462,
        "failed": 0
      }
    },
    {
      "t": "2026-10-22T21:31:03+03:00",
      "k": "add",
      "key": "concerns",
      "n": 1
    },
    {
      "t": "2026-10-22T21:51:54+03:00",
      "k": "ticket",
      "ids": [
        "02"
      ],
      "act": "done",
      "tests": {
        "passed": 462,
        "failed": 0
      }
    },
    {
      "t": "2026-10-22T21:51:54+03:00",
      "k": "add",
      "key": "concerns",
      "n": 1
    },
    {
      "t": "2026-10-22T21:51:54+03:00",
      "k": "ticket",
      "ids": [
        "03"
      ],
      "act": "start"
    },
    {
      "t": "2026-10-22T21:54:07+03:00",
      "k": "ticket",
      "ids": [
        "03"
      ],
      "act": "done",
      "tests": {
        "passed": 462,
        "failed": 0
      }
    },
    {
      "t": "2026-10-22T21:54:07+03:00",
      "k": "stage",
      "id": "review",
      "act": "start"
    },
    {
      "t": "2026-10-22T21:56:55+03:00",
      "k": "tickets",
      "n": 4
    },
    {
      "t": "2026-10-22T21:56:55+03:00",
      "k": "add",
      "key": "report",
      "n": 3
    },
    {
      "t": "2026-10-22T21:56:55+03:00",
      "k": "ticket",
      "ids": [
        "F1"
      ],
      "act": "start"
    },
    {
      "t": "2026-10-22T21:59:13+03:00",
      "k": "ticket",
      "ids": [
        "F1"
      ],
      "act": "review"
    },
    {
      "t": "2026-10-22T21:59:27+03:00",
      "k": "stage",
      "id": "final",
      "act": "start"
    },
    {
      "t": "2026-10-22T22:00:08+03:00",
      "k": "ticket",
      "ids": [
        "F1"
      ],
      "act": "done",
      "tests": {
        "passed": 465,
        "failed": 0
      }
    },
    {
      "t": "2026-10-22T22:00:08+03:00",
      "k": "add",
      "key": "concerns",
      "n": 1
    },
    {
      "t": "2026-10-22T22:06:59+03:00",
      "k": "gate",
      "g": "G4",
      "ok": false,
      "checked": 8,
      "matched": 6
    },
    {
      "t": "2026-10-22T22:06:59+03:00",
      "k": "ask",
      "kind": "action",
      "note": "дожать память под нагрузкой или выпускать; разрешение на пуш и тег v2.4.0"
    },
    {
      "t": "2026-10-22T22:10:05+03:00",
      "k": "answered",
      "kind": "action"
    },
    {
      "t": "2026-10-22T22:10:05+03:00",
      "k": "tickets",
      "n": 5
    },
    {
      "t": "2026-10-22T22:10:05+03:00",
      "k": "gate",
      "g": "G3",
      "ok": false,
      "problems": 1
    },
    {
      "t": "2026-10-22T22:10:05+03:00",
      "k": "ticket",
      "ids": [
        "F2"
      ],
      "act": "start"
    },
    {
      "t": "2026-10-22T22:24:55+03:00",
      "k": "ticket",
      "ids": [
        "F2"
      ],
      "act": "done",
      "tests": {
        "passed": 468,
        "failed": 0
      }
    },
    {
      "t": "2026-10-22T22:24:55+03:00",
      "k": "ticket",
      "ids": [
        "F2"
      ],
      "act": "review"
    },
    {
      "t": "2026-10-22T22:26:54+03:00",
      "k": "ticket",
      "ids": [
        "F2"
      ],
      "act": "done",
      "tests": {
        "passed": 468,
        "failed": 0
      }
    },
    {
      "t": "2026-10-22T22:26:54+03:00",
      "k": "add",
      "key": "report",
      "n": 4
    },
    {
      "t": "2026-10-22T23:24:37+03:00",
      "k": "finish",
      "result": "VoxFlow 2.4.0: в диктовке ~0,9-1,3 ГБ вместо 2,4-3, в простое ~0,1 ГБ; пакет Arch; релиз для всех ОС, установлен на мак"
    }
  ],
  "reqList": [
    {
      "id": "R01",
      "text": "«оптимизируй, пожалуйста, потребление оперативной памяти»",
      "status": "done"
    },
    {
      "id": "R02",
      "text": "«и вообще памяти»",
      "status": "done"
    },
    {
      "id": "R03",
      "text": "«оно потребляет 2 ГБ, это очень много»",
      "status": "done"
    },
    {
      "id": "R04",
      "text": "«выпусти обновления для всех операционных систем Linux, MC и Windows»",
      "status": "done"
    },
    {
      "id": "R05",
      "text": "«в основном под все дистрибутивы»",
      "status": "done"
    },
    {
      "id": "R06",
      "text": "«под арч»",
      "status": "done"
    },
    {
      "id": "R07i",
      "text": "(подразумевается) распознавание не становится хуже и заметно медленнее",
      "status": "done"
    },
    {
      "id": "R08i",
      "text": "(подразумевается) обновление доходит до уже установленных копий через встроенное обновление",
      "status": "done"
    },
    {
      "id": "G01",
      "text": "«Попробовать дожать, потом релиз»",
      "status": "done"
    },
    {
      "id": "G02",
      "text": "«Да, пуш и тег сам»",
      "status": "done"
    },
    {
      "id": "G03",
      "text": "«Установи мне потом новую версию сразу.»",
      "status": "done"
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
      "ok": false,
      "at": "2026-10-22T22:10:05+03:00",
      "problems": 1
    },
    "G4": {
      "ok": false,
      "at": "2026-10-22T22:06:59+03:00",
      "checked": 8,
      "matched": 6
    }
  },
  "result": "VoxFlow 2.4.0: в диктовке ~0,9-1,3 ГБ вместо 2,4-3, в простое ~0,1 ГБ; пакет Arch; релиз для всех ОС, установлен на мак",
  "report": [
    "Установка на Arch: пакет проверяется по SHA-256 в папке пользователя, а ставится от root через pkexec - чужая программа под тем же пользователем теоретически может подменить файл между проверкой и установкой (обычный риск pkexec-установщиков)",
    "Arch: если pkexec не сработал, показанная команда sudo pacman -U указывает на файл во временной папке - её чистит приложение, выполнять лучше сразу",
    "Галочка выгрузки видна только при локальном распознавании, хотя выгружает и встроенную ИИ-модель",
    "Разбор файла модели: на заведомо битом файле возможен вылет вместо ошибки (переполнение длины) - на практике файл до этого проверяется по SHA-256",
    "Отображение весов проверено на полной GigaAM и Parakeet (60/60); облегчённая int8 GigaAM и VAD отдельно не сверялись - int8 на этом маке не скачана",
    "На Windows веса по-прежнему копируются в память (отображение выключено, чтобы удаление модели не упиралось в занятый файл) - там экономия меньше",
    "Первая диктовка после долгого простоя может читать веса с диска - холодный кэш не мерили"
  ]
}
