# ASR/context multilingual upgrade notes

Цель этой ветки — приблизить поведение VoxFlow к «контекстному» диктовщику: меньше случайных языковых отсечек, лучше mixed speech и готовая точка входа для ASR prompt-biasing.

## Что уже изменено

### 1. Multilingual aliases для cloud STT

`Settings.language` теперь в cloud STT трактуется мягче:

- `auto`, `all`, `any`, `multi`, `multilingual`, `*` → автоопределение языка;
- конкретные языки (`ru`, `en`, `es`, `de`, `fr`, ...) отправляются как явный language hint.

Для OpenAI-compatible STT автоязык означает: поле `language` вообще не отправляется. Это важно для mixed speech: модель не зажимается в один язык.

Для Deepgram автоязык превращается в `language=multi`.

### 2. Prompt-capable API для OpenAI-compatible STT

Добавлен `cloud_stt::transcribe_with_prompt(s, wav, prompt)`.

Текущий `cloud_stt::transcribe(s, wav)` оставлен как совместимый wrapper без prompt, поэтому существующие вызовы не ломаются.

Prompt отправляется только в OpenAI-compatible путь через `--form-string prompt=...`, чтобы:

- не светить ключи в argv;
- не дать curl интерпретировать prompt как `@file`;
- безопасно передавать термины вроде `VoxFlow`, `Wispr Flow`, `Aqua Voice`, имена проектов, названия приложений.

Prompt автоматически схлопывает пробелы и режется до 1200 символов.

### 3. Unit tests

Добавлены тесты на:

- auto/all/multi aliases;
- Deepgram `multi` mapping;
- нормализацию и ограничение ASR prompt.

## Следующий безопасный шаг для engine.rs

Большой файл `engine.rs` уже содержит память последних диктовок и app-context, но для Aqua Voice-like качества нужно использовать это раньше — до ASR, а не только на rewrite/postprocess.

Рекомендуемый патч:

1. Перед финальным `local/cloud ASR` получить:
   - текущий active app context;
   - хвост последних диктовок для этого окна;
   - личный словарь;
   - snippets/corrections;
   - app category/tone.
2. Собрать короткий ASR prompt:

```text
Context: previous text tail ...
Terms: VoxFlow, Wispr Flow, Vite, Tauri, Rust, whisper.cpp ...
User may mix Russian, English and other languages. Preserve language switches.
```

3. Передать prompt:
   - в `cloud_stt::transcribe_with_prompt` для OpenAI-compatible STT;
   - в `asr::transcribe_server(..., Some(prompt))` для whisper-server;
   - в `AsrParams.initial_prompt` для whisper-cli.
4. Не передавать rewrite-инструкции как ASR prompt. ASR prompt должен быть только biasing/context, иначе модель начнёт переписывать текст вместо распознавания.

## QA matrix

Минимальные ручные проверки на Windows:

| Scenario | Settings | Expected |
|---|---|---|
| Russian | `language=auto`, local GigaAM or cloud | Русский не превращается в короткую латиницу |
| English | `language=auto`, cloud or whisper | Английский не режется русским gate |
| Mixed ru/en | `language=all` or `auto` | Переключения языка сохраняются |
| Spanish/German/French | `language=all` or exact code | Текст не отклоняется из-за ru/en-only assumptions |
| App terms | prompt contains project terms | `VoxFlow`, `Tauri`, `whisper.cpp`, `Vite` распознаются стабильнее |
| No speech | any language | Пустота не вставляется |

## Build gate

```powershell
cd voxflow
npm run build
cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri\Cargo.toml --lib
```
