//! Единая фабрика сессий onnxruntime для GigaAM, Parakeet и Silero VAD.
//! Зачем: опции памяти (D-053) живут в одном месте, иначе три загрузчика
//! разойдутся и одна из моделей снова начнёт держать лишнюю копию весов.
//!
//! Замер D-053: prepacking в int8-энкодере Parakeet держит вторую, упакованную
//! копию весов (~890 МБ). Без неё распознавание Parakeet медленнее на 13-15%,
//! тексты те же. У GigaAM fp32 prepacking памяти не добавляет и ускоряет - он
//! остаётся. Остальные опции (memory pattern, arena, env/device allocators)
//! памяти не сэкономили - не включены.

use std::path::Path;

use anyhow::Result;
use ort::session::Session;

use crate::gigaam::OrtCtx;

/// Файлы, в которых prepacking выключен (D-053).
const NO_PREPACK_FILES: &[&str] = &["encoder-model.int8.onnx"];

fn prepacking_wanted(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    !NO_PREPACK_FILES.contains(&name)
}

/// Сессия ORT из файла модели: CPU, `threads` потоков внутри операторов.
pub fn session_from_file(path: &Path, threads: usize) -> Result<Session> {
    let mut b = Session::builder()
        .oc("builder")?
        .with_intra_threads(threads.max(1))
        .oc("intra_threads")?;
    if !prepacking_wanted(path) {
        b = b.with_prepacking(false).oc("prepacking")?;
    }
    #[cfg(test)]
    {
        let only = std::env::var("VOXFLOW_ORT_ONLY").unwrap_or_default();
        if path.to_string_lossy().contains(only.as_str()) {
            b = bench_options(b)?;
        }
    }
    b.commit_from_file(path)
        .oc(&format!("загрузка {}", path.display()))
}

/// Только для замера D-053: опции из `VOXFLOW_ORT_OPTS` через запятую.
#[cfg(test)]
fn bench_options(
    mut b: ort::session::builder::SessionBuilder,
) -> Result<ort::session::builder::SessionBuilder> {
    let opts = BENCH_OPTS
        .with(|o| o.borrow().clone())
        .unwrap_or_else(|| std::env::var("VOXFLOW_ORT_OPTS").unwrap_or_default());
    for o in opts.split(',').filter(|o| !o.is_empty()) {
        b = match o {
            "noprepack" => b.with_prepacking(false).oc(o)?,
            "nomempat" => b.with_memory_pattern(false).oc(o)?,
            "noarena" => b
                .with_execution_providers([ort::ep::CPU::default()
                    .with_arena_allocator(false)
                    .build()])
                .oc(o)?,
            "devinit" => b.with_device_allocated_initializers().oc(o)?,
            "envalloc" => b.with_env_allocators().oc(o)?,
            other => panic!("неизвестная опция {other}"),
        };
    }
    Ok(b)
}

#[cfg(test)]
thread_local! {
    static BENCH_OPTS: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Только для замера D-053: собрать сессии внутри `f` с опциями `opts`.
#[cfg(test)]
pub(crate) fn with_bench_opts<R>(opts: &str, f: impl FnOnce() -> R) -> R {
    BENCH_OPTS.with(|o| *o.borrow_mut() = Some(opts.to_string()));
    let r = f();
    BENCH_OPTS.with(|o| *o.borrow_mut() = None);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepacking_is_off_only_for_parakeet_encoder() {
        let models = Path::new("/m");
        assert!(!prepacking_wanted(
            &models.join("parakeet/encoder-model.int8.onnx")
        ));
        assert!(prepacking_wanted(
            &models.join("parakeet/decoder_joint-model.int8.onnx")
        ));
        assert!(prepacking_wanted(
            &models.join("gigaam-fp32/v3_e2e_rnnt_encoder.onnx")
        ));
        assert!(prepacking_wanted(&models.join("vad/silero_vad.onnx")));
    }
}
