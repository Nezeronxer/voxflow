# WORKFLOW — VoxFlow (живой граф задач)

> Статусы: `todo` · `in-progress` · `blocked` · `done`. Двигаемся по разблокированным задачам с макс. приоритетом, переплан — по открытиям (причины → DECISIONS.md).
> Последнее обновление: 2026-06-02.

## Легенда фаз (backbone)
P0 research · P1 каркас · P2 аудио · P3 ASR · P4 хоткей · P5 инжект · P6 LLM · P7 словарь/сниппеты/команды · P8 настройки/статистика/мультиязык/облако · P9 персонализация · P10 упаковка.

---

## Эпики и задачи

### P0 — Research & decisions  ✅
- [done] Проверить тулчейн (rust/cargo/node/npm/git/msvc/cmake) — есть rust+msvc, НЕТ cmake/clang.
- [done] Проверить версии (Tauri 2, whisper.cpp релиз, модели) через веб.
- [done] Зафиксировать стек в DECISIONS.md (D-000…D-010).
- [done] Скаффолд Tauri 2 (react-ts), git init, npm install.

### P1 — Каркас  🔣
- [done] Базовый Tauri проект собирается (`cargo check`/`tauri build` базовый).
- [todo] Добавить Rust-зависимости (cpal, rubato, hound, rdev, enigo, arboard, rusqlite, reqwest, tokio, anyhow, thiserror, parking_lot, log).
- [todo] Трей-иконка + меню (Settings / Toggle dictation / Quit).
- [todo] Окно настроек скрыто по умолчанию, открывается из трея.
- [todo] Overlay-окно (non-activating индикатор записи).
- [todo] IPC команды frontend↔backend (get/set settings, start/stop, status events).
- [todo] Модуль настроек (SQLite) + дефолты.

### P2 — Аудио  🔣
- [todo] cpal: список устройств, выбор, захват f32.
- [todo] rubato: ресемпл → 16 кГц моно.
- [todo] Кольцевой буфер записи + старт/стоп.
- [todo] Энергетический VAD (trim тишины).
- [todo] hound: дамп 16-bit PCM WAV (16 кГц) для whisper.
- [verify] Записать реальный WAV с устройства и проверить параметры (16k/mono/PCM16).

### P3 — Локальный ASR  🔣
- [todo] Скрипт скачивания whisper.cpp `whisper-bin-x64.zip` + распаковка в resources.
- [verify] Подтвердить наличие `whisper-server.exe`/`whisper-cli.exe` + DLL в архиве (R2).
- [todo] Скачать `ggml-base.bin` (смоук) + менеджер моделей (докачка large-v3-turbo-q5_0).
- [todo] Запуск whisper-server как sidecar (управляемый процесс, порт, health-check).
- [todo] Клиент: POST WAV на `/inference` (language=ru) → текст; fallback whisper-cli.
- [verify] Эталонный русский WAV → корректный текст (R4).

### P4 — Хоткей  🔣
- [todo] rdev listen на выделенном потоке.
- [todo] hold-to-talk (Right Ctrl down→старт, up→стоп) + toggle.
- [todo] Перенастройка клавиши из UI.
- [todo] Overlay показывается на время записи + звук старт/стоп.

### P5 — Инжект (вертикальный срез!)  🔣
- [todo] arboard save/set/restore + enigo Ctrl+V.
- [todo] enigo type-fallback.
- [verify] e2e: Right Ctrl → речь → чистый текст появляется в стороннем окне (Notepad/браузер/Telegram).

### P6 — LLM-постобработка
- [todo] Rule-based очиститель (паразиты RU/EN, капитализация, добивка пунктуации).
- [todo] Verbatim-тумблер (постобработка off).
- [todo] Тон (formal/casual/neutral/very casual) через LLM-промпт (Ollama/BYOK), опц.

### P7 — Словарь / сниппеты / Command Mode
- [todo] Личный словарь (biasing whisper `--prompt` + пост-замена форм).
- [todo] Сниппеты/горячие слова (триггер→текст/шаблон, переменные дата/буфер), UI + SQLite.
- [todo] Command Mode (выдели текст → команда → переписать на месте).

