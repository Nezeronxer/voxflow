//! Определение локального ИИ на машине и подбор моделей под её память.
//!
//! Зачем. Раньше локальный бэкенд подключался руками: выбрать движок, вписать
//! адрес, выбрать модель, а если модели нет — идти в терминал за `ollama pull`.
//! Человек с уже установленной Ollama об этом не узнавал вовсе.
//!
//! Что делает модуль:
//!
//! 1. **Ищет движок** на известных локальных портах. Наружу не ходит — только
//!    петля, так что проба ничего не сообщает о пользователе третьим лицам.
//! 2. **Подбирает ряд моделей под конкретную машину.** Каталог размечен ярусами
//!    по объёму памяти: на 8 ГБ показываются только те, что реально влезут, на
//!    32 ГБ ряд сдвигается вверх. Пороги считаются от ПОЛНОГО объёма памяти, а не
//!    от свободного в момент запуска: свободное скачет от того, открыт ли браузер,
//!    и модель, выбранная по нему, назавтра не влезет.
//!
//! LM Studio и llama.cpp отдельного бэкенда не требуют: оба говорят
//! OpenAI-совместимым протоколом, который уже реализован в [`crate::rewrite`].

use serde::Serialize;

/// Локальные адреса, на которых слушают известные движки.
///
/// Порт 8771 занят собственным whisper-server проекта и сюда не попадает.
const PROBES: &[Probe] = &[
    Probe {
        engine: "ollama",
        label: "Ollama",
        url: "http://localhost:11434",
        endpoint: "/api/tags",
    },
    Probe {
        engine: "lmstudio",
        label: "LM Studio",
        url: "http://localhost:1234/v1",
        endpoint: "/models",
    },
    Probe {
        engine: "llamacpp",
        label: "llama.cpp",
        url: "http://localhost:8080/v1",
        endpoint: "/models",
    },
];

struct Probe {
    engine: &'static str,
    label: &'static str,
    url: &'static str,
    endpoint: &'static str,
}

/// Модель из каталога.
///
/// `min_ram_gb` — с какого объёма памяти модель имеет смысл предлагать. Это не
/// размер файла: модель делит память с распознаванием (whisper ~0.5 ГБ, GigaAM и
/// Parakeet ~0.65 ГБ каждая), браузером и мессенджерами, поэтому порог заметно
/// выше веса самой модели.
#[derive(Serialize, Clone, Debug)]
pub struct CatalogModel {
    pub tag: &'static str,
    pub label: &'static str,
    pub size_gb: f32,
    pub min_ram_gb: u32,
}

const CATALOG: &[CatalogModel] = &[
    CatalogModel {
        tag: "qwen2.5:3b",
        label: "Qwen2.5 3B",
        size_gb: 1.9,
        min_ram_gb: 8,
    },
    CatalogModel {
        tag: "qwen3:4b",
        label: "Qwen3 4B",
        size_gb: 2.6,
        min_ram_gb: 16,
    },
    CatalogModel {
        tag: "gemma3:4b",
        label: "Gemma 3 4B",
        size_gb: 3.3,
        min_ram_gb: 16,
    },
    CatalogModel {
        tag: "qwen3:8b",
        label: "Qwen3 8B",
        size_gb: 5.2,
        min_ram_gb: 24,
    },
    CatalogModel {
        tag: "gemma3:12b",
        label: "Gemma 3 12B",
        size_gb: 8.1,
        min_ram_gb: 32,
    },
];

/// Сколько моделей показываем как рекомендованные.
const SHORTLIST: usize = 3;

