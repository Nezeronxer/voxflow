//! SQLite (rusqlite bundled): настройки (kv), словарь, сниппеты, история, статистика.

use anyhow::Result;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS dictionary (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    term        TEXT NOT NULL,
    replacement TEXT NOT NULL DEFAULT '',
    sounds_like TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS snippets (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    trigger     TEXT NOT NULL UNIQUE,
    content     TEXT NOT NULL,
    is_template INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS history (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    ts    TEXT NOT NULL,
    text  TEXT NOT NULL,
    app   TEXT NOT NULL DEFAULT '',
    words INTEGER NOT NULL DEFAULT 0,
    ms    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS stats (
    day      TEXT PRIMARY KEY,
    words    INTEGER NOT NULL DEFAULT 0,
    sessions INTEGER NOT NULL DEFAULT 0,
    ms       INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS samples (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    ts    TEXT NOT NULL,
    audio TEXT NOT NULL DEFAULT '',
    text  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS corrections (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    wrong TEXT NOT NULL,
    right TEXT NOT NULL,
    hits  INTEGER NOT NULL DEFAULT 1
);
"#;

/// Записать выученное исправление (распознано → правильно). Дубликаты усиливают вес.
pub fn add_correction(conn: &Connection, wrong: &str, right: &str) -> Result<()> {
    let wrong = wrong.trim();
    let right = right.trim();
    if wrong.is_empty() || right.is_empty() || wrong.eq_ignore_ascii_case(right) {
        return Ok(());
    }
    // если такая пара уже есть — увеличить вес, иначе вставить
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM corrections WHERE lower(wrong)=lower(?1) AND lower(right)=lower(?2)",
            rusqlite::params![wrong, right],
            |r| r.get(0),
        )
        .ok();
    match existing {
        Some(id) => {
            conn.execute("UPDATE corrections SET hits=hits+1 WHERE id=?1", [id])?;
        }
        None => {
            conn.execute(
                "INSERT INTO corrections(wrong,right) VALUES(?1,?2)",
                rusqlite::params![wrong, right],
            )?;
        }
    }
    Ok(())
}

pub fn open() -> Result<Connection> {
    open_at(&crate::paths::db_path())
}

/// Открыть БД СТРОГО read-only — для диагностических CLI-путей (селфтесты).
/// Никогда не трогает пользовательский файл: без создания БД, без recovery,
/// без карантина, без смены journal_mode (read-only поверх существующего WAL
/// допустим). Любая проблема (нет файла, malformed, неподнимаемый WAL) = Err;
/// что делать дальше — решает вызывающий (селфтесты уходят на дефолты).
/// Инцидент 2026-06-11: --stream-selftest через open() заквантинил битую
/// voxflow.db и пересоздал её — настройки пользователя были сброшены.
pub fn open_readonly() -> Result<Connection> {
    open_readonly_at(&crate::paths::db_path())
}

fn open_readonly_at(path: &Path) -> Result<Connection> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // Подождать снятия блокировки, если GUI-процесс параллельно держит БД.
    let _ = conn.busy_timeout(Duration::from_millis(5000));
    // SQLite открывает файл лениво: мусор/повреждение всплывают только на первом
    // запросе. Проверяем здесь, чтобы вызывающий получил честный Err, а не тихо
    // «пустую» БД. Read-only: ни чинить, ни уносить в карантин нельзя.
    let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok" {
        anyhow::bail!("quick_check (read-only): {check}");
    }
    Ok(conn)
}

/// Открыть БД по пути с самовосстановлением: если файл не открывается или не
/// проходит quick_check (например, «database disk image is malformed» после
/// жёсткого убийства процесса) — убрать его в карантин рядом
/// (voxflow.db.corrupt-<unix_ts>) и создать свежую БД. Приложение обязано
/// подниматься всегда, паника на старте недопустима.
fn open_at(path: &Path) -> Result<Connection> {
    let conn = match try_open(path) {
        Ok(conn) => Ok(conn),
        Err(e) => {
            quarantine(path, &e);
            try_open(path) // повторная попытка на чистом месте
        }
    }?;
    // This database contains transcripts and API keys. Tighten both a newly
    // created file and legacy files that may have inherited a permissive umask.
    // Permission errors must not be treated as corruption (and must therefore
    // never quarantine an otherwise healthy database).
    secure_database_files(path)?;
    Ok(conn)
}

/// Открытие + прагмы + проверка целостности + схема. Любая ошибка = файл под подозрением.
fn try_open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // Устойчивость к конкурентным записям и жёстким убийствам: WAL (журнал
    // переживает kill посреди транзакции), synchronous=NORMAL (достаточно для
    // WAL), ждать освобождения блокировки до 5 с вместо мгновенного SQLITE_BUSY.
    let _ = conn.busy_timeout(Duration::from_millis(5000));
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    let _ = conn.pragma_update(None, "synchronous", "NORMAL");
    // Быстрая проверка целостности: битый файл ловим здесь, а не паникой позже.
    let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok" {
        anyhow::bail!("quick_check: {check}");
    }
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
    // при ошибке conn дропается здесь же → файл закрыт, rename в quarantine() возможен
}

/// Путь-сосед: тот же файл с дописанным суффиксом (voxflow.db → voxflow.db-wal и т.п.).
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn secure_database_files(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let candidate = sibling(path, suffix);
        if !candidate.exists() {
            continue;
        }
        match crate::paths::set_private_file_permissions(&candidate) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => anyhow::bail!("не удалось защитить {}: {e}", candidate.display()),
        }
    }
    Ok(())
}

