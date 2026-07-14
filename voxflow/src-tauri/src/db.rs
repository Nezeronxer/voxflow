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

fn normalize_inline(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalized_key(value: &str) -> String {
    normalize_inline(value).to_lowercase()
}

fn snippet_key(value: &str) -> String {
    normalize_inline(value)
        .trim_start_matches('/')
        .trim()
        .to_lowercase()
}

/// Записать выученное исправление (распознано → правильно). Unicode/пробельные
/// дубликаты усиливают вес одной строки; новое исправление того же `wrong`
/// заменяет старое неоднозначное правило.
pub fn add_correction(conn: &Connection, wrong: &str, right: &str) -> Result<()> {
    let wrong = normalize_inline(wrong);
    let right = normalize_inline(right);
    if wrong.is_empty() || right.is_empty() || wrong == right {
        return Ok(());
    }

    let wrong_key = normalized_key(&wrong);
    let right_key = normalized_key(&right);
    let mut stmt = conn.prepare("SELECT id,wrong,right,hits FROM corrections ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut same_wrong = Vec::new();
    for row in rows {
        let (id, stored_wrong, stored_right, hits) = row?;
        if normalized_key(&stored_wrong) == wrong_key {
            same_wrong.push((id, normalized_key(&stored_right), hits));
        }
    }
    drop(stmt);

    if same_wrong.is_empty() {
        conn.execute(
            "INSERT INTO corrections(wrong,right) VALUES(?1,?2)",
            rusqlite::params![wrong, right],
        )?;
        return Ok(());
    }

    let keep = same_wrong
        .iter()
        .find(|(_, stored_right, _)| *stored_right == right_key)
        .unwrap_or(&same_wrong[0]);
    let (keep_id, keep_right, keep_hits) = (keep.0, keep.1.clone(), keep.2);
    let next_hits = if keep_right == right_key {
        keep_hits + 1
    } else {
        1
    };
    conn.execute(
        "UPDATE corrections SET wrong=?1,right=?2,hits=?3 WHERE id=?4",
        rusqlite::params![wrong, right, next_hits, keep_id],
    )?;
    for (id, _, _) in same_wrong {
        if id != keep_id {
            conn.execute("DELETE FROM corrections WHERE id=?1", [id])?;
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

/// Save a dictionary preference. An empty replacement means "prefer this
/// spelling", never "delete this word". Unicode/case duplicates are folded
/// into the oldest row so the UI cannot create ambiguous entries.
pub fn upsert_dictionary(
    conn: &Connection,
    id: Option<i64>,
    term: &str,
    replacement: &str,
) -> Result<()> {
    let term = normalize_inline(term);
    if term.is_empty() {
        anyhow::bail!("Термин словаря не может быть пустым");
    }
    let replacement = normalize_inline(replacement);
    let replacement = if replacement.is_empty() {
        term.clone()
    } else {
        replacement
    };
    let tx = conn.unchecked_transaction()?;

    if let Some(id) = id {
        let exists = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM dictionary WHERE id=?1)",
            [id],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !exists {
            anyhow::bail!("Запись словаря не найдена");
        }
        // Editing an existing row can rename it onto another Unicode/case
        // variant. Keep the row the UI edited and remove the now-ambiguous
        // siblings just as the insert path does.
        let key = normalized_key(&term);
        let mut stmt = tx.prepare("SELECT id,term FROM dictionary WHERE id<>?1 ORDER BY id")?;
        let rows = stmt.query_map([id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut duplicates = Vec::new();
        for row in rows {
            let (existing_id, existing_term) = row?;
            if normalized_key(&existing_term) == key {
                duplicates.push(existing_id);
            }
        }
        drop(stmt);
        // Delete normalized siblings before UPDATE: one of them may already
        // have the exact target spelling protected by SQL UNIQUE.
        for duplicate in duplicates {
            tx.execute("DELETE FROM dictionary WHERE id=?1", [duplicate])?;
        }
        tx.execute(
            "UPDATE dictionary SET term=?1,replacement=?2 WHERE id=?3",
            rusqlite::params![&term, &replacement, id],
        )?;
        tx.commit()?;
        return Ok(());
    }

    let key = normalized_key(&term);
    let mut stmt = tx.prepare("SELECT id,term FROM dictionary ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut matching_ids = Vec::new();
    for row in rows {
        let (existing_id, existing_term) = row?;
        if normalized_key(&existing_term) == key {
            matching_ids.push(existing_id);
        }
    }
    drop(stmt);

    if let Some((&keep, duplicates)) = matching_ids.split_first() {
        for duplicate in duplicates {
            tx.execute("DELETE FROM dictionary WHERE id=?1", [duplicate])?;
        }
        tx.execute(
            "UPDATE dictionary SET term=?1,replacement=?2 WHERE id=?3",
            rusqlite::params![term, replacement, keep],
        )?;
    } else {
        tx.execute(
            "INSERT INTO dictionary(term,replacement) VALUES(?1,?2)",
            rusqlite::params![term, replacement],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// `/адрес` and `адрес` are the same spoken trigger. Repeated saves update one
/// row; empty bodies are rejected before reaching SQLite.
pub fn upsert_snippet(
    conn: &Connection,
    id: Option<i64>,
    trigger: &str,
    content: &str,
    is_template: bool,
) -> Result<()> {
    let trigger = normalize_inline(trigger);
    if snippet_key(&trigger).is_empty() {
        anyhow::bail!("Триггер сниппета не может быть пустым");
    }
    if content.trim().is_empty() {
        anyhow::bail!("Содержимое сниппета не может быть пустым");
    }
    let flag = i64::from(is_template);
    let tx = conn.unchecked_transaction()?;

    if let Some(id) = id {
        let exists = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM snippets WHERE id=?1)",
            [id],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !exists {
            anyhow::bail!("Сниппет не найден");
        }
        let key = snippet_key(&trigger);
        let mut stmt = tx.prepare("SELECT id,trigger FROM snippets WHERE id<>?1 ORDER BY id")?;
        let rows = stmt.query_map([id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut duplicates = Vec::new();
        for row in rows {
            let (existing_id, existing_trigger) = row?;
            if snippet_key(&existing_trigger) == key {
                duplicates.push(existing_id);
            }
        }
        drop(stmt);
        // A normalized sibling may already own this exact trigger under the
        // SQL UNIQUE constraint. Remove conflicts first, then update atomically.
        for duplicate in duplicates {
            tx.execute("DELETE FROM snippets WHERE id=?1", [duplicate])?;
        }
        tx.execute(
            "UPDATE snippets SET trigger=?1,content=?2,is_template=?3 WHERE id=?4",
            rusqlite::params![&trigger, content, flag, id],
        )?;
        tx.commit()?;
        return Ok(());
    }

    let key = snippet_key(&trigger);
    let mut stmt = tx.prepare("SELECT id,trigger FROM snippets ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut matching_ids = Vec::new();
    for row in rows {
        let (existing_id, existing_trigger) = row?;
        if snippet_key(&existing_trigger) == key {
            matching_ids.push(existing_id);
        }
    }
    drop(stmt);

    if let Some((&keep, duplicates)) = matching_ids.split_first() {
        for duplicate in duplicates {
            tx.execute("DELETE FROM snippets WHERE id=?1", [duplicate])?;
        }
        tx.execute(
            "UPDATE snippets SET trigger=?1,content=?2,is_template=?3 WHERE id=?4",
            rusqlite::params![trigger, content, flag, keep],
        )?;
    } else {
        tx.execute(
            "INSERT INTO snippets(trigger,content,is_template) VALUES(?1,?2,?3)",
            rusqlite::params![trigger, content, flag],
        )?;
    }
    tx.commit()?;
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

    #[test]
    fn repeated_unicode_correction_is_one_weighted_rule() {
        let p = tmp_db("correction-dedupe");
        let conn = open_at(&p).expect("создать БД");
        add_correction(&conn, "  Виспа   Фолл ", "Wispr Flow").unwrap();
        add_correction(&conn, "виспа фолл", "wispr flow").unwrap();

        let row: (i64, i64) = conn
            .query_row("SELECT count(*),max(hits) FROM corrections", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(row, (1, 2));
    }

    #[test]
    fn case_only_brand_correction_is_not_discarded() {
        let p = tmp_db("correction-case");
        let conn = open_at(&p).expect("создать БД");
        add_correction(&conn, "wispr flow", "Wispr Flow").unwrap();

        let row: (String, String) = conn
            .query_row("SELECT wrong,right FROM corrections", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(row, ("wispr flow".into(), "Wispr Flow".into()));
    }

    #[test]
    fn dictionary_upsert_preserves_blank_replacement_and_dedupes_unicode() {
        let p = tmp_db("dictionary-upsert");
        let conn = open_at(&p).expect("создать БД");
        upsert_dictionary(&conn, None, "  виспр   флоу  ", "").unwrap();
        upsert_dictionary(&conn, None, "ВИСПР ФЛОУ", "Wispr Flow").unwrap();

        let row: (i64, String) = conn
            .query_row("SELECT count(*),replacement FROM dictionary", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(row, (1, "Wispr Flow".into()));
    }

    #[test]
    fn dictionary_edit_cannot_create_a_duplicate_key() {
        let p = tmp_db("dictionary-edit-dedupe");
        let conn = open_at(&p).expect("создать БД");
        upsert_dictionary(&conn, None, "Alpha", "A").unwrap();
        upsert_dictionary(&conn, None, "Beta", "B").unwrap();
        let beta_id: i64 = conn
            .query_row("SELECT id FROM dictionary WHERE term='Beta'", [], |row| {
                row.get(0)
            })
            .unwrap();

        upsert_dictionary(&conn, Some(beta_id), "  Alpha ", "Preferred").unwrap();

        let row: (i64, i64, String) = conn
            .query_row(
                "SELECT count(*),max(id),replacement FROM dictionary",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (1, beta_id, "Preferred".into()));
    }

    #[test]
    fn snippet_upsert_dedupes_spoken_slash_and_rejects_empty_body() {
        let p = tmp_db("snippet-upsert");
        let conn = open_at(&p).expect("создать БД");
        upsert_snippet(&conn, None, "/Адрес", "Москва", false).unwrap();
        upsert_snippet(&conn, None, "адрес", "Казань", true).unwrap();

        let row: (i64, String, i64) = conn
            .query_row(
                "SELECT count(*),content,is_template FROM snippets",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (1, "Казань".into(), 1));
        assert!(upsert_snippet(&conn, None, "/пусто", "  ", false).is_err());
    }

    #[test]
    fn snippet_upsert_resolves_legacy_exact_unique_collision_in_any_order() {
        for (name, first, second) in [
            ("plain-first", "foo", "/foo"),
            ("slash-first", "/foo", "foo"),
        ] {
            let p = tmp_db(&format!("snippet-legacy-collision-{name}"));
            let conn = open_at(&p).expect("создать БД");
            conn.execute(
                "INSERT INTO snippets(trigger,content,is_template) VALUES(?1,'old-1',0)",
                [first],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO snippets(trigger,content,is_template) VALUES(?1,'old-2',0)",
                [second],
            )
            .unwrap();

            upsert_snippet(&conn, None, "/foo", "updated", true).unwrap();

            let row: (i64, String, String, i64) = conn
                .query_row(
                    "SELECT count(*),trigger,content,is_template FROM snippets",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(row, (1, "/foo".into(), "updated".into(), 1), "{name}");
        }
    }

    #[test]
    fn snippet_edit_cannot_create_a_spoken_trigger_duplicate() {
        let p = tmp_db("snippet-edit-dedupe");
        let conn = open_at(&p).expect("создать БД");
        upsert_snippet(&conn, None, "/address", "First", false).unwrap();
        upsert_snippet(&conn, None, "/email", "Second", false).unwrap();
        let email_id: i64 = conn
            .query_row(
                "SELECT id FROM snippets WHERE trigger='/email'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        upsert_snippet(&conn, Some(email_id), "/address", "Updated", true).unwrap();

        let row: (i64, i64, String, i64) = conn
            .query_row(
                "SELECT count(*),max(id),content,is_template FROM snippets",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(row, (1, email_id, "Updated".into(), 1));
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