### P8 — Настройки / статистика / мультиязык / облако
- [todo] UI настроек: устройство, движок/модель, язык, тон, хоткеи, приватность, автозапуск.
- [todo] Статистика: WPM, streak, всего слов, число приложений.
- [todo] Мультиязык (auto-detect, приоритет RU).
- [todo] Облачные ASR (BYOK): Deepgram/Groq/OpenAI/Qwen — опционально.

### P9 — Персонализация под голос
- [todo] Сбор пар (аудио↔исправленный текст), авто-пополнение словаря.
- [todo] (опц.) LoRA-дообучение — за рамками дефолта.

### P10 — Упаковка
- [done] Иконки/брендинг VoxFlow — концепт «пузырь текста», `.ico` 16–256 (per-size арт) + полный набор Tauri. Проверено визуально. (D-019)
- [todo] Бандл whisper resources + first-run download UX.
- [done] NSIS инсталлятор (Windows) — кастомизирован (своя иконка, фирменный визард, реестр, App Paths, RU). **Установка/удаление проверены вживую** (тихо `/S`, без UAC). `.dmg` (macOS) — позже. (D-019)
- [todo] Автозапуск (opt-in).
- [todo] README с инструкцией.

---

## Статус на 2026-06-02 (вертикальный срез собран)
**Сделано и проверено сборкой/прогоном:**
- P0 ✅ research, решения, скаффолд.
- P1 ✅ каркас: трей (Настройки/Старт-стоп/Выход), окна main(скрыто)+overlay, IPC (17 команд), настройки в SQLite. Бэкенд `cargo check`/`build` — exit 0, 0 warnings. Boot-смоук — стартует без паники.
- P2 ✅(код) аудио: cpal-захват, ресемпл 16к, энергетический VAD, hound WAV. Реальный микрофон — ждёт ручной проверки.
- P3 ✅ ASR: whisper-cli sidecar. **Проверено вживую** (`--selftest` через код приложения) — русский текст дословно. whisper-server — задел.
- P4 ✅(код) хоткей: rdev hold-to-talk (Right Ctrl) + toggle. Интерактивная проверка — за пользователем.
- P5 ✅(код) инжект: arboard+enigo, off-focus через non-activating overlay. e2e в чужое окно — ждёт реального голоса.
- P6 ✅(база) rule-based постобработка (паразиты/капитализация/пунктуация) + verbatim. LLM-тон — каркас.
- P7 ✅(база) словарь + сниппеты (UI+SQLite+биасинг). Command Mode — TODO.
- P8 ✅(база) настройки/статистика/мультиязык. Облачные ASR (BYOK) — TODO.
- P10 🔣 инсталлятор NSIS — `tauri build` идёт.

**Закрытые риски:** R1 (rusqlite/cc — собралось), R2 (whisper-server.exe в архиве — есть), R5 (кириллица — модели/WAV в ASCII-пути).

## Текущий фокус
P10: дождаться NSIS-инсталлятора → README (готов) → финальные доки. Дальше по приоритету: P9 (персонализация), LLM-тон (P6), Command Mode (P7), облако BYOK (P8), macOS.

## Открытые проверки (нужен интерактив/пользователь)
- Реальный микрофон → WAV (cpal на живом устройстве).
- e2e: Right Ctrl → речь → текст в Notepad/Telegram/браузере.
- Визуальная проверка отрисовки React-UI в webview (tsc проходит, рантайм-рендер не заскринен).

