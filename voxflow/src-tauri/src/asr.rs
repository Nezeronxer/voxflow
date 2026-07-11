//! Распознавание речи (ASR) через ВСТРОЕННЫЙ бинарник whisper.cpp.
//!
//! Транскрибируем 16 kHz mono WAV, запуская `whisper-cli.exe` как
//! одноразовый дочерний процесс (one-shot subprocess). Никаких серверов
//! и лишних зависимостей: только `std::process` и `std`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Параметры запуска `whisper-cli`.
///
/// Все пути передаются как `&Path`, чтобы скармливать их в `Command`
/// напрямую через `OsStr` (Unicode-safe), без потерь на lossy-строках.
pub struct AsrParams<'a> {
    /// Каталог с бинарником и соседними DLL (`whisper.dll`, `ggml*.dll`).
    pub whisper_dir: &'a Path,
    /// Путь к модели (`*.bin`).
    pub model_path: &'a Path,
    /// Путь к WAV-файлу (16 kHz mono).
    pub wav_path: &'a Path,
    /// Язык распознавания (например, `"ru"`, `"en"` или `"auto"`).
    pub language: &'a str,
    /// Количество потоков.
    pub threads: u32,
    /// Необязательная подсказка-затравка (`--prompt`).
    pub initial_prompt: Option<&'a str>,
}

/// Транскрибирует WAV, запуская встроенный `whisper-cli`.
///
/// Возвращает очищенный текст распознавания (строки склеены пробелом).
pub fn transcribe_cli(p: &AsrParams) -> anyhow::Result<String> {
    transcribe_cli_inner(p, None)
}

/// Same CLI path with a hard deadline. Runtime fallback uses it so a wedged
/// CUDA process cannot prevent trying the bundled CPU runtime.
pub fn transcribe_cli_with_timeout(p: &AsrParams, timeout: Duration) -> anyhow::Result<String> {
    transcribe_cli_inner(p, Some(timeout))
}

fn transcribe_cli_inner(p: &AsrParams, timeout: Option<Duration>) -> anyhow::Result<String> {
    // Имя бинарника зависит от платформы; соседние DLL подхватываются
    // за счёт `current_dir(whisper_dir)`.
    let exe = p.whisper_dir.join(if cfg!(windows) {
        "whisper-cli.exe"
    } else {
        "whisper-cli"
    });

    let mut cmd = Command::new(&exe);
    cmd.current_dir(p.whisper_dir);

    // Аргументы передаём как OsStr (пути) — без lossy-конверсий.
    cmd.arg("-m").arg(p.model_path);
    cmd.arg("-l").arg(p.language);
    cmd.arg("-nt"); // без таймстампов
    cmd.arg("-t").arg(p.threads.to_string());

    if let Some(prompt) = p.initial_prompt {
        cmd.arg("--prompt").arg(prompt);
    }

    cmd.arg(p.wav_path);

    // На Windows прячем консольное окно (CREATE_NO_WINDOW), чтобы не было
    // мигания консоли при запуске дочернего процесса.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    log::debug!("запуск whisper-cli: {:?}", exe);

    let out = if let Some(timeout) = timeout {
        command_output_with_timeout(cmd, timeout)?
    } else {
        cmd.output()?
    };

    // whisper-cli печатает UTF-8 текст транскрипции в stdout.
    let stdout = String::from_utf8_lossy(&out.stdout);

    let mut kept: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let mut line = line.trim();
        if line.is_empty() {
            continue;
        }

        // На всякий случай срезаем ведущий таймстамп вида "[...]" —
        // с флагом -nt их нет, но защищаемся.
        if line.starts_with('[') {
            if let Some(end) = line.find(']') {
                line = line[end + 1..].trim();
            }
        }

        if line.is_empty() {
            continue;
        }

        kept.push(line.to_string());
    }

    let text = kept.join(" ").trim().to_string();

    // Если процесс упал И при этом ничего не распарсилось — это ошибка.
    if !out.status.success() && text.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = {
            let s = stderr.trim_end();
            let chars: Vec<char> = s.chars().collect();
            let start = chars.len().saturating_sub(500);
            chars[start..].iter().collect()
        };
        anyhow::bail!(
            "whisper-cli завершился с ошибкой (status: {}). stderr (хвост ~500 симв.): {}",
            out.status,
            tail
        );
    }

    Ok(text)
}

