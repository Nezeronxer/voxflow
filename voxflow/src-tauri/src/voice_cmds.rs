//! Голосовые команды редактирования финального текста диктовки
//! («с новой строки», «абзац», «удали последнее предложение», «отмена»).
//!
//! Чистые функции без состояния: вызываются из финального пайплайна
//! `engine.rs` ПОСЛЕ постобработки, ПЕРЕД вставкой. Команда распознаётся
//! только как замыкающий фрагмент фразы — слова внутри обычной речи
//! («я начну с новой строки письма») командой не считаются.
//!
//! Замыкающий фрагмент = команда стоит в самом конце текста и отделена
//! от остального пунктуацией (запятая/точка и т.п.) или началом строки.
//! Разделение одним пробелом («добавь сюда новая строка») командой
//! НЕ считается — сомнительный случай трактуем как обычную речь.

/// Результат применения голосовых команд к финальному тексту.
#[derive(Debug, PartialEq)]
pub enum CmdOutcome {
    /// Текст после применения команд (возможно, без изменений).
    Text(String),
    /// Команда «отмена» — вставлять ничего не нужно.
    Cancel,
}

/// Внутренний вид команды; в порядке произнесения применяются по очереди.
#[derive(Debug, Clone, Copy)]
enum Cmd {
    /// «с новой строки» / «новая строка» → текст завершается "\n"
    /// (склейка со следующей диктовкой — на стороне вставки).
    Newline,
    /// «абзац» / «новый абзац» / «с нового абзаца» → завершение "\n\n".
    Paragraph,
    /// «удали/убери последнее предложение».
    DeleteLastSentence,
    /// «отмена» / «отменить» → вся вставка отменяется.
    Cancel,
}

/// Таблица фраз. Порядок важен: более длинные фразы раньше своих
/// суффиксов («новый абзац» раньше «абзац»), иначе короткий вариант
/// перехватит совпадение и забракует его по разделителю.
const COMMANDS: &[(&str, Cmd)] = &[
    ("удали последнее предложение", Cmd::DeleteLastSentence),
    ("убери последнее предложение", Cmd::DeleteLastSentence),
    ("с нового абзаца", Cmd::Paragraph),
    ("новый абзац", Cmd::Paragraph),
    ("с новой строки", Cmd::Newline),
    ("новая строка", Cmd::Newline),
    ("абзац", Cmd::Paragraph),
    ("отменить", Cmd::Cancel),
    ("отмена", Cmd::Cancel),
];

/// Применяет голосовые команды редактирования к финальному тексту диктовки.
/// Несколько команд подряд в хвосте применяются в порядке произнесения;
/// «отмена» в любом месте цепочки отменяет всю вставку.
#[allow(dead_code)] // потребитель подключается в engine.rs следующей волной
pub fn apply_voice_commands(text: &str) -> CmdOutcome {
    // Срезаем команды с хвоста: каждая итерация снимает одну замыкающую.
    let mut rest = text.to_string();
    let mut cmds: Vec<Cmd> = Vec::new();
    while let Some((head, cmd)) = strip_trailing_command(&rest) {
        cmds.push(cmd);
        rest = head;
    }
    if cmds.is_empty() {
        // Команд нет — текст возвращаем нетронутым (включая пунктуацию).
        return CmdOutcome::Text(text.to_string());
    }
    cmds.reverse(); // срезали с конца → разворачиваем в порядок произнесения
    let mut out = rest;
    for cmd in cmds {
        match cmd {
            Cmd::Cancel => return CmdOutcome::Cancel,
            // trim_end: при цепочке «с новой строки, абзац» побеждает
            // последняя суффикс-команда, а не сумма переводов строки.
            Cmd::Newline => out = format!("{}\n", out.trim_end()),
            Cmd::Paragraph => out = format!("{}\n\n", out.trim_end()),
            Cmd::DeleteLastSentence => out = delete_last_sentence(&out),
        }
    }
    CmdOutcome::Text(out)
}

