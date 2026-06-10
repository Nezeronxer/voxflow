# NOTES — рабочие заметки сессии production-ready (2026-06-10)

> Выжимка: VoxFlow = Tauri 2 + React 19 (фронт) + Rust (бэк), whisper.cpp sidecar (server :8771), Inno-инсталлятор.
> План сессии: RU-пайплайн → GigaAM-v3 e2e RNNT (ONNX через крейт `ort`, CPU), Silero VAD, инжект-очередь,
> orb-оверлей по спеке Aqua, темы светлая/тёмная, авто-скачивание моделей при первом запуске.
> Репо БЕЗ git-коммитов → работу делим по непересекающимся файлам.

## Состояние машины (проверено 2026-06-10)

- rustc/cargo 1.93.1, node 24.15, MSVC 14.44 (= VS 17.14+, достаточно для статической линковки ort). cmake/clang НЕТ — ничего, что требует C++-сборок, не брать.
- CPU i5-12400F (12 потоков), RAM 32 ГБ; NVIDIA GPU есть (whisper-cuda работает), но целевая метрика — CPU.
- Прокси обязателен: `HTTPS_PROXY=http://127.0.0.1:10808`, HuggingFace доступен (307 за 0.55с).
- Модели whisper НА МЕСТЕ (память от 04.06 устарела): `%LOCALAPPDATA%\VoxFlow\models\` — base, large-v3-turbo-q5_0, large-v3-turbo fp16.
- ISCC (Inno Setup 6) установлен.
- Бинарь от 04.06: `voxflow/src-tauri/target/release/voxflow.exe`.

## БД пользователя была ПОВРЕЖДЕНА (найдено и исправлено в этой сессии)

- `%LOCALAPPDATA%\VoxFlow\voxflow.db` — «database disk image is malformed» даже на read-only integrity_check
  → установленное приложение падало на старте (lib.rs:194 open db). Вероятная причина: жёсткое убийство процесса
  посреди записи (journal=DELETE, без busy_timeout). ВАЖНО: это может быть главной видимой «поломкой» у пользователя.
- Настройки вытащены из сырых байтов файла целиком (37 ключей, включая ключ Groq) →
  `%LOCALAPPDATA%\VoxFlow\backup_db_20260610\settings_recovered.json` (+ копия битого .db там же).
- Урок: перед ЛЮБЫМ запуском бинаря, открывающего БД, сначала бэкапить db+wal+shm. Я запустил селфтест до бэкапа —
  WAL зачекпойнтился и исчез (вероятно, шанса на восстановление через WAL уже не было, но порядок был неверный).
- Фикс в коде: db.rs — при «malformed» на открытии переносить файл в `voxflow.db.corrupt-<ts>` и создавать заново
  + включить WAL и busy_timeout (см. Задачи).

## Базовая латентность (до переделки, замерено)

- Холодный `--selftest` (whisper-server GPU, fp16 1.6ГБ): **18.2 с** на всё (доминируют старт сервера + загрузка модели + CUDA JIT). Горячий запрос по PROGRESS: ~0.36с GPU.
- Облако Groq whisper-large-v3 (текущий дефолт юзера): 1.6–2.7 с на фразу + сеть/прокси.
- Цель: ≤500 мс от конца фразы до вставки на CPU.

## STT-пайплайн (как есть)

- Поток: hotkey(rdev, mpsc) → engine_loop(`voxflow-engine`) → cpal-захват в Arc<Mutex<Vec<f32>>> → отпускание →
  stop_and_process (engine.rs:786) → detached-поток process_utterance (engine.rs:880–1158): ресемпл 16к → trim_silence(RMS 0.01) →
  ASR (приоритет: openai_compat/deepgram при ключе → gemini → локальный whisper) → гейт уверенности (verbose_json) →
  postprocess (правила+словарь+corrections) → опц. LLM-рефайн (СИНХРОННО, +1–10с!) → gen-guard → inject под inject_lock.
- Партиалы: partial_loop каденс 500мс, min_new 0.3с, try_lock(asr_lock), PrefixStabilizer(N6,K2) → событие `partial {text,committed,volatile,seq}`.
  Облачный черновик: каденс 2с, кап 4 запроса.
- whisper-server: порт 8771, прогрев при старте (1.2с задержка), выбор CPU/GPU по наличию nvcuda.dll (paths.rs:50).
- Замеров по этапам НЕТ (только итоговое ms в transcript) → добавить.

## Вставка (как есть) — причины «зависает текст»

- inject.rs:89–140 paste_text: arboard set_text → sleep 25мс → enigo Ctrl+V → sleep 130мс → восстановление буфера. Всё под inject_lock, в detached-потоке (НЕ UI). 
- У ПОЛЬЗОВАТЕЛЯ в БД было `paste_method="type"` → enigo печатает ДЛИННЫЙ текст ПОСИМВОЛЬНО — на 2–3 мин диктовки это десятки секунд «печатания», выглядит как зависание. Главная причина симптома.
- Вторичные: clipboard_monitor-поток опрашивает буфер каждые 1.3с (engine.rs:1458) → контеншн с arboard во время вставки; нет очереди вставки (ad-hoc detached-потоки + mutex); фиксированные sleep'ы держат inject_lock.
- Фикс: выделенный inject-воркер с упорядоченной очередью (mpsc), pause-флаг для clipboard_monitor на время вставки, дефолт paste_method="clipboard", восстановление буфера вне критического пути.

## Overlay (как есть) → orb

- Сейчас: пилюля 600×300 окно, anchor снизу-центр, click-through ВСЁ окно, без перетаскивания/запоминания позиции. Состояния idle/recording/transcribing. Пульсации по громкости НЕТ (события уровня нет вообще).
- Переиспользуем: rAF-движок посимвольной печати, PrefixStabilizer, GPU-анимации (transform/opacity), события partial/status/stt_mode.
- Спека Aqua Voice снята 1:1 из локального app.asar (Electron v0.14.17): пилюля #000 r30px снизу-центр; idle 55×10; hover 80×20; recording 110×37; processing scale .96 + спиннер (border-top белый, 0.4s); 12 баров 3px/gap2, высота `2+20·v^1.5` (2–22px), spring(1200,20,1); контейнер spring(1800,45,.1); орб 13px сине-голубой с glow `0.5+5.5·log10(1+3v)`. Бэкенд должен слать RMS-уровень (~30 Гц, событие `level`).

## UI (как есть)

- Одна светлая ч/б editorial-тема (styles.css, 7 серых, IBM Plex + Unbounded, забандлены с кириллицей). Тёмной темы НЕТ.
- Все команды через invoke + safe(), блокирующих операций в рендере нет. FpsMeter есть (localStorage voxfps=1).
- Слабости: нет токенов спейсинга, секции-таблицы скопипащены, тосты без автозакрытия, прогресс моделей без ETA.

## Сборка/установка (как есть)

- `tauri build --no-bundle` → exe 13МБ; resources: whisper CPU 3.1МБ + whisper-cuda 698МБ (cublas DLL).
- installer/VoxFlow.iss: per-user `%LOCALAPPDATA%\VoxFlow`, без UAC, AppId {B2F1A9E0-…F80} НЕ МЕНЯТЬ, ставит exe+оба whisper, модели качаются через models.rs (curl, прогресс поллингом .part каждые 400мс, события model:*).
- CI build-installer.yml: tauri build --no-bundle → ISCC. Предполагает закоммиченные whisper-бинари.
- Грабля Inno: setup.exe перезапускает себя дочерним процессом → проверять установку поллингом реестрового ключа `..._is1`, не `-Wait`. Не запускать установщик «от администратора» (запись уйдёт в HKCU админа).

## Решения этой сессии (обоснования)

- **D-S1. RU-движок = GigaAM-v3 e2e RNNT int8 ONNX** (HF `istupakov/gigaam-v3-onnx`, MIT): encoder int8 214МБ + decoder int8 1.2МБ + joint int8 0.7МБ + vocab 13КБ. RTFx≈42 на CPU (9800X3D), на i5-12400F ожидаю RTFx 15–25 → фраза 5с ≈ 200–350мс. e2e = пунктуация+капитализация+ITN из коробки → LLM-рефайн для нормального текста не нужен. int8 vs fp32: одинаковая скорость, в 4 раза меньше диска.
- **D-S2. Инференс через крейт `ort` 2.0.0-rc.12** (статическая линковка download-binaries, БЕЗ cmake; CPU EP; onnxruntime.dll не нужен). Никаких sherpa-onnx (cmake) и Python-сайдкаров.
- **D-S3. Препроцессинг GigaAM**: 16кГц → log-mel 64 (n_fft=320, hop=160, периодический Hann, HTK-мел 0–8000, без нормализации, ln(clip(x,1e-9,1e9)), center=false; окно+fbanks через bf16-округление как в эталоне). Greedy RNNT: blank=1024, max 3 токена/кадр, кэш dec_out при blank; vocab «токен␣id», ▁→пробел.
- **D-S4. EN/смешанная речь = существующий whisper.cpp** (large-v3-turbo на GPU, q5_0 на CPU). Parakeet TDT v3 отвергнут: +650МБ дистрибуции, по русскому радикально слабее GigaAM, а EN — вторичный сценарий; whisper уже встроен (нулевая цена). На чистом CPU whisper-turbo НЕ влезает в 500мс — это осознанный компромисс только для EN-режима.
- **D-S5. Silero VAD v6 ONNX** (~2.3МБ) бандлим как ресурс: чанк 512+64 контекст @16к, state [2,1,128], порог 0.5/0.35, min_silence для диктовки ~600мс. Применение: гейт партиал-тиков (не гонять ASR по тишине), отсечка тишины, авто-границы фраз для стриминга.
- **D-S6. Дефолты**: stt_provider="local", engine="gigaam" (язык ru), paste_method="clipboard". Облако (Groq) остаётся опцией — ключ юзера сохранён. stream_mode остаётся "never" (юзер ЯВНО просил не печатать в поле во время речи, 02.06) — «текст по мере диктовки» живёт в плашке-орбе.
- **D-S7. Модели GigaAM качаются при первом запуске** с прогрессом (инфраструктура models.rs уже на 90% готова), НЕ вшиваются в инсталлятор (216МБ + обновляемость). Silero (2МБ) — вшит.
- **D-S8. Очередь вставки** — выделенный воркер-поток с mpsc-очередью, порядок гарантирован каналом; clipboard_monitor приостанавливается атомарным флагом на время вставки.
- **D-S9. Темы** — поле theme ∈ {system,light,dark}, data-theme на html, существующие CSS-переменные перекрашиваются, стек не трогаем.
- **D-S10. БД** — WAL + busy_timeout(5s) + восстановление при malformed (rename → recreate). 

## План этапов (репо без коммитов → работа по непересекающимся файлам)

- Этап 1 (параллельно): gigaam.rs(new), vad.rs(new), inject.rs, Overlay.tsx+overlay.css(new), styles.css+секции (темы/полировка), models.rs+Models.tsx (каталог GigaAM+авто-скачивание), db.rs (recovery+WAL), installer/VoxFlow.iss.
- Этап 2 (серийно): engine.rs, lib.rs, settings.rs, types.ts, commands.rs, Cargo.toml — интеграция, роутинг ru→gigaam, событие level, латентность-лог по этапам.
- Этап 3: сборка + headless-селфтесты (gigaam-selftest на dataset/*.wav, замеры этапов) + тест длинной диктовки/очереди вставки.
- Этап 4: верификация задач 1–5 со свежим контекстом + ISCC + установка в песочницу + финальный отчёт.

## Реализация (статус по файлам)

- **gigaam.rs**: фронтенд log-mel64 (честный DFT-320 с таблицами, bf16-окно/банк), 3 ort-сессии, greedy RNNT
  с кэшем dec_out при blank и коммитом (h,c) от породившего кэш прогона; clean_spaces = семантика
  DECODE_SPACE_PATTERN. Грабля ort rc.12: `ort::Error<R>` НЕ Send+Sync и генерик → трейт-адаптер `.oc()` → anyhow.
  IO-имена: encoder audio_signal/length→encoded/encoded_len; decoder x,h.1,c.1→dec,h,c; joint enc,dec→joint.
- **vad.rs**: Silero v6, input[1,576]=64ctx+512, state[2,1,128], sr — process_chunk/has_speech.
- **inject.rs**: воркер "voxflow-inject" + FIFO mpsc, ack на job, is_busy() на счётчике PENDING,
  ретрай set_text ×3, dry-режим VOXFLOW_INJECT_DRY=1 + dry_log() для тестов.
- **db.rs**: open_at() с quick_check → rename в .corrupt-<ts> + пересоздание; WAL+busy_timeout 5000.
- **engine.rs**: EngineCtx+gigaam/vad резиденты; warmup грузит VAD всегда + GigaAM при engine=gigaam
  (whisper-server при gigaam НЕ поднимаем); spawn_level_loop (событие "level" rms 0..1 каждые 33мс);
  gigaam_partial_loop — СЕГМЕНТНАЯ схема: стриминговый VAD по новым сэмплам, пауза ≥600мс или сегмент ≥25с
  закрывает сегмент → текст фиксируется в committed (монотонно по построению), активный кусок = volatile;
  тишину не распознаём вообще. local_asr: VAD-гейт → GigaAM (чанк >28с режется по VAD-паузам) → фолбэк whisper.
  [lat]-лог: pre/asr(+frontend/encoder/decoder)/post/llm/inject/total в debug.log. clipboard_monitor скипает
  тики при inject::is_busy().
- **Overlay** (2 захода): Overlay.tsx+overlay.css, классы aq-*, состояния idle/rec/stream/trans/done/notice,
  спринги k=420/c=30, окно НЕ click-through, drag по пилюле (data-tauri-drag-region), overlay_box на смене состояния.
  Размеры окон: idle/done 220×80, rec/trans 140×64, stream 424×168, notice 424×92 (логич. px).
- **Темы**: [data-theme="dark"] полный сет токенов (инверс-блоки в dark — светлые), init из localStorage
  "vf-theme" до рендера, matchMedia для system, переключатель в Control «Вид»; тосты автозакрытие 6с, tab-fade,
  focus-visible, prefers-reduced-motion.
- **models.rs/Models.tsx**: каталог "gigaam-v3" (4 файла, суммарный прогресс, докачка по размеру),
  hero-карточка с ETA/скоростью (EMA 0.3), ensure_default_models() — воткнут в setup lib.rs (первый запуск
  качает GigaAM автоматически). Защита от двойного скачивания AtomicBool.
- **lib.rs/commands.rs**: команда overlay_box (низ на месте, центр по X), позиция overlay в kv "overlay_pos"
  (debounce 600мс на Moved), восстановление при старте с проверкой «на экране»; --gigaam-selftest CLI.
- **Инсталлятор**: VoxFlow.iss 0.2.0 + resources\vad; tauri.conf.json 0.2.0 + resources/vad/*.
- **capabilities**: + window:allow-start-dragging/set-size/set-position.

## Проверено в этой сессии (только реальные прогоны)

- 2026-06-10: baseline `--selftest` 18.2с холодный (GPU fp16, доминирует загрузка); Groq-облако (старый дефолт юзера) 1.6–2.7с/фразу.
- БД юзера была malformed → приложение падало на старте; настройки вытащены из сырых байтов и восстановлены
  (ключ Groq цел), новая БД integrity ok; дефолты обновлены (engine=gigaam, stt=local, paste=clipboard).
- `cargo test --lib` (7 тестов: db×2, gigaam×2, vad×1, inject×2) — ВСЕ ЗЕЛЁНЫЕ.
- **GigaAM на реальном голосе (debug-сборка!)**: фраза 2.1с → 142мс (frontend 75 / encoder 61 / decoder 6);
  фраза 16.1с → 891мс. Текст дословный, с пунктуацией и капитализацией. VAD: prob 1.000 на голосе, 139мкс/чанк.
- Очередь вставки: 4 потока × 100 фрагментов — порядок FIFO без потерь; 50КБ кириллицы — ок (dry-режим).
- `npx tsc --noEmit` exit 0; `npm run build` exit 0.
- **Инсталлятор 0.2.0**: ISCC exit 0 (287 МБ), тихая установка ПОВЕРХ 0.1.0 — реестр 0.2.0, данные целы.
  Грабля: осиротевший /VERYSILENT-процесс держал Output-файл → ISCC Error 32 (убит по PID).
  НЕ запускать инсталлятор через bash `&` — процесс обрывается; только Start-Process -Wait + поллинг реестра.
- **Верификация (независимые прогоны со свежим контекстом)**: задачи 1–5 = PASS/PASS_WITH_NOTES,
  адверсариальное ревью ядра: BLOCKER/MAJOR нет. Доказана бит-точность окна и мел-банка против
  эталона (0 расхождений на 320+10304 коэффициентах) и эквивалентность RNNT-декода.
- **Боевое подтверждение**: пользователь диктовал новой сборкой в Telegram (14:30) — total 190–375 мс
  с учётом вставки, всё вставлено. В окне AI-чата (категория "ai") диктовки ушли в синхронный Ollama-рефайн.
- **Фиксы по итогам верификации**: (1) таймауты рефайна 90/90/30с → 10с (ollama/gemini/rewrite) — синхронный
  рефайн дольше 10с обесценивает диктовку; (2) inject-тесты сериализованы static SERIAL (дефолтный
  параллельный cargo test флакал на общих DRY_LOG/PENDING — теперь дважды зелёный); (3) утечка
  utterance_*.wav при ошибке вставки в never-ветке. Не чинил (minor, осознанно): дубль level-потока при
  рестарте <33мс (фронт фильтрует по seq), потеря live-сегмента при ошибке transcribe (финал восстанавливает),
  мёртвый legacy-CSS .pill, clean_spaces строже эталона на ≥2 ведущих пробелах (для вставки лучше).
- Финальная сборка 14:53 установлена, приложение запущено (warmup: vad×2 166 мс, gigaam 907 мс, прогрев 27 мс).