/// Убрать битую БД в карантин: voxflow.db.corrupt-<unix_ts>. Sidecar'ы -wal/-shm
/// тоже уносим — оставлять старый WAL-журнал рядом со свежим файлом нельзя.
fn quarantine(path: &Path, err: &anyhow::Error) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for suffix in ["", "-wal", "-shm"] {
        let src = sibling(path, suffix);
        if !src.exists() {
            continue;
        }
        let dst = sibling(&src, &format!(".corrupt-{ts}"));
        if std::fs::rename(&src, &dst).is_err() {
            let _ = std::fs::remove_file(&src); // rename не удался — хотя бы убрать с дороги
        } else if let Err(permission_error) = crate::paths::set_private_file_permissions(&dst) {
            log::warn!(
                "не удалось защитить файл карантина {}: {permission_error}",
                dst.display()
            );
        }
    }
    log::warn!(
        "БД повреждена ({err}); файл убран в карантин: {}",
        sibling(path, &format!(".corrupt-{ts}")).display()
    );
}

pub fn kv_get(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0))
        .ok()
}

pub fn kv_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO kv(key,value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// Записать диктовку в историю и обновить дневную статистику.
pub fn record_dictation(
    conn: &Connection,
    text: &str,
    app: &str,
    words: u32,
    ms: u64,
) -> Result<()> {
    let now = chrono::Local::now();
    let ts = now.format("%Y-%m-%d %H:%M:%S").to_string();
    let day = now.format("%Y-%m-%d").to_string();
    conn.execute(
        "INSERT INTO history(ts,text,app,words,ms) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![ts, text, app, words, ms as i64],
    )?;
    conn.execute(
        "INSERT INTO stats(day,words,sessions,ms) VALUES(?1,?2,1,?3)
         ON CONFLICT(day) DO UPDATE SET
            words=words+?2, sessions=sessions+1, ms=ms+?3",
        rusqlite::params![day, words, ms as i64],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Уникальный tmp-путь под БД (без tempfile в dev-deps обходимся std).
    fn tmp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("voxflow-db-tests");
        let _ = std::fs::create_dir_all(&dir);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        dir.join(format!("{name}-{}-{nanos}.db", std::process::id()))
    }

    /// Есть ли рядом с path файл-карантин <имя>.corrupt-*.
    fn has_corrupt_sibling(path: &Path) -> bool {
        let prefix = format!("{}.corrupt-", path.file_name().unwrap().to_string_lossy());
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with(&prefix))
    }

    #[test]
    fn open_at_recovers_from_garbage() {
        let p = tmp_db("garbage");
        std::fs::write(&p, b"this is definitely not an sqlite database, just junk").unwrap();
        let conn = open_at(&p).expect("после карантина должно открыться");
        // соединение рабочее: схема создана, kv пишется и читается
        kv_set(&conn, "k", "v").unwrap();
        assert_eq!(kv_get(&conn, "k").as_deref(), Some("v"));
        // мусор уехал в карантин рядом
        assert!(has_corrupt_sibling(&p), "нет файла-карантина *.corrupt-*");
    }

    #[test]
    fn open_readonly_at_garbage_untouched() {
        let p = tmp_db("ro-garbage");
        let junk: &[u8] = b"this is definitely not an sqlite database, just junk";
        std::fs::write(&p, junk).unwrap();
        assert!(
            open_readonly_at(&p).is_err(),
            "мусорный файл не должен открываться read-only"
        );
        // файл цел байт-в-байт: ни карантина, ни пересоздания, ни sidecar'ов
        assert_eq!(
            std::fs::read(&p).unwrap(),
            junk,
            "read-only путь изменил файл"
        );
        assert!(
            !has_corrupt_sibling(&p),
            "read-only путь унёс файл в карантин"
        );
        assert!(!sibling(&p, "-wal").exists(), "read-only путь создал -wal");
    }

    #[test]
    fn open_readonly_at_missing_file_not_created() {
        let p = tmp_db("ro-missing");
        assert!(
            open_readonly_at(&p).is_err(),
            "несуществующая БД не должна открываться"
        );
        assert!(!p.exists(), "read-only открытие создало файл БД");
    }

    #[test]
    fn open_readonly_at_reads_but_rejects_writes() {
        let p = tmp_db("ro-read");
        {
            let conn = open_at(&p).expect("создать здоровую БД");
            kv_set(&conn, "k", "v").unwrap();
        }
        let conn = open_readonly_at(&p).expect("здоровая БД должна читаться");
        assert_eq!(kv_get(&conn, "k").as_deref(), Some("v"));
        assert!(
            kv_set(&conn, "k", "w").is_err(),
            "запись через read-only соединение прошла"
        );
        // и после попытки записи значение не изменилось
        assert_eq!(kv_get(&conn, "k").as_deref(), Some("v"));
    }

    #[test]
    fn open_at_normal_db_is_wal() {
        let p = tmp_db("wal");
        let conn = open_at(&p).expect("свежая БД");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        // здоровая БД при повторном открытии в карантин НЕ уезжает
        kv_set(&conn, "keep", "me").unwrap();
        drop(conn);
        let conn2 = open_at(&p).expect("повторное открытие");
        assert_eq!(kv_get(&conn2, "keep").as_deref(), Some("me"));
        assert!(!has_corrupt_sibling(&p), "здоровую БД унесло в карантин");
    }

    #[cfg(unix)]
    #[test]
    fn writable_database_and_sidecars_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let p = tmp_db("private");
        let conn = open_at(&p).expect("свежая БД");
        kv_set(&conn, "secret", "value").unwrap();

        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        for suffix in ["-wal", "-shm"] {
            let sidecar = sibling(&p, suffix);
            if sidecar.exists() {
                let mode = std::fs::metadata(&sidecar).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "неприватный sidecar: {}", sidecar.display());
            }
        }
    }
}
