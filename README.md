# VoxFlow — бесплатный диктовщик для Windows

VoxFlow превращает речь в текст и вставляет результат в активное приложение:
чат, браузер, документ, IDE или окно Codex. По умолчанию он работает локально,
а облачные провайдеры включаются только в формате BYOK — со своим ключом.

![status](https://img.shields.io/badge/platform-Windows-blue) ![version](https://img.shields.io/badge/version-1.0.3-green) ![license](https://img.shields.io/badge/license-proprietary-red) ![privacy](https://img.shields.io/badge/privacy-local%20or%20BYOK-orange)

VoxFlow — proprietary product by Nezeronxer. The source code is visible for
review only; copying, redistribution, rebranding, resale, forks, clones and
derivative products are prohibited without written permission. See [LICENSE](LICENSE).

## Что умеет v1.0.3

- Hold-to-talk и toggle-диктовка через глобальную горячую клавишу.
- Автообновление через GitHub Releases: VoxFlow сам проверяет новую версию,
  скачивает `VoxFlow-Setup-*.exe` и запускает установщик после подтверждения.
- Автовосстановление звука после диктовки через режим auto-mute.
- Wispr-style профили приложений: Telegram/Discord/WhatsApp, Gmail/Outlook,
  Codex/ChatGPT/Claude, VS Code/Cursor, Word/Docs.
- Категории стиля: чат, письма, промпты, код, документы, нейтральный и дословный режим.
- Scratchpad и transforms для локальной проверки текста перед вставкой.
- Current-app detector, test insert sandbox и API health center.
- Мультиязычное локальное STT по умолчанию, cloud STT и rewrite только при настройке BYOK.
- Личный словарь, сниппеты, исправления, история и статистика.

## Установка Windows

Локальный release installer собирается сюда:

```powershell
installer\Output\VoxFlow-Setup-1.0.3.exe
```

Установка идёт в профиль текущего пользователя:

```powershell
%LOCALAPPDATA%\VoxFlow\voxflow.exe
```

Администраторские права не нужны. Данные пользователя, модели, база SQLite и
логи остаются в `%LOCALAPPDATA%\VoxFlow`.

## Как пользоваться

1. Поставьте курсор в нужное поле ввода.
2. Зажмите правый `Ctrl` или нажмите выбранную toggle-клавишу.
3. Скажите фразу.
4. VoxFlow распознает речь, применит профиль активного приложения и вставит текст.

Для проверки без внешних приложений используйте встроенные проверки на вкладках
**Главная**, **Приложения** и **ИИ**: test insert sandbox, current-app detector
и health check не отправляют текст в Telegram, Codex или браузер.

Если диктовка распознаётся неправильно, сначала смените язык или модель на
вкладке **Модель**: для русского лучше выбрать **Русский** и GigaAM, для
смешанной речи и других языков — **Все языки (авто)** и Whisper Server. Если
локальное распознавание нестабильно именно на вашем голосе, микрофоне или языке,
включите cloud STT на вкладке **Облако** и добавьте свой API-ключ. Онлайн
зависит от сети, лимитов и доступности провайдера, поэтому выбор cloud/API
остаётся за пользователем.

## Вкладки

| Вкладка | Назначение |
|---|---|
| Главная | статус, старт/стоп диктовки, статистика, последняя диктовка |
| Модель | локальные модели и движок распознавания |
| Распознавание | пунктуация, слова-паразиты, tone, prompt и способ вставки |
| Управление | устройство ввода, хоткей, режим, тема, звуки, auto-mute, автозапуск |
| Словарь | термины и замены |
| Сниппеты | триггеры и шаблоны |
| Исправления | устойчивые автозамены ошибок распознавания |
| Приложения | app-specific профили и current-app detector |
| ИИ | rewrite backend: off, Ollama, Gemini или OpenAI-compatible |
| Облако | cloud STT, fallback и BYOK-провайдеры |
| История | последние диктовки |

## Приватность

- Локальный режим не отправляет аудио и текст наружу.
- BYOK-режим использует только ключи, которые пользователь сам добавил в UI или env.
- Fake/env key QA не требует реальных секретов.
- Ключи должны быть замаскированы в UI и не должны попадать в `debug.log`.
- Автозапуск включается только через настройку пользователя.

## Лицензия

Copyright (c) 2026 Nezeronxer. All rights reserved.

VoxFlow не является open-source проектом. Код открыт только для просмотра и
проверки. Нельзя копировать, перепродавать, переименовывать, публиковать форки,
клоны или производные продукты без письменного разрешения Nezeronxer.

## Сборка из исходников

Требуется Windows, Rust stable с MSVC toolchain, Node.js 22+ и Inno Setup 6.

```powershell
cd voxflow
npm install
npm run build
cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri\Cargo.toml --lib
npm run tauri -- build --no-bundle

cd ..
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" ".\installer\VoxFlow.iss"
```

Важно: для Inno installer используйте production exe после
`npm run tauri -- build --no-bundle`. Не собирайте финальный installer поверх
plain `cargo build --release`, иначе WebView может искать dev server `localhost:1420`.

## Локальный QA перед публикацией

Минимальный gate для v1.0.3:

```powershell
cd "C:\Моя папка\wispr flow\voxflow"
npm run build
cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri\Cargo.toml --lib
npm run tauri -- build --no-bundle

cd ..
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" ".\installer\VoxFlow.iss"
```

После установки проверить запуск `%LOCALAPPDATA%\VoxFlow\voxflow.exe`, вкладки,
app-profile CRUD, fake/env API-key сценарии, отсутствие ключей в логах, правый
клик без WebView menu и start/stop диктовки в безопасной песочнице.

## Лицензии

- Tauri, React, Rust dependencies — по лицензиям upstream-пакетов.
- Локальные ASR runtime/model assets поставляются отдельно в рамках текущей сборки.
