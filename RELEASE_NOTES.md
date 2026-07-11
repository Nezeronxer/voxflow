# VoxFlow v2.0.2 — мгновенные клавиши и быстрый финал на macOS

Patch-релиз заменяет локальные v1.0.8, v2.0.0 и v2.0.1. Он устраняет задержанные
нажатия и отпускания Right Option, ускоряет финальное распознавание, делает
первую вставку надёжной и безопасно удаляет только строго более старые копии
VoxFlow, не затрагивая модели, историю и настройки пользователя.

## Критическое исправление macOS

- Причина найдена в упаковке v2.0.0: `--no-sign` оставлял только встроенную
  linker-generated ad-hoc подпись главного Mach-O. Она не связывала
  `Info.plist` и resources, поэтому `codesign --verify --deep --strict` падал с
  `code has no resources but signature indicates they must be present`.
- Whisper CLI/server теперь подписываются inside-out с hardened runtime, после
  чего Tauri полностью ad-hoc подписывает внешний `.app` до создания DMG.
- Release gate требует связанный `Info.plist`, sealed CodeResources, отсутствие
  `linker-signed` у sidecar-файлов и успешные strict/deep проверки для app, DMG
  и распакованного ZIP.
- `syspolicy_check` отдельно проверяется на структурный `Codesign Error`, чтобы
  дефект «приложение повреждено» больше не мог пройти CI.
- В hardened-runtime подпись обязательно встраиваются права Audio Input и
  Apple Events. Первый запуск аудиозахвата достигает системного запроса
  микрофона даже пока модель ещё скачивается; onboarding сначала проводит через
  Input Monitoring для Right Option, затем через Accessibility для вставки.

## Диктовка и интерфейс VoxFlow 2.0

- Индикатор записи снова компактный: орб зафиксирован на 13 px, а idle Flow Bar
  уменьшен с 350×54 до 244×38 логических px. Остальные состояния и размеры
  Tauri-окна синхронизированы и защищены тестом от позднего CSS-каскада.
- Перетаскивание Flow Bar на macOS больше не зависит от глобального
  CoreGraphics-поллера и Input Monitoring: pointer-drag одинаково работает на
  macOS и Windows, после отпускания сохраняет позицию и не конфликтует с кликом.
- Двойной быстрый Right Option включает режим без удержания по умолчанию.
  Первый release всегда немедленно отправляет Stop; второй быстрый press
  начинает новую запись и защёлкивает её — фонового 300-мс ожидания больше нет.
- Повторные `flagsChanged`, auto-repeat и orphan key-up на macOS сверяются с
  физическим HID-состоянием и больше не создают запаздывающие Start/Stop.
- При `stream_mode=never` не запускаются GigaAM/Parakeet/Whisper preview-петли:
  финальный ASR не ждёт их завершения и не конкурирует с ними за CPU/модель.
- Warmup начинается на 1,2 с раньше; readiness и сетевые таймауты адаптированы
  под длину записи. CoreAudio начинает писать до медленного определения окна.
- Без Accessibility диктовка не стартует и не теряет первую вставку: VoxFlow
  открывает нужный раздел настроек и просит повторить после выдачи доступа.
- Cmd+V получает 8 мс на публикацию pasteboard; отложенное восстановление
  clipboard не затирает новое копирование пользователя.
- При запуске удаляются только проверенные `.app` со строго меньшей версией в
  `/Applications` и `~/Applications`; временные старые установщики тоже очищаются.
- Right Option (`AltRight`) остаётся стандартной клавишей macOS; Windows по
  умолчанию использует Right Control (`ControlRight`).
- Запуск live Whisper preview больше не блокирует Stop; macOS AppleScript
  checks ограничены таймаутами, а контекст активного окна переиспользуется.
- Windows получает CUDA→CPU fallback, ограниченные startup/CLI таймауты,
  восстановление целевого HWND и устойчивое переподключение hotkey listener.
- Чистая установка больше не включает Ollama/Qwen rewrite неявно. Старый
  нетронутый default 2.0.0 однократно переводится в `AI backend: off`, поэтому
  короткая фраза не запускает синхронный локальный LLM до 10 секунд с высокой
  загрузкой CPU. Явно настроенные backend, модель, URL или smart-prompt
  сохраняются.
- На Windows режим live-вставки `never` больше не запускает фоновый CUDA
  Whisper partial только ради preview: прогретый server остаётся свободен для
  финала и не проходит цикл wait → kill → холодная загрузка после короткой фразы.
- Updater проверяет ОС, tag/version, точное имя, размер и доступный SHA-256.

## Проверка платформ

- Windows CI: чистая тихая установка, runtime/версия, первый запуск и база,
  single-instance, закрытие окна в трей, повторный запуск, удаление приложения
  и сохранность пользовательской базы.
- macOS CI: ARM64/macOS 11 metadata, полный codesign seal, DMG mount, ZIP
  extraction, runtime architecture/dependencies и SHA-256.
- Общие frontend tests/build/audit, Rust fmt/clippy/unit tests и version gate.

## Артефакты

- `VoxFlow-Setup-2.0.2.exe`
- `SHA256SUMS-windows.txt`
- `VoxFlow-macOS-2.0.2-arm64-adhoc.dmg`
- `VoxFlow-macOS-2.0.2-arm64-adhoc.app.zip`
- `SHA256SUMS.txt`
- `release-manifest.txt`

## Ограничения доверия ОС

В GitHub Actions нет Apple Developer ID/notarization credentials и Windows
Authenticode-сертификата. Поэтому macOS bundle теперь структурно корректен и
полностью ad-hoc подписан, но не идентифицирован Apple и не нотариализован. При
первом запуске macOS может потребовать открыть «Системные настройки →
Конфиденциальность и безопасность» и нажать «Всё равно открыть». Windows
SmartScreen также может показать предупреждение для setup.

Ad-hoc подпись устраняет именно ложное сообщение о повреждении. Бесшовный
двойной клик без предупреждения возможен только после Developer ID signing,
notarization и stapling. Intel Mac не входит в подтверждённую матрицу; macOS
релиз предназначен для Apple Silicon и требует macOS 11 или новее.
