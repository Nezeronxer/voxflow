# Заметки исполнителям: память моделей, выгрузка, экран «Модели», пакет Arch

Версия сейчас 2.3.5. Последнее решение в DECISIONS.md - D-052, следующий номер D-053.
Пути ниже относительно `voxflow/src-tauri/src/` (Rust) и `voxflow/src/` (фронт), если не сказано иначе.
Проверено чтением кода на маке. Что не проверялось - помечено «не проверено».

## 1. Создание `ort::Session`

Версия: `ort = "=2.0.0-rc.12"` (Cargo.toml:45), features не заданы, то есть по умолчанию. В Cargo.lock:2998-3010 у ort зависимости
ndarray, tracing, ureq, ort-sys - значит включены download-binaries (статические prebuilt, комментарий Cargo.toml:41-44) и ndarray.
Execution provider только CPU.

Места создания (везде одинаково `Session::builder().with_intra_threads(N).commit_from_file(...)`; никаких
других опций нет: ни `with_memory_pattern`, ни `with_optimization_level`, ни настроек arena, ни `with_inter_threads`):

| Где | Что | Потоки |
|---|---|---|
| gigaam.rs:132-137 (замыкание `mk`), вызовы gigaam.rs:141-143 | encoder / decoder / joint = 3 сессии на экземпляр `GigaAm` | encoder `threads.max(1)`, decoder 1, joint 1 |
| parakeet.rs:89-94 (`mk`), вызовы parakeet.rs:98-100 | preprocessor `nemo128.onnx`, encoder `encoder-model.int8.onnx`, `decoder_joint-model.int8.onnx` = 3 сессии | 1-я и 2-я `threads.max(1)`, 3-я 1 |
| vad.rs:28-33 (`SileroVad::load`) | 1 сессия на экземпляр; движок грузит ДВА экземпляра (engine.rs:1037-1038) | 1 |
| final_eval.rs:235-239 | только `#[cfg(test)]`, CTC-замер | threads |

Входы/выходы идут через `ort::value::Tensor` (gigaam.rs:16, 268 `.run(ort::inputs![...])`). Тензоры создаются на каждый вызов.

Итого резидентных сессий при полном наборе: 3 (GigaAM) + 3 (Parakeet) + 2 (VAD) = 8.
Куда добавлять опции: единственные три замыкания/места выше; общей фабрики нет, придётся править три места
(или завести общий хелпер, например в gigaam.rs, и звать из parakeet.rs). API опций arena в rc.12 не проверял -
сверить по docs.rs/ort/2.0.0-rc.12 перед правкой (имена методов в rc менялись).

## 2. Где модели живут резидентно

`EngineCtx` - engine.rs:258-259 (`#[derive(Clone)]`, все поля `Arc<Mutex<..>>`), конструктор значений engine.rs:~360-390.

| Поле | Строка объявления | Тип | Создание значения |
|---|---|---|---|
| `server` | engine.rs:265 | `Arc<Mutex<Option<asr::Server>>>` - дочерний процесс whisper-server | `ensure_server_inner` engine.rs:4849 |
| `gigaam` | engine.rs:318 | `Arc<Mutex<Option<crate::gigaam::GigaAm>>>` | `ensure_gigaam` engine.rs:4743; `upgrade_gigaam_in_background` engine.rs:4785 |
| `parakeet` | engine.rs:322 | `Arc<Mutex<Option<crate::parakeet::Parakeet>>>` | `ensure_parakeet` engine.rs:4809 |
| `vad` | engine.rs:325 | `Option<SileroVad>` - для партиал-петли (несёт стриминговый state) | warmup engine.rs:1037-1041 |
| `vad_final` | engine.rs:329 | `Option<SileroVad>` - финалы, гейт тишины | warmup engine.rs:1042 |
| `partial` | engine.rs:284 | состояние живого превью (`PartialState`, поток), НЕ модель | `maybe_start_partial_loop` engine.rs:1427 |

Значения `None` при конструкции: engine.rs:375-378.