fn command_output_with_timeout(mut cmd: Command, timeout: Duration) -> anyhow::Result<Output> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(anyhow!("child stdout pipe is unavailable"));
    };
    let Some(mut stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(anyhow!("child stderr pipe is unavailable"));
    };
    // Drain both pipes while the process runs. Waiting for exit before reading
    // can deadlock on Windows once whisper's startup/timing logs fill a pipe.
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(anyhow!(
                    "whisper-cli exceeded its {}s runtime deadline",
                    timeout.as_secs()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(error.into());
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow!("whisper-cli stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("whisper-cli stderr reader panicked"))??;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

// ─────────────────────────── whisper-server (persistent) ───────────────────────────

/// Постоянный whisper-server: модель грузится ОДИН раз → быстрые повторные запросы.
pub struct Server {
    pub child: Child,
    pub model: PathBuf,
    pub runtime_dir: PathBuf,
    pub port: u16,
}

#[derive(Debug)]
pub struct ServerStartTimeout;

impl std::fmt::Display for ServerStartTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("whisper-server readiness timeout")
    }
}

impl std::error::Error for ServerStartTimeout {}

pub fn reserve_loopback_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("не удалось подобрать свободный loopback-порт")?;
    let port = listener
        .local_addr()
        .context("не удалось прочитать loopback-порт")?
        .port();
    Ok(port)
}

/// Поднять whisper-server и дождаться готовности (большая модель грузится несколько секунд).
///
/// Точность: запускаем с beam search (`-bs 5`) и best-of (`-bo 5`). Эти параметры
/// действуют ТОЛЬКО на запуске сервера (per-request best_of/beam_size игнорируются
/// этим билдом — проверено побайтово), поэтому жадное декодирование по умолчанию
/// (`-bo 2`, без beam) и было источником «неверных слов». Если билд сервера не
/// понимает эти флаги и не поднимается — откатываемся на минимальные аргументы.
pub fn start_server(
    whisper_dir: &Path,
    model: &Path,
    port: u16,
    threads: u32,
) -> anyhow::Result<Server> {
    start_server_inner(
        whisper_dir,
        model,
        port,
        threads,
        None,
        Duration::from_secs(60),
    )
}

pub fn start_server_with_timeout(
    whisper_dir: &Path,
    model: &Path,
    port: u16,
    threads: u32,
    ready_timeout: Duration,
) -> anyhow::Result<Server> {
    start_server_inner(whisper_dir, model, port, threads, None, ready_timeout)
}

/// Start whisper-server while allowing a dictation's Stop flag to abort the
/// readiness wait. This is used only by live preview/warmup: final ASR keeps the
/// non-cancellable path above so it can finish after the key is released.
pub fn start_server_cancellable(
    whisper_dir: &Path,
    model: &Path,
    port: u16,
    threads: u32,
    cancel: &AtomicBool,
    ready_timeout: Duration,
) -> anyhow::Result<Server> {
    start_server_inner(
        whisper_dir,
        model,
        port,
        threads,
        Some(cancel),
        ready_timeout,
    )
}

fn start_server_inner(
    whisper_dir: &Path,
    model: &Path,
    port: u16,
    threads: u32,
    cancel: Option<&AtomicBool>,
    ready_timeout: Duration,
) -> anyhow::Result<Server> {
    if cancellation_requested(cancel) {
        return Err(anyhow!("whisper-server start cancelled"));
    }
    // Сначала пробуем с флагами точности; при неудаче — без них (совместимость билда).
    match try_start_server(
        whisper_dir,
        model,
        port,
        threads,
        true,
        cancel,
        ready_timeout,
    ) {
        Ok(srv) => Ok(srv),
        Err(e) => {
            if cancellation_requested(cancel) || e.downcast_ref::<ServerStartTimeout>().is_some() {
                return Err(e);
            }
            log::warn!("whisper-server с флагами точности не поднялся ({e}), откат на минимальные аргументы");
            try_start_server(
                whisper_dir,
                model,
                port,
                threads,
                false,
                cancel,
                ready_timeout,
            )
        }
    }
}