/// Полный объём оперативной памяти в гигабайтах.
///
/// Именно полный, а не свободный: см. заметку о порогах в шапке модуля.
/// Ноль — определить не удалось; вызывающий обязан трактовать это как «ярусы не
/// фильтруем», а не как «памяти нет».
pub fn total_ram_gb() -> u32 {
    #[cfg(target_os = "macos")]
    {
        // sysctl вместо крейта: два вызова не стоят новой зависимости.
        if let Ok(out) = std::process::Command::new("sysctl")
            .arg("-n")
            .arg("hw.memsize")
            .output()
        {
            if let Ok(text) = String::from_utf8(out.stdout) {
                if let Ok(bytes) = text.trim().parse::<u64>() {
                    return (bytes / 1_073_741_824) as u32;
                }
            }
        }
        0
    }
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok() {
            return (status.ullTotalPhys / 1_073_741_824) as u32;
        }
        0
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        0
    }
}

/// Чем машина считает модель. Это главный вопрос, а не объём ОЗУ: на десктопе с
/// дискретной картой решает VRAM, и системная память там ни при чём.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Accel {
    /// Apple Silicon: память общая, GPU берёт из тех же гигабайт, что и всё остальное.
    ///
    /// Вне macOS не конструируется (симметрично `Nvidia` ниже), но в правилах
    /// подбора и тестах участвует на всех платформах.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    AppleSilicon,
    /// Дискретная NVIDIA. `vram_gb == 0` — карта есть, объём выяснить не удалось.
    ///
    /// На macOS этот вариант не собирается (дискретных NVIDIA там нет), но в
    /// правилах подбора и тестах он участвует на всех платформах.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Nvidia { vram_gb: u32 },
    /// Ни Metal, ни CUDA: Intel-мак, машина без карты. Инференс идёт на CPU и
    /// медленный, поэтому потолок здесь заметно ниже.
    CpuOnly,
}

/// Конфигурация машины, по которой подбирается модель.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Machine {
    pub ram_gb: u32,
    pub cpu_cores: u32,
    pub accel: Accel,
}

impl Default for Machine {
    fn default() -> Self {
        Machine {
            ram_gb: 0,
            cpu_cores: 0,
            accel: Accel::CpuOnly,
        }
    }
}

/// Модель, которую CPU-only машина посчитает за разумное время. Всё, что крупнее,
/// на голом процессоре превращает вставку текста в ожидание.
const CPU_ONLY_MAX_GB: f32 = 2.5;

/// Запас VRAM поверх веса модели: контекст и KV-кэш тоже живут в видеопамяти.
const VRAM_HEADROOM_GB: f32 = 1.5;

/// Осмотреть машину.
pub fn probe_machine() -> Machine {
    Machine {
        ram_gb: total_ram_gb(),
        cpu_cores: std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0),
        accel: probe_accel(),
    }
}

fn probe_accel() -> Accel {
    #[cfg(target_os = "macos")]
    {
        // hw.optional.arm64 = 1 только на Apple Silicon; на Intel-маке Metal есть,
        // но общей памяти и её пропускной способности — нет.
        let arm = std::process::Command::new("sysctl")
            .arg("-n")
            .arg("hw.optional.arm64")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|v| v.trim() == "1")
            .unwrap_or(false);
        if arm {
            return Accel::AppleSilicon;
        }
        Accel::CpuOnly
    }
    #[cfg(not(target_os = "macos"))]
    {
        if crate::paths::gpu_active() {
            return Accel::Nvidia {
                vram_gb: nvidia_vram_gb(),
            };
        }
        Accel::CpuOnly
    }
}

/// Объём видеопамяти через `nvidia-smi` — он ставится вместе с драйвером.
/// Ноль означает «карта есть, объём неизвестен»: тогда падаем на общее правило
/// по ОЗУ, а не выдумываем число.
#[cfg(not(target_os = "macos"))]
fn nvidia_vram_gb() -> u32 {
    let mut cmd = std::process::Command::new("nvidia-smi");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.arg("--query-gpu=memory.total")
        .arg("--format=csv,noheader,nounits")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|text| {
            // Несколько карт — берём самую большую: на ней и будем считать.
            text.lines()
                .filter_map(|l| l.trim().parse::<u32>().ok())
                .max()
        })
        .map(|mib| mib / 1024)
        .unwrap_or(0)
}