**Превью плашки использует ТЕ ЖЕ модели, отдельной модели нет.** `maybe_start_partial_loop` (engine.rs:1427) передаёт
в `start_local_partial_loop` (engine.rs:2253) `Arc::clone(&ctx.gigaam)` / `ctx.parakeet` (engine.rs:1497-1568).
Проверка готовности без блокировки - `resident_model_ready` (engine.rs:803, через `try_lock`).
Ветки:
- auto + engine gigaam: engine.rs:1465-1503; при наличии Parakeet обёртка `AutoLangStt` (engine.rs:940) держит оба `Arc`.
- macOS + whisper_server + ru/auto + GigaAM есть: engine.rs:1509-1530, превью гонит GigaAM.
- `LocalRoute::GigaAm`: engine.rs:1533-1555; `LocalRoute::Parakeet`: engine.rs:1557-1575.
- иначе whisper-петля (engine.rs:1660-1700, `ensure_server_cancellable` engine.rs:1669) - живой whisper-server.
Трейт, через который крутятся модели: `LocalStt` engine.rs:784. VAD превью: engine.rs:2344, 2375 (`a.vad`).

Все вызовы:
- `ensure_gigaam`: engine.rs:1068 (warmup), 5346, 5395, 5472, 5539.
- `ensure_parakeet`: engine.rs:1083 (warmup), 5377, 5574 (`auto_parakeet_final` engine.rs:5568).
- `ensure_server*`: engine.rs:1142 (warmup), 1669 (превью), 8308 (финал); `restart_whisper_server` engine.rs:4949 (вызов 8181 - таймаут asr_lock).
- `upgrade_gigaam_in_background`: engine.rs:1063 (по событию `model:done` с именем fp32, подписка engine.rs:1054-1065).
- `SileroVad::load` в рабочем коде: только engine.rs:1037-1038.
- Прогрев: `warmup(ctx)` engine.rs:1024, запускается из `EngineCmd::Warmup` (engine.rs:1211, отдельный поток), при старте и из `save_settings` при смене языка (commands.rs:145, 192-194).

**Выгрузка сейчас: нет.** Моделей `*ctx.gigaam.lock() = None` / `parakeet = None` / `vad = None` в рабочем коде не существует
(`rg` по engine.rs: единственные `= None` для моделей - в тесте engine.rs:7597). Единственная «замена» - fp32 поверх int8:
`*ctx.gigaam.lock() = Some(g)` (engine.rs:4796), старая модель просто дропается. Не выгружаются при:
- смене языка/движка/модели (commands.rs:145-215 шлёт `Warmup`, который лишь ДОГРУЖАЕТ нужное, старое остаётся - значит ru -> en оставляет в RAM и GigaAM, и Parakeet);
- удалении модели: `delete_model` (commands.rs:254) зовёт только `models::delete` (models.rs:489), движок не трогает - удалённая с диска модель остаётся в RAM до выхода;
- простое: таймера нет.
whisper-server убивается только: Shutdown (engine.rs:1222, `srv.child.kill()`), смена модели/каталога в `ensure_server_inner` (engine.rs:4877-4880), `restart_whisper_server` (engine.rs:4949). По простою не гасится.

Подсказки для выгрузки по простою:
- Все чтения/записи через `Mutex`; финал и превью берут `ctx.gigaam.lock()` (engine.rs:5348, 5474, 5541) - сброс в `None` под тем же замком безопасен, `ensure_*` перегрузит лениво (холодный старт ~1 с по комментарию engine.rs:4741).
- `resident_model_ready` и превью при `None` просто не запускают живой стрим (engine.rs:1535-1539 пишет «gigaam не готов»), то есть после выгрузки ПЕРВАЯ диктовка получит превью не сразу - надо либо греть в `Start`, либо принять.
- Отметка «последнее использование» ставится удобнее всего в `maybe_start_partial_loop` (engine.rs:1427) и в начале финала (около engine.rs:5330); таймер - отдельный поток рядом с `warmup`. Не выгружать, пока `ctx.recording` или идёт финал (`final_queue`, `cancel_active`).
- VAD (2 МБ) выгружать смысла нет; два экземпляра есть из-за гонки финала и следующей петли (комментарий engine.rs:322-329).
- Образец размера: у GigaAM fp32 885 МБ диска (models.rs:150), int8 217, Parakeet 640 (models.rs:137-170, поле `size_mb`), whisper large-v3-turbo-q5_0 547 МБ.

Нюанс fp32/int8: `gigaam_variant()` (models.rs:460) выбирает fp32, если он есть и не помечен битым, иначе int8 - значит int8 при готовой fp32 фактически «не используется» (кроме отката по `FP32_BROKEN`, `report_fp32_failure` engine.rs:4726).