fn cancellation_requested(cancel: Option<&AtomicBool>) -> bool {
    cancel
        .map(|flag| flag.load(Ordering::Acquire))
        .unwrap_or(false)
}

/// Одна попытка запуска whisper-server. `accuracy` — добавить ли beam/best-of.
fn try_start_server(
    whisper_dir: &Path,
    model: &Path,
    port: u16,
    threads: u32,
    accuracy: bool,
    cancel: Option<&AtomicBool>,
    ready_timeout: Duration,
) -> anyhow::Result<Server> {
    if cancellation_requested(cancel) {
        return Err(anyhow!("whisper-server start cancelled"));
    }
    let exe = whisper_dir.join(if cfg!(windows) {
        "whisper-server.exe"
    } else {
        "whisper-server"
    });
    let mut cmd = Command::new(&exe);
    cmd.current_dir(whisper_dir);
    cmd.arg("-m")
        .arg(model)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("-t")
        .arg(threads.to_string());
    if accuracy {
        // beam search + best-of: заметно лучше выбор слов, чем жадное декодирование.
        // beam=2 (а не 5): ~1.5–2× быстрее на GPU при почти той же точности на
        // large-v3-turbo — баланс «скорость↔качество» в пользу отзывчивости (жалоба
        // на задержку). Чистая жадность (-bo 2 без beam) давала неверные слова.
        cmd.arg("-bs").arg("2").arg("-bo").arg("2");
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let child = cmd
        .spawn()
        .map_err(|e| anyhow!("не удалось запустить whisper-server: {e}"))?;
    let mut srv = Server {
        child,
        model: model.to_path_buf(),
        runtime_dir: whisper_dir.to_path_buf(),
        port,
    };

    let deadline = Instant::now() + ready_timeout;
    let mut exited = false;
    while Instant::now() < deadline {
        if cancellation_requested(cancel) {
            let _ = srv.child.kill();
            let _ = srv.child.wait();
            return Err(anyhow!("whisper-server start cancelled"));
        }
        // Probe immediately, then at a short cadence. The previous fixed 500 ms
        // sleep was paid even when a warm macOS model became ready almost at
        // once, and made Stop cancellation sluggish while ensure_server held
        // the shared server mutex.
        if server_ready(port) {
            return Ok(srv);
        }
        // если процесс уже умер — выходим раньше
        if srv.child.try_wait().map(|o| o.is_some()).unwrap_or(true) {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = srv.child.kill();
    let _ = srv.child.wait();
    if exited {
        Err(anyhow!("whisper-server завершился до готовности"))
    } else {
        Err(anyhow::Error::new(ServerStartTimeout))
    }
}

/// Готов ли сервер (curl к корню отвечает).
pub fn server_ready(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/");
    let mut cmd = Command::new("curl");
    cmd.arg("--noproxy")
        .arg("*")
        .arg("-s")
        .arg("-o")
        .arg(if cfg!(windows) { "NUL" } else { "/dev/null" })
        .arg("--connect-timeout")
        .arg("0.1")
        .arg("-m")
        .arg("0.25")
        .arg(&url);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// Транскрибировать WAV через работающий whisper-server (/inference, multipart via curl).
pub fn transcribe_server(
    port: u16,
    wav: &Path,
    language: &str,
    prompt: Option<&str>,
) -> anyhow::Result<String> {
    let url = format!("http://127.0.0.1:{port}/inference");
    let file_arg = format!("file=@{}", wav.display());
    let lang_arg = format!("language={language}");
    let mut cmd = Command::new("curl");
    cmd.arg("--noproxy")
        .arg("*")
        .arg("-s")
        .arg("-m")
        .arg(server_request_timeout_secs(wav).to_string())
        .arg("-F")
        .arg(&file_arg)
        .arg("-F")
        .arg("response_format=verbose_json")
        .arg("-F")
        .arg(&lang_arg)
        .arg("-F")
        .arg("temperature=0.0");
    if let Some(p) = prompt {
        cmd.arg("-F").arg(format!("prompt={p}"));
    }
    cmd.arg(&url);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd.output().map_err(|e| anyhow!("curl /inference: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!("whisper-server /inference: код {}", out.status));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    Ok(parse_verbose(&raw, language))
}

fn server_request_timeout_for_audio(audio_seconds: u64) -> u64 {
    // Warm macOS inference is normally far below this. Short clips should not
    // hold the final-ASR lane for the old fixed 45 seconds if a sidecar wedges;
    // longer dictations retain the full historical budget.
    10u64
        .saturating_add(audio_seconds.saturating_mul(3))
        .clamp(15, 45)
}

fn server_request_timeout_secs(wav: &Path) -> u64 {
    server_request_timeout_for_audio(crate::audio::wav_duration_secs_ceil(wav).unwrap_or(10))
}

/// Транскрибировать WAV через whisper-server БЕЗ гейта уверенности.
///
/// Для ЖИВЫХ (partial) результатов: текст может быть черновым/«мусорным» —
/// мы показываем его в пилюле серым и (опционально) вставляем инкрементально,
/// но НЕ применяем порог уверенности (его место — финальный проход).
/// Таймаут очень короткий: live-preview best-effort, а зависший тик держит
/// общий asr_lock и задерживает финальный проход после отпускания хоткея.
pub fn transcribe_server_partial(port: u16, wav: &Path, language: &str) -> anyhow::Result<String> {
    let url = format!("http://127.0.0.1:{port}/inference");
    let file_arg = format!("file=@{}", wav.display());
    let lang_arg = format!("language={language}");
    let mut cmd = Command::new("curl");
    cmd.arg("--noproxy")
        .arg("*")
        .arg("-s")
        .arg("-m")
        .arg("2")
        .arg("-F")
        .arg(&file_arg)
        .arg("-F")
        .arg("response_format=verbose_json")
        .arg("-F")
        .arg(&lang_arg)
        .arg("-F")
        .arg("temperature=0.0")
        .arg(&url);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd
        .output()
        .map_err(|e| anyhow!("curl /inference (partial): {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "whisper-server /inference (partial): код {}",
            out.status
        ));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    Ok(extract_text(&raw))
}

// Пороги «гейта уверенности» против галлюцинаций whisper на невнятном/коротком звуке.
const MIN_MEAN_PROB: f64 = 0.60; // средняя вероятность слов
const LOW_PROB: f64 = 0.40; // что считаем «неуверенным» словом
const MAX_LOW_FRAC: f64 = 0.34; // доля неуверенных слов
const MAX_NO_SPEECH: f64 = 0.70; // вероятность «это не речь»

/// Достать «сырой» текст из ответа сервера БЕЗ гейта уверенности.
/// Парсит поле "text" из verbose_json (схлопывает переводы строк/пробелы);
/// если это не JSON — откатывается на `clean_server_text`.
fn extract_text(json: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(json.trim()) {
        Ok(v) => v,
        Err(_) => return clean_server_text(json), // не JSON — вернём как есть
    };
    let text = v
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .replace('\n', " ");
    // Схлопнуть пробелы, убрать «прилипшие» к пунктуации пробелы (для пилюли/живой
    // вставки) и срезать повторяющиеся подряд n-граммы (beam иногда зацикливается).
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let deduped = dedup_repeats(&collapsed);
    crate::postprocess::normalize_spaces(&deduped)
}

/// Срезать подряд повторяющиеся n-граммы (1..=5 токенов), оставив одну копию.
///
/// Защита от редкого «repeat loop» beam-декодера, который гейт уверенности может
/// пропустить (повтор бывает высоковероятным). Сравнение регистронезависимое и
/// без хвостовой пунктуации, чтобы «слово слово.» тоже схлопывалось.
///
/// КОНСЕРВАТИВНО, чтобы не портить осмысленные повторы:
///  - блок из 1 токена схлопываем ТОЛЬКО при 3+ копиях подряд (сигнатура цикла);
///    обычное «да да» / «очень очень» (2 копии) оставляем как есть;
///  - блок из 2+ токенов схлопываем при 2+ копиях (повтор фразы — почти всегда петля).
pub fn dedup_repeats(text: &str) -> String {
    let toks: Vec<&str> = text.split_whitespace().collect();
    if toks.len() < 2 {
        return text.to_string();
    }
    let norm = |t: &str| {
        t.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let mut out: Vec<&str> = Vec::with_capacity(toks.len());
    let mut i = 0usize;
    while i < toks.len() {
        let mut matched = false;
        // Пробуем самые длинные повторы первыми, чтобы не дробить фразу.
        let max_n = ((toks.len() - i) / 2).min(5);
        for n in (1..=max_n).rev() {
            let a = &toks[i..i + n];
            let b = &toks[i + n..i + 2 * n];
            if a.iter().map(|t| norm(t)).eq(b.iter().map(|t| norm(t))) {
                // Считаем, сколько раз блок повторяется подряд.
                let mut j = i + n;
                let mut reps = 1usize; // уже знаем про 1 повтор (a == b)
                while j + n <= toks.len()
                    && toks[i..i + n]
                        .iter()
                        .map(|t| norm(t))
                        .eq(toks[j..j + n].iter().map(|t| norm(t)))
                {
                    j += n;
                    reps += 1;
                }
                // reps — число ДОПОЛНИТЕЛЬНЫХ копий (всего блоков = reps + 1).
                let total_blocks = reps + 1;
                let collapse = n >= 2 || total_blocks >= 3;
                if collapse {
                    out.extend_from_slice(&toks[i..i + n]); // оставляем ОДНУ копию
                    i = j;
                    matched = true;
                    break;
                }
                // Не схлопываем (2 копии одиночного токена) — пропускаем как обычные токены.
            }
        }
        if !matched {
            out.push(toks[i]);
            i += 1;
        }
    }
    out.join(" ")
}

/// Разобрать verbose_json и применить гейт уверенности.
/// Возвращает текст, ТОЛЬКО если модель уверена; иначе пустую строку (не вставляем мусор).
fn parse_verbose(json: &str, requested_language: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(json.trim()) {
        Ok(v) => v,
        Err(_) => return clean_server_text(json), // не JSON — вернём как есть
    };

    let text = extract_text(json);

    let mut probs: Vec<f64> = Vec::new();
    // Гейт no_speech: раньше брали МАКСИМУМ no_speech_prob по всем сегментам — из-за
    // этого одна пауза/вдох в середине длинной диктовки заворачивала ВЕСЬ текст.
    // Теперь считаем ДОЛЮ сегментов, помеченных как «не речь», и общее число сегментов:
    // отклоняем по no_speech лишь если таких сегментов большинство (или сегмент один).
    let mut seg_count = 0usize;
    let mut nsp_high = 0usize;
    if let Some(segs) = v.get("segments").and_then(|s| s.as_array()) {
        for seg in segs {
            seg_count += 1;
            if let Some(nsp) = seg.get("no_speech_prob").and_then(|x| x.as_f64()) {
                if nsp > MAX_NO_SPEECH {
                    nsp_high += 1;
                }
            }
            if let Some(words) = seg.get("words").and_then(|w| w.as_array()) {
                for w in words {
                    let wt = w.get("word").and_then(|x| x.as_str()).unwrap_or("");
                    if !wt.chars().any(|c| c.is_alphabetic()) {
                        continue; // пропускаем чистую пунктуацию
                    }
                    if let Some(p) = w.get("probability").and_then(|x| x.as_f64()) {
                        probs.push(p);
                    }
                }
            }
        }
    }

    let det_lang = v
        .get("detected_language")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let det_prob = v
        .get("detected_language_probability")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let lang_mismatch = language_mismatch(requested_language, det_lang, det_prob);

    let (mean, frac_low) = if probs.is_empty() {
        (1.0, 0.0)
    } else {
        let mean = probs.iter().sum::<f64>() / probs.len() as f64;
        let low = probs.iter().filter(|&&p| p < LOW_PROB).count();
        (mean, low as f64 / probs.len() as f64)
    };

    // no_speech: при ≥2 сегментах заворачиваем только если БОЛЬШИНСТВО сегментов —
    // «не речь» (одиночная пауза в длинной диктовке больше не убивает весь текст).
    // При одном сегменте (короткий клип) поведение строгое, как раньше.
    let nsp_reject = if seg_count <= 1 {
        nsp_high >= 1
    } else {
        (nsp_high as f64 / seg_count as f64) > 0.6
    };
    let nsp_frac = if seg_count > 0 {
        nsp_high as f64 / seg_count as f64
    } else {
        0.0
    };

    let reject = mean < MIN_MEAN_PROB || frac_low > MAX_LOW_FRAC || nsp_reject || lang_mismatch;

    if reject || text.is_empty() {
        log::info!(
            "ASR отклонён: mean={mean:.2} frac_low={frac_low:.2} nsp_frac={nsp_frac:.2} ({nsp_high}/{seg_count}) req_lang={requested_language} lang={det_lang}/{det_prob:.2} text={text:?}"
        );
        return String::new();
    }
    text
}

fn canonical_gate_language(language: &str) -> Option<&'static str> {
    match language.trim().to_ascii_lowercase().as_str() {
        "ru" | "russian" => Some("ru"),
        "en" | "english" => Some("en"),
        _ => None,
    }
}

fn language_mismatch(
    requested_language: &str,
    detected_language: &str,
    detected_probability: f64,
) -> bool {
    if detected_probability <= 0.60 || detected_language.trim().is_empty() {
        return false;
    }
    let Some(requested) = canonical_gate_language(requested_language) else {
        return false;
    };
    match canonical_gate_language(detected_language) {
        Some(detected) => detected != requested,
        None => true,
    }
}

/// Сервер может вернуть text или json {"text":"..."} — нормализуем в чистый текст.
fn clean_server_text(s: &str) -> String {
    let t = s.trim();
    if t.starts_with('{') {
        if let Some(i) = t.find("\"text\"") {
            if let Some(colon) = t[i..].find(':') {
                let after = t[i + colon + 1..].trim_start();
                if let Some(stripped) = after.strip_prefix('"') {
                    if let Some(end) = stripped.find('"') {
                        return stripped[..end].replace("\\n", " ").trim().to_string();
                    }
                }
            }
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_gate_respects_auto_and_manual_ru_en() {
        assert!(!language_mismatch("auto", "english", 0.95));
        assert!(!language_mismatch("de", "german", 0.95));
        assert!(!language_mismatch("ru", "russian", 0.95));
        assert!(!language_mismatch("en", "english", 0.95));
        assert!(language_mismatch("ru", "english", 0.95));
        assert!(language_mismatch("en", "russian", 0.95));
        assert!(!language_mismatch("ru", "english", 0.40));
    }

    #[test]
    fn reserve_loopback_port_returns_reusable_port() {
        let port = reserve_loopback_port().expect("reserve loopback port");
        assert_ne!(port, 0);
        let listener = std::net::TcpListener::bind(("127.0.0.1", port))
            .expect("reserved port should be reusable after listener drop");
        drop(listener);
    }

    #[test]
    fn final_server_timeout_scales_without_penalizing_short_taps() {
        assert_eq!(server_request_timeout_for_audio(0), 15);
        assert_eq!(server_request_timeout_for_audio(1), 15);
        assert_eq!(server_request_timeout_for_audio(5), 25);
        assert_eq!(server_request_timeout_for_audio(30), 45);
    }
}