/// Потянет ли эта машина эту модель.
pub fn fits_machine(model: &CatalogModel, machine: &Machine) -> bool {
    fits_spec(model.size_gb, model.min_ram_gb, machine)
}

/// То же правило для любой модели по её весу и порогу памяти — им пользуется
/// и встроенный локальный ИИ ([`crate::local_llm`]), чтобы ряд GGUF-моделей
/// подбирался по тем же ярусам, что и модели Ollama.
pub fn fits_spec(size_gb: f32, min_ram_gb: u32, machine: &Machine) -> bool {
    let model = CatalogModel {
        tag: "",
        label: "",
        size_gb,
        min_ram_gb,
    };
    let model = &model;
    match machine.accel {
        // Дискретная карта: модель живёт в VRAM, системная память ей не предел.
        // Именно поэтому нельзя судить по одному ОЗУ — 16 ГБ VRAM тянут то, чего
        // не потянут 16 ГБ системной памяти на маке.
        Accel::Nvidia { vram_gb } if vram_gb > 0 => {
            model.size_gb + VRAM_HEADROOM_GB <= vram_gb as f32
        }
        // Голый процессор: даже если памяти много, крупная модель считает так
        // долго, что предлагать её нечестно.
        Accel::CpuOnly => {
            model.size_gb <= CPU_ONLY_MAX_GB
                && (machine.ram_gb == 0 || machine.ram_gb >= model.min_ram_gb)
        }
        // Общая память (Apple Silicon) и NVIDIA с неизвестным объёмом — по ярусам.
        _ => machine.ram_gb == 0 || machine.ram_gb >= model.min_ram_gb,
    }
}

/// Модели, которые эта машина потянет, — от самой лёгкой к самой тяжёлой.
pub fn fits(machine: &Machine) -> Vec<CatalogModel> {
    CATALOG
        .iter()
        .filter(|m| fits_machine(m, machine))
        .cloned()
        .collect()
}

/// Ряд для плашки: [`SHORTLIST`] самых крупных подходящих моделей.
pub fn shortlist(machine: &Machine) -> Vec<CatalogModel> {
    let fitting = fits(machine);
    let skip = fitting.len().saturating_sub(SHORTLIST);
    fitting.into_iter().skip(skip).collect()
}

/// Модели, которые машина не тянет, — показываем свёрнутым списком с пометкой.
/// Скрывать совсем нельзя: врать о доступном не наше дело, как и запрещать.
pub fn too_heavy(machine: &Machine) -> Vec<CatalogModel> {
    CATALOG
        .iter()
        .filter(|m| !fits_machine(m, machine))
        .cloned()
        .collect()
}

/// Найденный на машине движок.
#[derive(Serialize, Clone, Debug)]
pub struct FoundEngine {
    pub engine: String,
    pub label: String,
    pub url: String,
    pub models: Vec<String>,
}

/// Что нашлось и что предлагать.
#[derive(Serialize, Clone, Debug, Default)]
pub struct Detection {
    pub engines: Vec<FoundEngine>,
    /// Конфигурация машины — её же показываем в интерфейсе, чтобы было видно,
    /// ПОЧЕМУ предложен именно такой ряд.
    pub machine: Machine,
    pub shortlist: Vec<CatalogModel>,
    pub too_heavy: Vec<CatalogModel>,
    /// Установлена ли Ollama: только она умеет ставить модели по кнопке.
    pub can_pull: bool,
}

/// Опросить локальные порты. Не ходит наружу и не бросает ошибок: не ответивший
/// порт — это норма, а не сбой.
pub fn detect() -> Detection {
    let machine = probe_machine();
    let mut engines = Vec::new();

    for probe in PROBES {
        if let Some(models) = probe_models(probe) {
            engines.push(FoundEngine {
                engine: probe.engine.to_string(),
                label: probe.label.to_string(),
                url: probe.url.to_string(),
                models,
            });
        }
    }

    let can_pull = engines.iter().any(|e| e.engine == "ollama");
    Detection {
        shortlist: shortlist(&machine),
        too_heavy: too_heavy(&machine),
        engines,
        machine,
        can_pull,
    }
}

