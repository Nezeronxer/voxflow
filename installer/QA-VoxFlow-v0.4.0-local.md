# VoxFlow v0.4.0 Local QA

Дата: 2026-06-23

## Артефакты

- Установлено: `%LOCALAPPDATA%\VoxFlow\voxflow.exe`
- Installer: `installer\Output\VoxFlow-Setup-0.4.0.exe`
- Backup перед последней установкой:
  - `%LOCALAPPDATA%\VoxFlow\voxflow.exe.bak.autostyle-mute.20260623-233736`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db.bak.autostyle-mute.20260623-233736`
- Лог установки: `%LOCALAPPDATA%\VoxFlow\install-v040-autostyle-mute.20260623-233736.log`

## Проверено

- `npm run build` — OK.
- `cargo check` — OK.
- `cargo test engine::seg_tests --lib` — OK, 5 тестов.
- `cargo test app_context --lib` — OK, 5 тестов.
- `npm run tauri -- build --no-bundle` — OK, release exe собран.
- Inno Setup compile — OK, installer пересобран.
- Silent install — OK, exit code 0.
- Computer Use:
  - установленный VoxFlow запускается;
  - вкладка `Приложения` открывается;
  - старый текст `Авто-стиль` в UI не найден;
  - `Pre-release` в UI не найден;
  - плитки приложений показывают иконки и selector стиля под приложением;
  - selector содержит ровно три варианта: `Неформальный`, `Формальный`, `Официальный`;
  - вкладка `ИИ` больше не показывает старый переключатель авто-стиля;
  - start/stop диктовки работает, auto-mute пишет `muted` и затем `restored` в debug.log.

## Изменения

- Видимый переключатель `Авто-стиль по приложениям/приложению` убран из `Приложения` и `ИИ`.
- Профили приложений теперь применяются движком всегда, без старого UI-флага.
- Добавлен синхронный restore auto-mute перед tray-exit и Drop guard для нормального завершения.
- Порог автоматического нового абзаца поднят до 4 секунд.
- Инструкция ИИ-редактору уточнена: короткое продолжение сохраняет контекст и не режется в отдельный абзац.

## Остаточные риски

- `cargo fmt` не запускался: в toolchain не установлен `cargo-fmt.exe`.
- Hard kill процесса (`Stop-Process -Force`, Task Manager end task, аварийное выключение питания) не может гарантированно выполнить restore системного mute. Нормальный stop и tray-exit закрыты.
- Accessibility WebView2 не всегда отдаёт видимый текст select без открытия списка; визуально и через открытый dropdown проверено.

## Дополнение 2026-06-24 — double-press, overlay, multilingual

### Артефакты

- Установлено: `%LOCALAPPDATA%\VoxFlow\voxflow.exe` (`LastWriteTime: 2026-06-24 09:00:04`)
- Installer: `installer\Output\VoxFlow-Setup-0.4.0.exe` (`287985994` bytes, `LastWriteTime: 2026-06-24 09:03:37`)
- Backup перед установкой:
  - `%LOCALAPPDATA%\VoxFlow\voxflow.exe.bak.doubletap.20260624-090409`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db.bak.doubletap.20260624-090409`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db-wal.bak.doubletap.20260624-090409`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db-shm.bak.doubletap.20260624-090409`
- Лог установки: `%LOCALAPPDATA%\VoxFlow\install-v040-doubletap.log`

### Проверено

- `npm run build` — OK.
- `cargo check` — OK.
- `cargo test hotkey --lib` — OK, 9 тестов.
- `cargo test language_gate --lib` — OK, 1 тест.
- `cargo test seg_tests --lib` — OK, 6 тестов.
- `cargo test smart_prompt_tests --lib` — OK, 3 теста.
- `npm run tauri -- build --no-bundle` — OK, release exe собран.
- Inno Setup compile — OK, installer пересобран.
- Silent install — OK, exit code 0.
- Computer Use:
  - установленный VoxFlow запускается из `%LOCALAPPDATA%\VoxFlow\voxflow.exe`;
  - вкладка `Модель` показывает `Whisper (все языки)` и новый текст про универсальный локальный движок;
  - список языков открывается и содержит `Все языки (авто)`, `Русский`, `English`, `Українська`, `Deutsch`, `Français`, `Español`, `Italiano`, `Português`, `Polski`, `Türkçe`, `中文`, `日本語`, `한국어`, `العربية`, `हिन्दी`;
  - вкладка `Приложения` показывает плитки с иконками, группу `ПРОМТЫ`, Codex/ChatGPT/Claude и стили под каждым приложением;
  - selector стиля содержит ровно три варианта: `Неформальный`, `Формальный`, `Официальный`;
  - правый клик внутри WebView не открывает WebView context menu;
  - double-press `Right Ctrl` показывает overlay-плашку `2×`, `Режим без удержания`, `Двойное нажатие`;
  - после остановки latch в `debug.log` есть `hotkey: double-press latch enabled`, `auto-mute: system output muted for dictation` и `auto-mute: system output restored`.