## 3. Выгрузка Ollama (D-052, коммит ced6408)

DECISIONS.md:1206-1215. Это НЕ таймер VoxFlow, а просто `keep_alive` в запросе: Ollama сама гасит модель. Отвергнут «свой таймер с unload».

Бэкенд:
- settings.rs:175-177: `pub ollama_unload_idle: bool`, `pub ollama_unload_after_min: u32`; значения по умолчанию settings.rs:306-307: `true` и `15`.
- Нормализация: settings.rs:370 `self.ollama_unload_after_min.clamp(1, 1440)` внутри `normalize_user_values` (settings.rs:320).
- Старые настройки: `#[serde(default)]` на всей структуре `Settings` (settings.rs:27), то есть отсутствующие поля берут `Default` (миграции нет). Отдельных `legacy_*` функций для D-052 нет (образец legacy-версий: settings.rs:39, 100).
- ollama.rs:349-366: `static KEEP_ALIVE_MIN: AtomicI64`, `pub fn set_keep_alive(&Settings)`, `fn keep_alive()` -> `"15m"` или `-1`. Применяется в `chat_body` ollama.rs:386 (`"keep_alive": keep_alive()`).
- Вызовы `set_keep_alive`: lib.rs:219 (старт), commands.rs:174 (`save_settings`).
- Явная выгрузка `ollama::unload` ollama.rs:427 (`keep_alive: 0`): на выходе lib.rs:1213 (`request_shutdown`) и при смене модели/бэкенда commands.rs:177-190.
- Тест: ollama.rs:634 `keep_alive_follows_unload_setting`.
Фронт:
- sections/Ai.tsx ~373-404 (блок после поля модели Ollama): `<Field label="Выгружать модель в простое">` с `<Switch checked={settings.ollama_unload_idle}>`, и при включённом - `<Field label="Выгружать через, мин">` с `<input type="number" min=1 max=1440>` (clamp 1-1440, по умолчанию 15 при NaN).
- types.ts:55-57 (интерфейс `Settings`), types.ts:290-291 (`DEFAULT_SETTINGS`).
Для ASR образец по именам/UI подходит (`asr_unload_idle`, `asr_unload_after_min`, галочка + минуты), но ТАЙМЕР придётся писать самим: у ASR нет внешнего демона с keep_alive. Подписка на изменение настроек - `save_settings` (commands.rs ~140-215) как место для применения.

## 4. local_llm.rs / local_ai.rs

- `local_llm.rs` (1211 строк): встроенный llama.cpp - ОТДЕЛЬНЫЙ процесс `llama-server` (`Command::new(&bin)` local_llm.rs:731, `.spawn()` 742), loopback-порт, OpenAI-совместимые запросы. Хэндл: `static SERVER: Mutex<Option<Running>>` local_llm.rs:599.
- Живёт, пока живёт приложение (шапка модуля, строки 24-26). Выгрузки по простою НЕТ. Останавливается: `pub fn stop()` local_llm.rs:879 (выход lib.rs:1210 `request_shutdown`; `save_settings` commands.rs:163-170 при смене модели/gpu_mode/выключении встроенного ИИ), перезапуск в `ensure_server` при смене модели/gpu (local_llm.rs:~700-710).
- Прогрев: `pub fn warmup` local_llm.rs:1023 (поток `voxflow-llm-warmup`), вызывается lib.rs:218 и commands.rs:171.
- Аргументы сервера собираются local_llm.rs:~669-690 (`--host`, `--port`, `-c`, при CPU `-ngl 0`, `--no-webui`, `--jinja`); флагов `--mlock`/`--no-mmap` нет.
- Простоя-таймера нет: если нужна выгрузка встроенного LLM, это отдельная задача; точка - `stop()` + отметка времени последнего запроса.
- `local_ai.rs` (865 строк): только обнаружение внешних движков на портах (`PROBES`) и каталог моделей по ярусам памяти. Памяти не держит.

## 5. Экран «Модели»

Файл: `sections/Models.tsx` (392 строки; НЕ Stt.tsx и не Recognition.tsx - эти про другое).
- Загрузка списка: `refresh()` Models.tsx:~189 -> `listModels()` из api.ts:~240-256 (`invoke<ModelInfo[]>("list_models")`, в браузерном моке api.ts:240-253).
- Тип: types.ts:334-343
  `interface ModelInfo { name; label; size_mb: number; installed: boolean; kind?: string }` (`kind`: "gigaam" | "parakeet" | "whisper").