fn probe_models(probe: &Probe) -> Option<Vec<String>> {
    // Ollama уже умеет читать свой /api/tags — не дублируем разбор.
    if probe.engine == "ollama" {
        return crate::ollama::list_models(probe.url).ok();
    }

    let endpoint = format!("{}{}", probe.url, probe.endpoint);
    let mut cmd = crate::net::curl();
    // Короткий таймаут: это проба локального порта, ждать там нечего.
    cmd.arg("-s").arg("-m").arg("2").arg(&endpoint);
    let out = cmd.output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    // OpenAI-совместимый формат: {"data":[{"id":"..."}]}.
    let models: Vec<String> = v
        .get("data")?
        .as_array()?
        .iter()
        .filter_map(|m| m.get("id").and_then(|id| id.as_str()))
        .map(str::to_string)
        .collect();
    Some(models)
}

/// Что предлагается включить.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub engine: String,
    pub label: String,
    /// Значение `ai_backend`: "ollama" либо "openai_compat".
    pub backend: String,
    pub url: String,
    pub model: String,
}

/// Стоит ли предлагать включить найденный локальный ИИ.
///
/// `None` означает «молчим», и молчим мы в трёх случаях:
///
/// - пользователь уже отказался (`local_ai_dismissed`) — навязываться нельзя;
/// - **бэкенд уже настроен** (`ai_backend != "off"`) — увести диктовку с
///   выбранного облака на локальную модель за спиной значило бы молча поменять
///   и качество, и приватность;
/// - движок есть, но моделей в нём нет — включать нечего.
pub fn suggest(det: &Detection, s: &crate::settings::Settings) -> Option<Suggestion> {
    if s.local_ai_dismissed || s.ai_backend.trim() != "off" {
        return None;
    }
    let engine = det.engines.iter().find(|e| !e.models.is_empty())?;
    let model = pick_installed(&engine.models, &det.machine)?;
    let backend = if engine.engine == "ollama" {
        "ollama"
    } else {
        // LM Studio и llama.cpp говорят OpenAI-совместимым протоколом, который
        // в проекте уже реализован, — отдельный бэкенд им не нужен.
        "openai_compat"
    };
    Some(Suggestion {
        engine: engine.engine.clone(),
        label: engine.label.clone(),
        backend: backend.to_string(),
        url: engine.url.clone(),
        model,
    })
}

/// Какую модель включить из уже установленных.
///
/// Берём самую крупную, проходящую по памяти: если на 32 ГБ уже стоит 8B, глупо
/// включать трёхмиллиардную. Незнакомую модель (не из каталога) считаем
/// подходящей — раз человек её поставил, он знал, что делает.
pub fn pick_installed(installed: &[String], machine: &Machine) -> Option<String> {
    let fitting = fits(machine);
    // Сравниваем с ПОЛНЫМ тегом, а не с семейством: `gemma3:12b` не становится
    // четырёхмиллиардной оттого, что у неё то же имя семейства, что у `gemma3:4b`.
    // Суффикс через дефис — это вариант кванта того же размера (`qwen3:4b-q4_K_M`),
    // его принимаем.
    let rank = |name: &str| -> Option<usize> {
        fitting
            .iter()
            .position(|m| name == m.tag || name.starts_with(&format!("{}-", m.tag)))
    };

    let mut best: Option<(usize, &String)> = None;
    for name in installed {
        if let Some(r) = rank(name) {
            if best.map(|(br, _)| r > br).unwrap_or(true) {
                best = Some((r, name));
            }
        }
    }
    if let Some((_, name)) = best {
        return Some(name.clone());
    }
    // Ничего из каталога — берём первую установленную, какая есть.
    installed.first().cloned()
}