/// Пытается снять одну замыкающую команду с конца текста.
/// Возвращает (текст без команды, команда) либо None.
fn strip_trailing_command(text: &str) -> Option<(String, Cmd)> {
    // Завершающая пунктуация самой команды («Отмена.», «абзац!») не мешает.
    let tail = trim_trailing_punct(text);
    if tail.is_empty() {
        return None;
    }
    for (phrase, cmd) in COMMANDS {
        if let Some(start) = ends_with_ci(tail, phrase) {
            let head = &tail[..start];
            if separated_ok(head) {
                // Висячую запятую-разделитель перед командой тоже убираем:
                // «Привет, с новой строки» → «Привет\n», а не «Привет,\n».
                let head = head.trim_end().trim_end_matches(',').trim_end();
                return Some((head.to_string(), *cmd));
            }
            // Совпадение по суффиксу без разделителя («это новый абзац») —
            // не команда; пробуем остальные фразы таблицы.
        }
    }
    None
}

/// Срезает хвостовую пунктуацию и пробелы (допустимое завершение команды).
fn trim_trailing_punct(s: &str) -> &str {
    s.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | '!' | '?' | '…'))
}

/// Заканчивается ли `haystack` фразой `needle` без учёта регистра.
/// Возвращает байтовый индекс начала совпадения в `haystack`.
fn ends_with_ci(haystack: &str, needle: &str) -> Option<usize> {
    let mut it = haystack.char_indices().rev();
    let mut start = haystack.len();
    for nc in needle.chars().rev() {
        let (idx, hc) = it.next()?;
        if !hc.to_lowercase().eq(nc.to_lowercase()) {
            return None;
        }
        start = idx;
    }
    Some(start)
}

/// Отделена ли команда от предшествующего текста: пунктуацией или началом
/// строки/текста. Один лишь пробел разделителем НЕ считается — иначе
/// «добавь сюда новая строка» превратилось бы в команду.
fn separated_ok(head: &str) -> bool {
    for c in head.chars().rev() {
        if c == ' ' || c == '\t' {
            continue;
        }
        return matches!(c, '\n' | '\r' | ',' | '.' | '!' | '?' | '…' | ';' | ':');
    }
    true // пустая голова — команда стоит с самого начала текста
}

