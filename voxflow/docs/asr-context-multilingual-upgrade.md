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
- сборку engine-level ASR prompt из app context, хвоста прошлой диктовки, словаря, snippets и corrections.

### 4. Engine-level ASR prompt для финального cloud STT

`engine.rs` теперь собирает короткий ASR prompt перед финальным cloud STT проходом и передаёт его в:

```rust
cloud_stt::transcribe_with_prompt(&s, &wav, asr_prompt.as_deref())
```

Prompt строится только как bias/context для распознавания, без rewrite-инструкций:

- active app берётся как короткий label без заголовка окна;
- хвост прошлой диктовки берётся только для того же целевого поля;
- dictionary, snippet triggers и learned corrections добавляются компактными списками;
- тела snippets не отправляются в prompt;
- если окно уже сменилось до ASR, финал отменяется до сетевого cloud STT вызова.

Deepgram по-прежнему игнорирует текстовый prompt намеренно: для него нужен отдельный `keywords`/`keyterms` biasing.

## Следующий безопасный шаг

Первый engine-level слой уже подключён для финального OpenAI-compatible cloud STT. Дальше можно расширять контекст осторожно, отдельными PR:

Рекомендуемый следующий патч:

1. Добавить provider-specific biasing для Deepgram:
   - `keywords` / `keyterm`;
   - маппинг dictionary/corrections в короткие weighted terms;
   - отдельные тесты URL/query escaping.
2. Осторожно решить, нужен ли prompt для локального whisper path сверх текущего `dict_bias_prompt`.
3. Не трогать live cloud draft loop первым: prompt там увеличит latency/стоимость, а финальный текст уже получает контекст.

Форма ASR prompt должна оставаться примерно такой:

```text
Speech recognition context only...
Previous same-field text tail: ...
Likely names and technical terms: VoxFlow, Wispr Flow, Codex, Tauri, Rust, whisper.cpp ...
Known recognition corrections: ...
```

Не передавать rewrite-инструкции как ASR prompt. ASR prompt должен быть только biasing/context, иначе модель начнёт переписывать текст вместо распознавания.

## QA matrix

Минимальные ручные проверки на Windows:

| Scenario | Settings | Expected |
|---|---|---|
| Russian | `language=auto`, local GigaAM or cloud | Русский не превращается в короткую латиницу |
| English | `language=auto`, cloud or whisper | Английский не режется русским gate |
| Mixed ru/en | `language=all` or `auto` | Переключения языка сохраняются |
| Spanish/German/French | `language=all` or exact code | Текст не отклоняется из-за ru/en-only assumptions |
| App terms | prompt contains project terms | `VoxFlow`, `Tauri`, `whisper.cpp`, `Codex` распознаются стабильнее |
| No speech | any language | Пустота не вставляется |

## Build gate

```powershell
cd voxflow
npm run build
cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri\Cargo.toml --lib
```