- Строка модели: `ModelRow` Models.tsx:~120-175. Размер выводится как `fmtSize(model.size_mb)` (Models.tsx:44) - это КАТАЛОЖНАЯ константа (МБ), реального размера на диске нет. Кнопка удаления уже есть: корзина `onDelete(model.name)` (Models.tsx:~158) -> `onDelete` Models.tsx:~273 -> `deleteModel(name)` (api.ts:~289, `invoke("delete_model", { name })`), потом `refresh()`. Подтверждения нет.
- «Активна» сейчас только для whisper: `selected={m.kind === "whisper" && settings.model === m.name}` (Models.tsx:~291); `<span className="badge accent">Активна</span>` в `ModelRow`. Для GigaAM/Parakeet пометки «используется» нет. Разделение списка: `inUse` (стоит на диске или качается) и «Другие модели» (`<details className="model-more">`) - слово inUse там означает «установлена», а не «используется».
- Бэкенд: `list_models` commands.rs:231 -> `models::list()` models.rs:224. `ModelInfo` models.rs:18-26 (`name, label, size_mb: u32, installed, kind`). Каталог: `DIR_MODELS` models.rs:137 (gigaam-v3 217 МБ, gigaam-v3-fp32 885 МБ, parakeet-v3 640 МБ) и `CATALOG` (whisper `Entry`, поиск `catalog_entry` models.rs:254).
- Удаление: `delete_model` commands.rs:254 -> `models::delete` models.rs:489: для каталожной ONNX удаляет каталог целиком (`remove_dir_all`); для fp32 ставит метку `gigaam-fp32.skip` (`fp32_opt_out_path` models.rs:476) и сбрасывает `FP32_BROKEN`; для whisper `remove_artifact` + `.part`. Тест на path traversal models.rs:1280.
- Размеров на диске нигде не считают: функции `dir_size`/`du` НЕТ (проверено grep). Для «реального размера» нужно новое поле (например `disk_mb`/`disk_bytes: u64`) в `ModelInfo` (Rust + types.ts + мок в api.ts:240-253) и обход каталога/`metadata().len()`; у whisper это один файл `paths::model_path(name)` (paths.rs:217), у ONNX - каталоги `paths::gigaam_dir()` (paths.rs:236), `models::gigaam_fp32_dir()`, `paths::parakeet_dir()` (paths.rs:242). Лишние файлы в каталоге моделей есть (`*.sha256`, `.part`, `models/llm` 0 Б) - учитывать при подсчёте.
- Что считать «используется» (логика маршрута, engine.rs:810-845):
  - `local_route_with_parakeet` (engine.rs:831): если `settings.engine != "gigaam"` -> Whisper. Иначе язык `ru` -> GigaAM; `en` и Parakeet готов -> Parakeet; прочее -> Whisper.
  - Режим `language == "auto"` и `engine == "gigaam"`: финал идёт GigaAM -> Parakeet -> Whisper запасной (D-048, engine.rs:5454-5580), превью GigaAM (+Parakeet) - то есть при auto используются и GigaAM, и Parakeet, и модель `settings.model` как запасная.
  - Какой вариант GigaAM: `models::gigaam_variant()` (models.rs:460) - fp32 если есть, иначе int8.
  - Whisper-модель: `settings.model` (settings.rs:46), `engine` (settings.rs:48), `language` (settings.rs:44), `stt_provider` (облако; на экране уже учтено в подписи Models.tsx).
  - Рекомендация: считать функцию `model_in_use(name, settings)` на Rust (чтобы ложилась рядом с `local_route_with_parakeet`) и отдавать флаг `in_use: bool` из `list_models`, а не дублировать правила на фронте. Тестов на неё придётся добавить.
- Автозагрузка/автовыбор моделей: `ensure_default_models`, `plan_local_models` (models.rs ~500-560, структура `InstalledModels`); не трогать без решения (D-042).

## 6. updater.rs: Linux