/// Удаляет последнее предложение. Граница предложения — серия `.!?…`,
/// за которой идёт пробел и дальше есть содержимое: многоточие считается
/// одним терминатором, а «3.14» без пробела после точки границей не станет.
/// Если предложение одно (границ нет) — результат пустой.
fn delete_last_sentence(text: &str) -> String {
    let t = text.trim_end();
    let mut cut: Option<usize> = None;
    let mut prev_term = false;
    for (i, c) in t.char_indices() {
        if c.is_whitespace() && prev_term && t[i..].chars().any(|x| !x.is_whitespace()) {
            cut = Some(i);
        }
        prev_term = matches!(c, '.' | '!' | '?' | '…');
    }
    match cut {
        Some(i) => t[..i].trim_end().to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> CmdOutcome {
        CmdOutcome::Text(s.to_string())
    }

    // --- «с новой строки» / «новая строка» ---

    #[test]
    fn newline_after_comma() {
        assert_eq!(
            apply_voice_commands("Привет, с новой строки"),
            text("Привет\n")
        );
    }

    #[test]
    fn newline_after_period_with_trailing_punct() {
        assert_eq!(
            apply_voice_commands("Привет. Новая строка."),
            text("Привет.\n")
        );
    }

    #[test]
    fn newline_case_insensitive() {
        assert_eq!(
            apply_voice_commands("привет, С НОВОЙ СТРОКИ"),
            text("привет\n")
        );
    }

    #[test]
    fn newline_alone_whole_phrase() {
        // вся диктовка = команда: вставляем только перевод строки
        assert_eq!(apply_voice_commands("С новой строки"), text("\n"));
    }

    // --- «абзац» / «новый абзац» / «с нового абзаца» ---

    #[test]
    fn paragraph_bare_word() {
        assert_eq!(
            apply_voice_commands("Текст готов. Абзац."),
            text("Текст готов.\n\n")
        );
    }

    #[test]
    fn paragraph_new_paragraph() {
        assert_eq!(
            apply_voice_commands("Текст, новый абзац"),
            text("Текст\n\n")
        );
    }

    #[test]
    fn paragraph_from_new_paragraph() {
        assert_eq!(
            apply_voice_commands("Текст. С НОВОГО АБЗАЦА!"),
            text("Текст.\n\n")
        );
    }

    // --- «удали/убери последнее предложение» ---

    #[test]
    fn delete_last_sentence_basic() {
        assert_eq!(
            apply_voice_commands("Раз. Два. Удали последнее предложение."),
            text("Раз.")
        );
    }

    #[test]
    fn delete_last_sentence_single_sentence_gives_empty() {
        assert_eq!(
            apply_voice_commands("Только одно предложение, убери последнее предложение"),
            text("")
        );
    }

    #[test]
    fn delete_last_sentence_respects_ellipsis() {
        // многоточие — один терминатор: после среза команды удаляется «Это лишнее.»
        assert_eq!(
            apply_voice_commands("Подожди... Это лишнее. Удали последнее предложение"),
            text("Подожди...")
        );
    }

    #[test]
    fn delete_last_sentence_decimal_not_boundary() {
        // точка внутри числа границей предложения не считается
        assert_eq!(
            apply_voice_commands("Пи равно 3.14, удали последнее предложение"),
            text("")
        );
    }

    // --- «отмена» / «отменить» ---

    #[test]
    fn cancel_whole_phrase() {
        assert_eq!(apply_voice_commands("Отмена"), CmdOutcome::Cancel);
        assert_eq!(apply_voice_commands("отменить."), CmdOutcome::Cancel);
    }

    #[test]
    fn cancel_as_trailing_fragment() {
        assert_eq!(
            apply_voice_commands("Это всё неправильно, отмена"),
            CmdOutcome::Cancel
        );
    }

    // --- негативные: слова команды внутри обычной речи ---

    #[test]
    fn negative_newline_mid_phrase() {
        let s = "я начну с новой строки моего письма";
        assert_eq!(apply_voice_commands(s), text(s));
    }

    #[test]
    fn negative_space_only_separator_is_not_command() {
        // решение зафиксировано: без пунктуации перед командой — это речь
        let s = "добавь сюда новая строка";
        assert_eq!(apply_voice_commands(s), text(s));
    }

    #[test]
    fn negative_paragraph_as_object_of_speech() {
        let s = "это новый абзац";
        assert_eq!(apply_voice_commands(s), text(s));
        let s2 = "Прочитай последний абзац";
        assert_eq!(apply_voice_commands(s2), text(s2));
    }

    #[test]
    fn negative_cancel_mid_phrase() {
        let s = "он крикнул отмена и убежал";
        assert_eq!(apply_voice_commands(s), text(s));
    }

    #[test]
    fn negative_plain_text_untouched() {
        let s = "Просто обычный текст, без команд.";
        assert_eq!(apply_voice_commands(s), text(s));
    }

    #[test]
    fn negative_empty_input() {
        assert_eq!(apply_voice_commands(""), text(""));
    }

    // --- цепочки команд в хвосте ---

    #[test]
    fn chain_delete_then_paragraph() {
        assert_eq!(
            apply_voice_commands("Раз. Два. Удали последнее предложение, абзац."),
            text("Раз.\n\n")
        );
    }

    #[test]
    fn chain_cancel_wins() {
        assert_eq!(
            apply_voice_commands("Текст, с новой строки, отмена"),
            CmdOutcome::Cancel
        );
    }

    #[test]
    fn chain_last_suffix_command_wins() {
        // «с новой строки, абзац» — побеждает абзац, переводы строк не суммируются
        assert_eq!(
            apply_voice_commands("Текст, с новой строки, абзац"),
            text("Текст\n\n")
        );
    }
}
