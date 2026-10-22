//! Единая фабрика сессий onnxruntime для GigaAM, Parakeet и Silero VAD.
//! Зачем: опции памяти (D-053) живут в одном месте, иначе три загрузчика
//! разойдутся и одна из моделей снова начнёт держать лишнюю копию весов.
//!
//! Замер D-053: prepacking в int8-энкодере Parakeet держит вторую, упакованную
//! копию весов (~890 МБ). Без неё распознавание Parakeet медленнее на 13-15%,
//! тексты те же. У GigaAM fp32 prepacking памяти не добавляет и ускоряет - он
//! остаётся. Остальные опции (memory pattern, arena, env/device allocators)
//! памяти не сэкономили - не включены.
//!
//! Веса из файла (D-053): ORT отображает в память только внешние данные
//! (`.onnx` + external data), а цельный `.onnx` копирует в кучу процесса.
//! Поэтому модель отдаётся ORT как маленький граф в памяти, где каждый крупный
//! тензор весов - «внешние данные» по смещению внутри того же исходного файла.
//! Файл на диске не меняется (SHA и каталог те же), страницы весов чистые и
//! file-backed: не входят в phys_footprint и при нехватке памяти отдаются без свопа.

use std::io::{BufReader, Read, Seek};
use std::path::Path;

use anyhow::{anyhow, bail, ensure, Context, Result};
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
    let what = format!("загрузка {}", path.display());
    if !MAP_WEIGHTS {
        return b.commit_from_file(path).oc(&what);
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let graph = external_view(path).with_context(|| what.clone())?;
    b.with_config_entry(
        "session.model_external_initializers_file_folder_path",
        dir.to_string_lossy(),
    )
    .oc("external_folder")?
    .commit_from_memory(&graph)
    .oc(&what)
}

/// Windows не даёт удалить или заменить файл, пока он отображён в память, а
/// `delete_model` удаляет файлы раньше, чем выгружает модель (D-053).
const MAP_WEIGHTS: bool = !cfg!(windows);

/// Тензоры меньше порога остаются в графе: отображать их нет смысла.
const MAP_MIN_BYTES: u64 = 4096;

/// Читатель protobuf поверх файла с учётом позиции: смещение `raw_data` в файле
/// и есть смещение внешних данных для ORT.
struct Pb<R> {
    r: BufReader<R>,
    pos: u64,
}