/// На что переключить локальный ИИ, если выбранная модель не установлена (D-043).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalAiAdopt {
    /// Встроенный llama.cpp с этой моделью каталога.
    Builtin(String),
    /// Ollama с этим тегом.
    Ollama(String),
}

/// Чистый выбор установленной локальной модели ИИ-обработки.
///
/// - `builtin_ready` - готовые встроенные модели (порядок каталога, пусто без рантайма);
/// - `ollama` - модели локальной Ollama, `None` - не ответила;
/// - `ollama_local` - адрес Ollama на петле: удалённую не трогаем, это уже не локальный ИИ.
///
/// `None` - оставить как есть: выбранная модель на месте, бэкенд облачный или
/// выключен, либо ставить нечего. Недоступная Ollama - не повод уводить с неё:
/// она могла ещё не стартовать, а модель там есть.
pub fn plan_local_ai(
    backend: &str,
    builtin_model: &str,
    builtin_ready: &[String],
    ollama_model: &str,
    ollama: Option<&[String]>,
    ollama_local: bool,
    machine: &Machine,
) -> Option<LocalAiAdopt> {
    // Самая крупная из готовых встроенных - последняя в порядке каталога.
    let builtin_pick = || builtin_ready.last().cloned().map(LocalAiAdopt::Builtin);
    let ollama_pick = || {
        let list = ollama.filter(|_| ollama_local)?;
        if crate::ollama::has_model(list, ollama_model) {
            return Some(LocalAiAdopt::Ollama(ollama_model.trim().to_string()));
        }
        pick_installed(list, machine).map(LocalAiAdopt::Ollama)
    };
    match backend.trim() {
        "builtin" if builtin_ready.iter().any(|m| m == builtin_model.trim()) => None,
        "builtin" => builtin_pick().or_else(ollama_pick),
        "ollama" if !ollama_local => None,
        "ollama" => match ollama {
            None => None,
            Some(list) if crate::ollama::has_model(list, ollama_model) => None,
            Some(list) if !list.is_empty() => ollama_pick(),
            Some(_) => builtin_pick(),
        },
        _ => None,
    }
}