## Дополнение 2026-06-02 (продолжение 2) — живой стрим в плашке (D-018)
- [done] Движок стрима выбран (чанки на whisper-server; истинного стрима для RU нет — сверено вебом).
- [done] Backend: `buffer_handle`, `transcribe_server_partial` (ungated), `inject_incremental`, партиал-цикл + `asr_lock` + режимы never/auto/always + защита от смены окна + CPU-фолбэк, `stream_mode`.
- [done] Frontend: плашка слушает `partial` (серый живой текст), растёт до 520px, селектор режима.
- [done] `--stream-selftest` headless-замер: партиал 0.33–0.74с на GPU, префикс стабилен, финал гейтован. Сборка вся exit 0.
- [done] Фикс гонки нажатий между диктовками (`inject_lock`) + minor `last_len`.
- [verify] **Реальная живая диктовка (за пользователем):** ощущение стрима в плашке + инкрементальная вставка в режимах «Всегда»/«Авто» в Notepad/Telegram/браузере.
- [todo] Полировка по желанию: per-tick TOCTOU (R6), выравнивание `DEFAULT_SETTINGS` (hygiene), позиция плашки сверху (бриф 4.3 — сейчас снизу), индикатор `→ <app>` и селектор микрофона прямо в плашке (бриф 4.3, отдельная подзадача с non-activating кликабельным окном).

## Дополнение 2026-06-02 (продолжение 3) — иконка + установщик (D-019)
- [done] Иконка: 3 концепта (попугай/пузырь/точка), выбран «пузырь текста». `.ico` 16–256 per-size (детальный 48+, упрощённый 16/32), прозрачный фон, полный набор Tauri. Проверено визуально.
- [done] Установщик: NSIS Tauri кастомизирован — своя иконка установщика/деинсталлятора, фирменные BMP-визарда (header/sidebar), русский язык, App Paths через installerHooks, метаданные (publisher и пр.). `tauri build` exit 0.
- [done] **E2E вживую:** тихая установка → запись в реестр (Uninstall + App Paths) + ярлыки «Пуск»/рабочий стол → тихое удаление → всё убрано, данные пользователя не тронуты (тест маячками). Без UAC.
- [todo] Развязать install-dir и data-dir (`%LOCALAPPDATA%\VoxFlow` совпадают) — нужна миграция уже скачанных моделей; сейчас не критично (потери данных нет).
- [skip] AUMID-форсинг и WinRT-pin — не нужны/вредны в Tauri (см. D-019); закрепление работает через path-идентичность.
- [verify] (по желанию) пользователь сам прогоняет GUI-установщик, чтобы увидеть фирменный визард глазами.

## Дополнение 2026-06-02 (продолжение 4) — локальный Qwen3 через Ollama (D-021)
- [done] Новый офлайн ИИ-бэкенд Ollama/Qwen3 рядом с Gemini (`ai_backend ∈ {off,gemini,ollama}`), только рефайн текста.
- [done] `ollama.rs`: curl `/api/chat`+`/api/tags`, Qwen non-thinking параметры, тройное глушение reasoning (`/no_think`+`think:false`+срез `<think>`).
- [done] Динамическая таблица профилей `app_context.rs`: verbatim/ai/formal/work/casual/doc/neutral (данно-управляемый массив Rule — добавление приложения = строка).
- [done] Проводка `engine.rs` (диспетч gemini/ollama, verbatim-short-circuit, graceful-деградация), `commands.rs` (ai_test ollama), `settings.rs`+`types.ts` (`ollama_url`/`ollama_model`), `Ai.tsx` (UI + инструкция установки).
- [done] Артефакты `voxflow/ollama/` (Modelfile + README) + `prompts/voiceflow_ru.txt`. Сборка `cargo check`/`npm build` exit 0.
- [done] Adversarial-ревью: 🔴 фикс `strip_think` (одиночный `</think>` у qwen3) + 🟡 2 фронт-минора (stale cloud_asr/result).
- [verify] **Установка Ollama (за пользователем):** `ollama.com/download` → `ollama pull qwen3:4b` → выбрать бэкенд в UI → «Проверить» → e2e-диктовка по профилям.
- [todo] `tauri build` для пересборки exe/инсталлятора (закрыть `voxflow.exe`).
- [todo] (по желанию) захват caret-контекста `[ОКРУЖЕНИЕ]`; передавать модели вычисленный профиль явно (сейчас Qwen роутит по имени приложения из промпта).