Файл `updater.rs`. Дистрибутив сейчас НЕ определяется вообще; есть только платформа.
- `UpdateTarget` updater.rs:30-36 (`LinuxX64`), `asset_pattern` updater.rs:40: `LinuxX64 => ("VoxFlow-", "-linux-x64.AppImage")` - один префикс/суффикс на цель.
- `current_update_target` updater.rs:61 (`cfg(target_os="linux", target_arch="x86_64")`).
- `target_auto_install` updater.rs:872-877: для Linux `std::env::var_os("APPIMAGE").is_some()`; для deb/rpm/pkg кнопка ведёт на страницу релиза (`auto_install = false`; фронт: App.tsx:172, 285, Control.tsx:117, 376).
- `update_info_from_release_for` updater.rs:880, `find_installer_asset_for` updater.rs:960, `asset_name_matches_version` updater.rs:984 (Linux updater.rs:1005: `name == "VoxFlow-{version}-linux-x64.AppImage"`), `validate_asset_name_for` updater.rs:1022 (через `asset_pattern`), `is_installer_asset_name_for` updater.rs:1092, `installer_download_path` updater.rs:1109 (расширение берётся последним `.`: для `.pkg.tar.zst` получится `...pkg.tar` + `.zst`, итоговое имя в tmp будет с `_`-санитизацией и `unique_tmp_path` paths.rs:109 - проверить, что `pacman -U` примет файл; не проверено).
- Установка: `apply_downloaded_update` для Linux updater.rs:492-532 (`cfg(target_os="linux")`): берёт `APPIMAGE`, копирует новый файл рядом, `chmod 755`, `rename` поверх, потом `sh -c 'sleep 3; exec "$0"'` для перезапуска. Если `APPIMAGE` не задан - ошибка «автообновление работает только для AppImage» (строка 502). `pkexec`/`dpkg`/`pacman` в updater.rs НЕ используются.
- Скачивание/проверка: `download_and_launch` updater.rs:151 (валидирует URL `RELEASE_DOWNLOAD_PREFIX` и имя, скачивает curl, `verify_downloaded_asset` updater.rs:1056 по размеру и SHA-256 из поля `digest` GitHub-ассета).
- Готовое определение дистрибутива есть в другом месте: `linux_env.rs:57-83` `package_manager_from(os_release)` (читает ID/ID_LIKE: arch/manjaro/endeavouros -> `PackageManager::Pacman`), обёртка `install_hint` linux_env.rs:136 читает `/etc/os-release`. `PackageManager` - приватный enum (linux_env.rs:49), его надо сделать `pub(crate)` или вынести общий хелпер. Модуль подключён lib.rs:17 (`mod linux_env`, код под cfg Linux).
- Тесты рядом: `selects_linux_appimage_asset_and_ignores_packages` updater.rs:1284 (подсовывает `.deb` и `.AppImage`, ждёт AppImage - его надо поменять/расширить под выбор `.pkg.tar.zst` на Arch); `selects_platform_specific_asset_from_release_json` updater.rs:1306; `every_supported_platform_installs_automatically` updater.rs:1401 (Linux намеренно не в списке). Тесты `cfg(target_os)` Linux-кода запускает только CI.
- Имена ассетов релиза Linux сейчас: `VoxFlow-X.Y.Z-linux-x64.AppImage`, `.deb`, `.rpm`, `SHA256SUMS-linux.txt`. Для Arch предлагаю `VoxFlow-X.Y.Z-linux-x64.pkg.tar.zst` (под префикс `VoxFlow-` и версию в имени, как у остальных).
- Установка пакета с правами: нужен root. Варианты: `pkexec pacman -U --noconfirm <файл>` (нужен polkit-агент; на Hyprland его может не быть - тогда фолбэк «открыть релиз» + показать команду `sudo pacman -U <путь>`), перезапуск после. Правило CLAUDE.md: «пакет, которому на Arch нужен sudo, - в `needs-sudo.linux.txt`» относится к setup машины, не к приложению. Решение по способу (pkexec vs подсказка команды) - за пользователем/исполнителем, неочевидное: поэтому в D-053 вынести.
- Как узнать, что приложение поставлено пакетом Arch: `pacman -Qo <current_exe>` (код 0) либо переменная `APPIMAGE` пустая + дистрибутив Arch. Не проверено на живой системе.

## 7. Workflow-файлы