/// В фоне подобрать установленную локальную ИИ-модель вместо отсутствующей и
/// записать её в настройки. Ничего не качает. Зовётся на старте и при смене
/// `ai_backend`: правку модели руками (например, перед `ollama pull`) не перебиваем.
pub fn spawn_adopt_installed(app: tauri::AppHandle, snapshot: crate::settings::Settings) {
    if !matches!(snapshot.ai_backend.trim(), "builtin" | "ollama") {
        return;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("voxflow-ai-adopt".into())
        .spawn(move || {
            let ollama_local = crate::net::is_loopback_base_url(&snapshot.ollama_url);
            // Петля отвечает мгновенно; секунда - запас на занятую машину.
            let ollama = ollama_local
                .then(|| crate::ollama::list_models_within(&snapshot.ollama_url, 1).ok())
                .flatten();
            let plan = plan_local_ai(
                &snapshot.ai_backend,
                &snapshot.builtin_llm_model,
                &crate::local_llm::ready_models(),
                &snapshot.ollama_model,
                ollama.as_deref(),
                ollama_local,
                &probe_machine(),
            );
            if let Some(plan) = plan {
                adopt(&app, &snapshot, plan);
            }
        })
    {
        log::warn!("не удалось запустить подбор локального ИИ: {e}");
    }
}

fn adopt(app: &tauri::AppHandle, snapshot: &crate::settings::Settings, plan: LocalAiAdopt) {
    use tauri::{Emitter, Manager};
    let Some(state) = app.try_state::<crate::commands::AppState>() else {
        return;
    };
    // Проверка и запись под одним захватом: save_settings не вклинится между ними,
    // а правка пользователя за время пробы отменяет подмену.
    let next = {
        let mut cur = state.settings.lock();
        if cur.ai_backend != snapshot.ai_backend
            || cur.builtin_llm_model != snapshot.builtin_llm_model
            || cur.ollama_model != snapshot.ollama_model
            || cur.ollama_url != snapshot.ollama_url
        {
            return;
        }
        let mut next = cur.clone();
        match &plan {
            LocalAiAdopt::Builtin(id) => {
                next.ai_backend = "builtin".into();
                next.builtin_llm_model = id.clone();
            }
            LocalAiAdopt::Ollama(tag) => {
                next.ai_backend = "ollama".into();
                next.ollama_model = tag.clone();
            }
        }
        if let Err(e) = crate::settings::save(&state.db.lock(), &next) {
            log::error!("подбор локального ИИ: не записал {plan:?}: {e:#}");
            return;
        }
        *cur = next.clone();
        next
    };
    if snapshot.ai_backend == "builtin" {
        crate::local_llm::stop();
    }
    log::info!(
        "локальный ИИ: {} / {} не установлена, используем {plan:?}",
        snapshot.ai_backend,
        if snapshot.ai_backend == "builtin" {
            &snapshot.builtin_llm_model
        } else {
            &snapshot.ollama_model
        }
    );
    let _ = app.emit("settings_changed", next.redacted_for_renderer());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ряд обязан подстраиваться под машину: на слабой не предлагаем то, что
    /// уйдёт в своп, на мощной не занижаем.
    #[test]
    fn shortlist_follows_the_machine() {
        let light: Vec<&str> = shortlist(&unified(8)).iter().map(|m| m.tag).collect();
        assert_eq!(
            light,
            vec!["qwen2.5:3b"],
            "на 8 ГБ только самый лёгкий ярус"
        );

        let mid: Vec<&str> = shortlist(&unified(16)).iter().map(|m| m.tag).collect();
        assert_eq!(mid, vec!["qwen2.5:3b", "qwen3:4b", "gemma3:4b"]);

        let big: Vec<&str> = shortlist(&unified(32)).iter().map(|m| m.tag).collect();
        assert_eq!(big.len(), SHORTLIST);
        assert!(big.contains(&"gemma3:12b"), "на 32 ГБ показываем крупные");
        assert!(!big.contains(&"qwen2.5:3b"), "и не занижаем ряд");

        // Совсем слабая машина: ряд пуст, но не паникуем.
        assert!(shortlist(&unified(4)).is_empty());
        // Память не определилась — показываем всё, а не ничего.
        assert_eq!(shortlist(&unified(0)).len(), SHORTLIST);
        assert!(too_heavy(&unified(0)).is_empty());
    }

    /// Главное, ради чего заведён профиль машины: на десктопе с дискретной
    /// картой решает VRAM, а не системная память. 16 ГБ видеопамяти тянут то,
    /// чего не потянут 16 ГБ общей памяти на маке, — судить по одному ОЗУ нельзя.
    #[test]
    fn discrete_gpu_is_judged_by_vram_not_system_ram() {
        let card = nvidia(16, 16);
        let tags: Vec<&str> = shortlist(&card).iter().map(|m| m.tag).collect();
        assert!(
            tags.contains(&"gemma3:12b"),
            "на карте 16 ГБ двенадцатимиллиардная обязана предлагаться, была {tags:?}"
        );

        // Та же системная память, но карты нет — ряд заметно скромнее.
        let mac_tags: Vec<&str> = shortlist(&unified(16)).iter().map(|m| m.tag).collect();
        assert!(!mac_tags.contains(&"gemma3:12b"));

        // Слабая карта: в 6 ГБ VRAM крупная не влезет даже при 64 ГБ ОЗУ.
        let weak: Vec<&str> = shortlist(&nvidia(64, 6)).iter().map(|m| m.tag).collect();
        assert!(!weak.contains(&"gemma3:12b"));
        assert!(weak.contains(&"qwen3:4b"));

        // Карта есть, а объём выяснить не удалось — не выдумываем, судим по ОЗУ.
        let unknown: Vec<&str> = shortlist(&nvidia(16, 0)).iter().map(|m| m.tag).collect();
        assert_eq!(unknown, mac_tags);
    }

    /// Без Metal и CUDA считает процессор: даже при горе памяти крупная модель
    /// превратит вставку текста в ожидание, поэтому потолок ниже.
    #[test]
    fn cpu_only_machine_is_capped_regardless_of_ram() {
        let tags: Vec<&str> = shortlist(&cpu_only(64)).iter().map(|m| m.tag).collect();
        assert_eq!(tags, vec!["qwen2.5:3b"], "на голом CPU только самая лёгкая");
        assert!(too_heavy(&cpu_only(64)).iter().any(|m| m.tag == "qwen3:4b"));
    }

    /// Не влезающие не прячем: пользователь должен видеть, что существует и
    /// сколько памяти для этого нужно.
    #[test]
    fn too_heavy_is_shown_not_hidden() {
        let heavy: Vec<&str> = too_heavy(&unified(16)).iter().map(|m| m.tag).collect();
        assert_eq!(heavy, vec!["qwen3:8b", "gemma3:12b"]);
        assert!(too_heavy(&unified(64)).is_empty());
    }

    fn unified(ram_gb: u32) -> Machine {
        Machine {
            ram_gb,
            cpu_cores: 10,
            accel: Accel::AppleSilicon,
        }
    }

    fn nvidia(ram_gb: u32, vram_gb: u32) -> Machine {
        Machine {
            ram_gb,
            cpu_cores: 12,
            accel: Accel::Nvidia { vram_gb },
        }
    }

    fn cpu_only(ram_gb: u32) -> Machine {
        Machine {
            ram_gb,
            cpu_cores: 8,
            accel: Accel::CpuOnly,
        }
    }

    fn detection_with(engine: &str, url: &str, models: &[&str]) -> Detection {
        Detection {
            engines: vec![FoundEngine {
                engine: engine.to_string(),
                label: engine.to_string(),
                url: url.to_string(),
                models: models.iter().map(|m| m.to_string()).collect(),
            }],
            machine: unified(16),
            ..Detection::default()
        }
    }

    /// Главная гарантия: настроенный пользователем бэкенд не перезаписывается.
    /// Молча увести диктовку с облака на локальную модель — значит поменять и
    /// качество, и приватность за спиной.
    #[test]
    fn suggest_never_overrides_a_configured_backend() {
        let det = detection_with("ollama", "http://localhost:11434", &["qwen3:4b"]);

        let clean = crate::settings::Settings::default();
        assert_eq!(clean.ai_backend, "off");
        let s = suggest(&det, &clean).expect("на чистой установке предлагаем");
        assert_eq!(s.backend, "ollama");
        assert_eq!(s.model, "qwen3:4b");

        let configured = crate::settings::Settings {
            ai_backend: "gemini".into(),
            ..crate::settings::Settings::default()
        };
        assert_eq!(suggest(&det, &configured), None);

        // Отказался один раз — больше не предлагаем.
        let dismissed = crate::settings::Settings {
            local_ai_dismissed: true,
            ..crate::settings::Settings::default()
        };
        assert_eq!(suggest(&det, &dismissed), None);
    }

    /// LM Studio и llama.cpp подключаются существующим OpenAI-совместимым
    /// маршрутом, отдельного бэкенда для них не заводили.
    #[test]
    fn openai_compatible_engines_reuse_the_existing_route() {
        let det = detection_with("lmstudio", "http://localhost:1234/v1", &["local-model"]);
        let s = suggest(&det, &crate::settings::Settings::default()).expect("предложение");
        assert_eq!(s.backend, "openai_compat");
        assert_eq!(s.url, "http://localhost:1234/v1");

        // Движок есть, а моделей в нём нет — включать нечего.
        let empty = detection_with("ollama", "http://localhost:11434", &[]);
        assert_eq!(suggest(&empty, &crate::settings::Settings::default()), None);
    }

    #[test]
    fn pick_installed_takes_the_largest_that_fits() {
        let installed = vec![
            "qwen2.5:3b".to_string(),
            "qwen3:4b".to_string(),
            "gemma3:12b".to_string(),
        ];
        // На 16 ГБ двенадцатимиллиардная не проходит по ярусу.
        assert_eq!(
            pick_installed(&installed, &unified(16)).as_deref(),
            Some("qwen3:4b")
        );
        // На 32 ГБ берём её же.
        assert_eq!(
            pick_installed(&installed, &unified(32)).as_deref(),
            Some("gemma3:12b")
        );
        // Вариант кванта того же размера — это та же модель, её принимаем.
        let quant = vec!["qwen3:4b-q4_K_M".to_string()];
        assert_eq!(
            pick_installed(&quant, &unified(16)).as_deref(),
            Some("qwen3:4b-q4_K_M")
        );

        // Незнакомую модель принимаем как есть: раз поставил — значит нужна.
        let custom = vec!["my-own-model:latest".to_string()];
        assert_eq!(
            pick_installed(&custom, &unified(16)).as_deref(),
            Some("my-own-model:latest")
        );
        assert_eq!(pick_installed(&[], &unified(16)), None);
    }

    #[test]
    fn plan_local_ai_adopts_installed_local_model() {
        let m = unified(16);
        let tags = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let none: Vec<String> = Vec::new();
        let gemma4 = tags(&["gemma4:e2b-it-qat"]);
        // Случай с этого мака: встроенной нет, в Ollama другая модель.
        assert_eq!(
            plan_local_ai(
                "builtin",
                "qwen2.5-3b-instruct",
                &none,
                "gemma3:4b",
                Some(&gemma4),
                true,
                &m
            ),
            Some(LocalAiAdopt::Ollama("gemma4:e2b-it-qat".into()))
        );
        assert_eq!(
            plan_local_ai(
                "ollama",
                "qwen2.5-3b-instruct",
                &none,
                "gemma3:4b",
                Some(&gemma4),
                true,
                &m
            ),
            Some(LocalAiAdopt::Ollama("gemma4:e2b-it-qat".into()))
        );
        // Выбранная на месте - не трогаем (llama3 == llama3:latest).
        assert_eq!(
            plan_local_ai(
                "ollama",
                "x",
                &none,
                "llama3",
                Some(&tags(&["llama3:latest"])),
                true,
                &m
            ),
            None
        );
        let ready = tags(&["qwen3-1.7b", "qwen3-4b"]);
        assert_eq!(
            plan_local_ai("builtin", "qwen3-1.7b", &ready, "", None, true, &m),
            None
        );
        // Другая встроенная - важнее Ollama, берём самую крупную.
        assert_eq!(
            plan_local_ai("builtin", "qwen3-8b", &ready, "", Some(&gemma4), true, &m),
            Some(LocalAiAdopt::Builtin("qwen3-4b".into()))
        );
        // Ollama жива, но пуста - на встроенную; не ответила - оставляем.
        assert_eq!(
            plan_local_ai("ollama", "", &ready, "qwen3:4b", Some(&none), true, &m),
            Some(LocalAiAdopt::Builtin("qwen3-4b".into()))
        );
        assert_eq!(
            plan_local_ai("ollama", "", &ready, "qwen3:4b", None, true, &m),
            None
        );
        // Удалённая Ollama, облако, выключенный ИИ и пустая машина - не трогаем.
        assert_eq!(
            plan_local_ai("ollama", "", &ready, "q", Some(&none), false, &m),
            None
        );
        assert_eq!(
            plan_local_ai("builtin", "q", &none, "", Some(&gemma4), false, &m),
            None
        );
        assert_eq!(
            plan_local_ai("gemini", "q", &ready, "", Some(&gemma4), true, &m),
            None
        );
        assert_eq!(
            plan_local_ai("off", "q", &ready, "", Some(&gemma4), true, &m),
            None
        );
        assert_eq!(
            plan_local_ai("builtin", "q", &none, "", None, true, &m),
            None
        );
    }
}