impl<R: Read + Seek> Pb<R> {
    fn byte(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.r.read_exact(&mut b)?;
        self.pos += 1;
        Ok(b[0])
    }

    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bail!("испорченный varint на {}", self.pos)
    }

    fn bytes(&mut self, n: u64, end: u64) -> Result<Vec<u8>> {
        ensure!(self.pos + n <= end, "поле выходит за конец сообщения");
        let mut v = vec![0u8; n as usize];
        self.r.read_exact(&mut v)?;
        self.pos += n;
        Ok(v)
    }

    fn skip(&mut self, n: u64, end: u64) -> Result<()> {
        ensure!(self.pos + n <= end, "поле выходит за конец сообщения");
        self.r.seek_relative(n as i64)?;
        self.pos += n;
        Ok(())
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_len(out: &mut Vec<u8>, field: u64, data: &[u8]) {
    put_varint(out, field << 3 | 2);
    put_varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

/// Уровни ONNX, которые переписываются: ModelProto.graph (7) →
/// GraphProto.initializer (5) → TensorProto.raw_data (9). Остальное копируется.
#[derive(Clone, Copy)]
enum Level {
    Model,
    Graph,
    Tensor,
}

fn rewrite<R: Read + Seek>(pb: &mut Pb<R>, end: u64, level: Level, file: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut external: Option<(u64, u64)> = None;
    while pb.pos < end {
        let key = pb.varint()?;
        let (field, wire) = (key >> 3, key & 7);
        match wire {
            0 => {
                let v = pb.varint()?;
                put_varint(&mut out, key);
                put_varint(&mut out, v);
            }
            1 | 5 => {
                let n = if wire == 1 { 8 } else { 4 };
                put_varint(&mut out, key);
                out.extend(pb.bytes(n, end)?);
            }
            2 => {
                let n = pb.varint()?;
                let inner = match (level, field) {
                    (Level::Model, 7) => Some(Level::Graph),
                    (Level::Graph, 5) => Some(Level::Tensor),
                    _ => None,
                };
                if let Some(inner) = inner {
                    let body = rewrite(pb, pb.pos + n, inner, file)?;
                    put_len(&mut out, field, &body);
                } else if matches!(level, Level::Tensor) && field == 9 && n >= MAP_MIN_BYTES {
                    external = Some((pb.pos, n));
                    pb.skip(n, end)?;
                } else {
                    put_varint(&mut out, key);
                    put_varint(&mut out, n);
                    out.extend(pb.bytes(n, end)?);
                }
            }
            _ => bail!("неизвестный тип поля {wire} на {}", pb.pos),
        }
    }
    ensure!(pb.pos == end, "сообщение не совпало по длине");
    if let Some((offset, length)) = external {
        // TensorProto.external_data (13) location/offset/length, data_location (14) = EXTERNAL.
        for (k, v) in [
            ("location", file.to_string()),
            ("offset", offset.to_string()),
            ("length", length.to_string()),
        ] {
            let mut entry = Vec::new();
            put_len(&mut entry, 1, k.as_bytes());
            put_len(&mut entry, 2, v.as_bytes());
            put_len(&mut out, 13, &entry);
        }
        put_varint(&mut out, 14 << 3);
        put_varint(&mut out, 1);
    }
    Ok(out)
}

/// Граф модели без крупных весов: они ссылаются на этот же файл как на внешние данные.
fn external_view(path: &Path) -> Result<Vec<u8>> {
    let file = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("имя файла модели не UTF-8"))?;
    let f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut pb = Pb {
        r: BufReader::with_capacity(1 << 16, f),
        pos: 0,
    };
    rewrite(&mut pb, len, Level::Model, file)
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

    const N: usize = 4096;

    fn tensor_type(out: &mut Vec<u8>) {
        let mut dim = Vec::new();
        put_varint(&mut dim, 1 << 3);
        put_varint(&mut dim, N as u64);
        let mut shape = Vec::new();
        put_len(&mut shape, 1, &dim);
        let mut tt = Vec::new();
        put_varint(&mut tt, 1 << 3);
        put_varint(&mut tt, 1); // FLOAT
        put_len(&mut tt, 2, &shape);
        let mut ty = Vec::new();
        put_len(&mut ty, 1, &tt);
        put_len(out, 2, &ty);
    }

    /// y = x + w, w - fp32-веса на 16 КБ, `b` - маленький тензор (y2 = x + b).
    fn add_model(w: &[f32]) -> Vec<u8> {
        let io = |name: &str| {
            let mut v = Vec::new();
            put_len(&mut v, 1, name.as_bytes());
            tensor_type(&mut v);
            v
        };
        let init = |name: &str, dims: u64, data: &[u8]| {
            let mut t = Vec::new();
            put_varint(&mut t, 1 << 3);
            put_varint(&mut t, dims);
            put_varint(&mut t, 2 << 3);
            put_varint(&mut t, 1);
            put_len(&mut t, 8, name.as_bytes());
            put_len(&mut t, 9, data);
            t
        };
        let raw: Vec<u8> = w.iter().flat_map(|f| f.to_le_bytes()).collect();
        let mut g = Vec::new();
        for (a, b, y) in [("x", "w", "y"), ("x", "b", "y2")] {
            let mut node = Vec::new();
            put_len(&mut node, 1, a.as_bytes());
            put_len(&mut node, 1, b.as_bytes());
            put_len(&mut node, 2, y.as_bytes());
            put_len(&mut node, 4, b"Add");
            put_len(&mut g, 1, &node);
        }
        put_len(&mut g, 2, b"g");
        put_len(&mut g, 5, &init("w", N as u64, &raw));
        put_len(&mut g, 5, &init("b", 1, &2.5f32.to_le_bytes()));
        put_len(&mut g, 11, &io("x"));
        put_len(&mut g, 12, &io("y"));
        put_len(&mut g, 12, &io("y2"));
        let mut m = Vec::new();
        put_varint(&mut m, 1 << 3);
        put_varint(&mut m, 8);
        put_len(&mut m, 7, &g);
        let mut opset = Vec::new();
        put_varint(&mut opset, 2 << 3);
        put_varint(&mut opset, 13);
        put_len(&mut m, 8, &opset);
        m
    }

    fn tmp_model(tag: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vf-ort-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model.onnx");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn big_weights_become_external_data_of_the_same_file() {
        let w: Vec<f32> = (0..N).map(|i| i as f32 * 0.5).collect();
        let bytes = add_model(&w);
        let path = tmp_model("view", &bytes);
        let graph = external_view(&path).unwrap();
        // Веса (16 КБ) ушли из графа, маленький тензор остался.
        assert!(graph.len() < 1024, "граф {} байт", graph.len());
        assert!(graph.windows(4).any(|c| c == 2.5f32.to_le_bytes()));

        let mut s = session_from_file(&path, 1).unwrap();
        let x: Vec<f32> = (0..N).map(|i| i as f32).collect();
        let out = s
            .run(ort::inputs!["x" => ort::value::Tensor::from_array(([N], x.clone())).unwrap()])
            .unwrap();
        let (_, y) = out["y"].try_extract_tensor::<f32>().unwrap();
        let (_, y2) = out["y2"].try_extract_tensor::<f32>().unwrap();
        assert!((0..N).all(|i| y[i] == x[i] + w[i] && y2[i] == x[i] + 2.5));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn truncated_model_is_an_error_not_a_crash() {
        let bytes = add_model(&vec![1.0; N]);
        let path = tmp_model("cut", &bytes[..bytes.len() - 100]);
        assert!(session_from_file(&path, 1).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