### Изменения

- Double-press hotkey теперь шлёт отдельное событие `hotkey_latch`, проигрывает короткий two-tone звук и показывает плавную Aqua-style плашку.
- Overlay получил режим `latch` с 180 ms pop-анимацией и reduced-motion fallback.
- Whisper verbose-json language gate больше не отбрасывает все не-русские языки в режиме `auto`; строгий mismatch-гейт остаётся только для ручных `ru`/`en`.
- Локальный роутер уважает явный выбор `whisper_server`/`whisper_cli`; auto через Parakeet/GigaAM используется только в авто-локальном режиме.
- `no_model` guard теперь смотрит на фактический маршрут: для остальных языков требуется Whisper-модель, а не только наличие GigaAM.
- Prompt-инструкция усилена: короткие продолжения сохраняют контекст и не режутся в отдельный абзац без смены мысли.

### Остаточные риски

- Реальные многоязычные аудиосэмплы не прогонялись: проверены UI-выбор языков, ASR language-gate unit test и маршрут/модельный guard.
- Звук double-press проверен по коду и включённому `play_sounds`; Computer Use не умеет записывать системный звук, поэтому на слух в QA не зафиксировано.

## Дополнение 2026-06-24 — AI model selects and OpenRouter

### Артефакты

- Установлено: `%LOCALAPPDATA%\VoxFlow\voxflow.exe` (`LastWriteTime: 2026-06-24 09:19:24`)
- Installer: `installer\Output\VoxFlow-Setup-0.4.0.exe`
- Backup перед установкой:
  - `%LOCALAPPDATA%\VoxFlow\voxflow.exe.bak.openrouter.20260624-092236`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db.bak.openrouter.20260624-092236`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db-wal.bak.openrouter.20260624-092236`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db-shm.bak.openrouter.20260624-092236`
- Лог установки: `%LOCALAPPDATA%\VoxFlow\install-v040-openrouter.log`

### Проверено

- `npm run build` — OK.
- `cargo check` — OK.
- `cargo test openrouter --lib` — OK, 2 теста.
- `cargo test net:: --lib` — OK, 3 passed, 1 ignored network test.
- `cargo test smart_prompt_tests --lib` — OK, 3 теста.
- `npm run tauri -- build --no-bundle` — OK, release exe собран.
- Inno Setup compile — OK, installer пересобран.
- Silent install — OK, exit code 0.
- Computer Use:
  - установленный VoxFlow запускается из `%LOCALAPPDATA%\VoxFlow\voxflow.exe`;
  - вкладка `ИИ` открывается без крашей;
  - `Бэкенд ИИ` содержит `Облачный (OpenRouter / OpenAI-compatible)`;
  - при выборе облачного backend появляется `Провайдер` как select, по умолчанию `OpenRouter`;
  - `Модель` — select, не текстовое поле; список OpenRouter содержит `OpenAI GPT-5.2`, `OpenAI latest`, `Claude Sonnet 4.5`, `Gemini 2.5 Flash`, `Llama 3.3 70B`;
  - подсказка ключей показывает `REWRITE_API_KEY / OPENROUTER_API_KEY / OPENAI_API_KEY`;
  - `Проверить` без ключа не падает и показывает понятную ошибку про отсутствующий ключ;
  - после QA `ИИ` возвращён в состояние `Выключен`.

### Изменения

- В `ИИ` модели Gemini, Ollama и OpenAI-compatible теперь выбираются через `Select`, а не вводятся вручную.
- Для OpenAI-compatible добавлен выбор провайдера: OpenRouter, Groq, OpenAI, Aqua/Avalon. Провайдер автоматически задаёт Base URL и список моделей.
- OpenRouter поддержан как `https://openrouter.ai/api/v1` + OpenRouter model id; backend добавляет `X-OpenRouter-Title: VoxFlow`.
- `OPENROUTER_API_KEY` добавлен в fallback-цепочку rewrite-ключей.
- Кнопка `Проверить` перед тестом синхронно сохраняет текущие настройки и использует лёгкий `rewrite::ping`, а не production rewrite.