`.github/workflows/release.yml` (252 строки) оркестратор: `build-windows` (16), `build-macos` (22), `build-linux` (42-46, `uses: ./.github/workflows/release-linux.yml`), `publish` (48, `needs: [build-windows, build-macos, build-linux]`).
- publish скачивает артефакт Linux: release.yml:71-75 (`name: VoxFlow-Linux-x64`, `path: release/linux`).
- Поиск файлов: release.yml:92-95 `find release/linux -name 'VoxFlow-*-linux-x64.AppImage' | .deb | .rpm | SHA256SUMS-linux.txt`; проверка «ровно по одному» release.yml:106-109; список `files=(...)` release.yml:~112-123 (`appimages[0] debs[0] rpms[0] linux_checksums[0]`); проверка дубликатов имён ~125-135; `sha256sum --check` Linux ~146-148; создание/дозагрузка релиза: `gh release create` release.yml:183, `gh release upload` 208/210, публикация `gh release edit --draft=false` 245.
- Для нового пакета в release.yml надо добавить: `mapfile ... pkgs` рядом со строками 92-95, расширить условие на 106, добавить `"${pkgs[0]}"` в `files`.

`.github/workflows/release-linux.yml` (165 строк), job `build-linux` на `ubuntu-24.04` (строка 18; glibc 2.38+ для onnxruntime):
- 47 `Verify release version consistency` (check_versions.py), 55 зависимости apt, 76 `npm audit`, 82 `bash script/build_linux_runtime.sh`, 85-95 fmt/clippy/test, 97-99 `npx tauri build --bundles deb,rpm,appimage`.
- 101 `Collect and verify packages`: копирует и переименовывает в `release/linux/` строки 119-122: `VoxFlow-${version}-linux-x64.{AppImage,deb,rpm}` (функция `pick` ровно одного непустого файла); проверки содержимого 127-154 (`check` для deb: `whisper-linux-x64/whisper-cli`, `vad/silero_vad.onnx`, Depends `libwebkit2gtk-4.1-0`, `.desktop` Categories; rpm: Requires `libwebkit2gtk-4.1`).
- 155: `(cd "$out" && sha256sum VoxFlow-* > SHA256SUMS-linux.txt && sha256sum --check SHA256SUMS-linux.txt)` - новый файл `VoxFlow-*` попадёт в суммы автоматически.
- 157-165 `Upload Linux artifact` (`release/linux/VoxFlow-*` и `SHA256SUMS-linux.txt`) - глоб подхватит и `.pkg.tar.zst`.
- Tauri 2 не умеет цель `pacman`: `tauri.linux.conf.json` `bundle.targets` = `["deb","rpm","appimage"]` (строка 5), зависимости deb/rpm в `bundle.linux` (строки 19-41). Макеты зависимостей Arch: `webkit2gtk-4.1`, `gtk3`, `gtk-layer-shell`, `alsa-lib`, `libayatana-appindicator`. Готовых PKGBUILD в репо нет (`find` ничего не дал).
- Способы собрать `.pkg.tar.zst` (решение за исполнителем, не проверено): (a) шаг после `tauri build`: распаковать готовый `.deb` (`dpkg-deb -x`) в `pkgdir`, написать `.PKGINFO` (+ `.MTREE` не обязателен для `pacman -U`), упаковать `bsdtar --zstd`/`tar --zstd`; (b) контейнер `archlinux` с `makepkg` и PKGBUILD, берущим `.deb`/bundle (`makepkg` запрещён от root - нужен пользователь). Бинарь собран на glibc 2.39: на Arch (новее) запускается. Раскладка Tauri deb (`/usr/bin/voxflow`, ресурсы в `/usr/lib/VoxFlow/...`) не проверена, смотреть `dpkg-deb -c` в CI.
- Проверка пакета в CI по образцу `check`: `bsdtar -tf *.pkg.tar.zst | grep whisper-cli`, `silero_vad.onnx`, `.PKGINFO` с `depend =`.

## 8. check_versions.py

`script/check_versions.py` (запуск: `python3 script/check_versions.py [--tag vX.Y.Z]`; сейчас печатает `Version consistency: 2.3.5`).
Сверяет (строки 40-85, `collect_versions`):
`voxflow/package.json`, `voxflow/package-lock.json` (верх и `packages[""]`), `voxflow/src-tauri/Cargo.toml`, `voxflow/src-tauri/tauri.conf.json`,
`installer/VoxFlow.iss` (`AppVersion` и `VersionInfoVersion = X.Y.Z.0`), `voxflow/src/sections/SettingsHub.tsx` (`VoxFlow X.Y.Z`),
`voxflow/src/App.tsx` (бренд `major.minor` + patch из package.json), `voxflow/README.md` (`check_versions.py --tag vX.Y.Z`), `RELEASE_NOTES.md` (заголовок `# VoxFlow vX.Y.Z`). Опционально тег.
Cargo.lock отдельно не проверяется. Версия меняется одним шагом релиза (правило CLAUDE.md), не вперемешку с фичами; Cargo.toml:3 и tauri.conf.json:4 сейчас 2.3.5.