### Остаточные риски

- Реальный OpenRouter-запрос с настоящим ключом не выполнялся: ключи пользователя не запрашивались. Проверены UI, отсутствие сетевого вызова без ключа, env-order и backend route unit tests.

## Дополнение 2026-06-24 — per-network AI prompts

### Артефакты

- Установлено: `%LOCALAPPDATA%\VoxFlow\voxflow.exe` (`33398272` bytes, `LastWriteTime: 2026-06-24 09:38:16`)
- Installer: `installer\Output\VoxFlow-Setup-0.4.0.exe` (`287903429` bytes, `LastWriteTime: 2026-06-24 09:41:54`)
- Backup перед установкой:
  - `%LOCALAPPDATA%\VoxFlow\voxflow.exe.bak.ai-prompts.20260624-093845`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db.bak.ai-prompts.20260624-093845`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db-wal.bak.ai-prompts.20260624-093845`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db-shm.bak.ai-prompts.20260624-093845`
- Лог установки: `%LOCALAPPDATA%\VoxFlow\install-v040-ai-prompts.log`

### Проверено

- `npm run build` — OK.
- `cargo check` — OK.
- `cargo test smart_prompt_tests --lib` — OK, 5 тестов.
- `cargo test app_context::tests --lib` — OK, 6 тестов.
- `cargo test settings::tests --lib` — OK, 2 теста.
- `npm run tauri -- build --no-bundle` — OK, release exe собран.
- Inno Setup compile — OK, installer пересобран.
- Silent install — OK, exit code 0.
- Computer Use:
  - установленный VoxFlow запускается из `%LOCALAPPDATA%\VoxFlow\voxflow.exe`;
  - вкладка `Приложения` открывается без краша;
  - группа `ПРОМТЫ` видна и содержит Codex, ChatGPT, Claude, Gemini, Perplexity, DeepSeek, Grok, OpenRouter;
  - у prompt-приложений виден selector `СТИЛЬ` и отдельное поле `ПРОМТ`;
  - поле ChatGPT принимает ввод через реальный UI и очищается обратно;
  - после очистки в SQLite `ai_prompt_rules_count=0`, тестовая строка `Тест промта VoxFlow` не осталась.

### Изменения

- В настройки добавлено совместимое поле `ai_prompt_rules: [{ match, prompt }]`.
- На вкладке `Приложения` группа `Промты` получила per-network textarea `Промт` под каждой нейросетью.
- Дополнительные AI-сервисы можно добавить через ручные match-правила в блоке `Дополнительные промты`.
- Backend теперь выбирает AI-prompt контекст по встроенной таблице, per-network rule или `tone == ai`, даже если видимый стиль у ChatGPT/Claude/Codex выбран как `Формальный/Неформальный/Официальный`.
- `codex` добавлен во встроенную AI-классификацию.

### Остаточные риски

- `cargo fmt` не запускался: в toolchain не установлен `cargo-fmt.exe`.
- Реальная диктовка с включённым ИИ и пользовательским prompt-rule не гонялась с настоящим ключом/моделью; проверены UI, сохранение/очистка, backend resolver unit tests и установленная сборка.

## Дополнение 2026-06-24 — header free/local wording

### Артефакты

- Установлено: `%LOCALAPPDATA%\VoxFlow\voxflow.exe` (`33511936` bytes, `LastWriteTime: 2026-06-24 10:07:42`)
- Installer: `installer\Output\VoxFlow-Setup-0.4.0.exe` (`287930818` bytes, `LastWriteTime: 2026-06-24 10:10:50`)
- Backup перед установкой:
  - `%LOCALAPPDATA%\VoxFlow\voxflow.exe.bak.header-local.20260624-101106`
  - `%LOCALAPPDATA%\VoxFlow\voxflow.db.bak.header-local.20260624-101106`

### Проверено

- `npm run build` — OK.
- `npm run tauri -- build --no-bundle` — OK, release exe собран.
- Inno Setup compile — OK, installer пересобран.
- Silent install — OK, exit code 0.
- Production `dist` содержит строки `Бесплатная локальная диктовка` и `Бесплатная диктовка · локально`.

### Изменения

- Верхняя подпись бренда заменена на `Бесплатная локальная диктовка`.
- Нижний статус сайдбара заменён на `Бесплатная диктовка · локально`.
- Описание главной страницы заменено на `Бесплатная локальная диктовка: работает на вашем устройстве и готова сразу после запуска.`