## 9. Тесты и замер точности

- `VOXFLOW_TEST_DATA_DIR`: paths.rs:89 (`data_dir_path` под `#[cfg(test)]`); без переменной данные идут в `temp/voxflow-test-data-<pid>`. Для настоящих моделей: `VOXFLOW_TEST_DATA_DIR="$HOME/Library/Application Support/VoxFlow"` (на этой машине там `models/` с gigaam-fp32 851 МБ, parakeet 640 МБ, три whisper-файла 547 МБ / 834 МБ / 2,9 ГБ, int8 `models/gigaam` пуст, и `dataset/`).
- Замер точности GigaAM по датасету: `final_eval.rs:294 final_accuracy_eval` (`#[ignore]`). Команда:
  `cd voxflow/src-tauri && VOXFLOW_TEST_DATA_DIR="$HOME/Library/Application Support/VoxFlow" VOXFLOW_EVAL_MODEL=int8|fp32 VOXFLOW_EVAL_LIST=<файл со списком wav> VOXFLOW_EVAL_REF=<каталог .txt псевдоэталонов Whisper> cargo test --locked --lib final_accuracy_eval -- --ignored --nocapture`
  (+ `VOXFLOW_EVAL_DIR`, `VOXFLOW_EVAL_ONLY`). Нужны список и каталог эталонов - в репо их нет; про них см. D-036 в DECISIONS.md. Измеряет WER (`word_errors` final_eval.rs:197) по набору вариантов VAD, а не «до/после памяти» - для памяти не годится как есть.
- Для проверки, что после выгрузки/снижения памяти качество не упало: `gigaam.rs:484 gigaam_fp32_loads_from_own_dir_and_transcribes_10s_fast` (грузит fp32 и распознаёт 10-секундную запись < 1,5 с), `engine.rs:6410 preview_vs_final_worst` (расхождение превью/финал, `VOXFLOW_EVAL_LIMIT`, по умолчанию 80), `engine.rs:6466 preview_latency_on_dataset` (задержка превью). Все `#[ignore]`, запуск: `VOXFLOW_TEST_DATA_DIR=... cargo test --locked --lib <имя> -- --ignored --nocapture`.
- Замера ПАМЯТИ в тестах нет. Для до/после ставить вручную: `ps -o rss= -p $(pgrep -f voxflow)` (на маке; whisper-server отдельный процесс - его RSS отдельно) после warmup и после прогона диктовок; либо `/usr/bin/time -l`. Предлагаю зафиксировать базу ДО правок.
- Обычные тесты: `cd voxflow/src-tauri && cargo test --locked --lib` (по CLAUDE.md 422 ok, 9 ignored), `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --check`; фронт `cd voxflow && npx tsc --noEmit && node --test tests/*.test.mjs && npx vite build`.
- Готовые тесты, которые заденут правки: engine.rs:7590-7600 `resident_preview_probe_never_waits_for_model_initialization` (проверяет `resident_model_ready`; выгрузка по простою не должна ломать его контракт), updater.rs:1284 (п.6), ollama.rs:634.

## Что учесть при работе (сводка рисков)

1. Смена языка/модели не выгружает старую (п.2) - самый дешёвый выигрыш по памяти: при ru -> en в RAM оба.
2. `delete_model` не касается RAM (п.2): при удалении активной модели движок продолжает её держать - нужна выгрузка вместе с удалением и запрет удалять модель во время диктовки.
3. Удаление fp32 пишет метку `gigaam-fp32.skip` (models.rs:476) - кнопка в UI не должна молча это менять; удаление fp32 при готовой int8 безопасно, обратного не делать без явного предупреждения (int8 запасной, D-038/D-040).
4. Не менять `updater.rs`/каталог моделей без решения (CLAUDE.md проекта): оформить D-053 (выгрузка ASR), D-054 (пакет Arch), если исполнители меняют поведение.
5. Linux-код проверяет только CI (`cfg(target_os = "linux")`): clippy на dead_code, зелёный Linux-job до тега.
6. Версию не трогать в фичевых коммитах.
