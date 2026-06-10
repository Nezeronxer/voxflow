//! Детерминированная (offline, без LLM) постобработка распознанного текста:
//! сниппеты → паразиты → словарь → капитализация. Verbatim отключает всё.

use crate::settings::Settings;

#[derive(Clone, Debug)]
pub struct Dict {
    pub term: String,
    pub replacement: String,
}

#[derive(Clone, Debug)]
pub struct Snippet {
    pub trigger: String,
    pub content: String,
    pub is_template: bool,
}

/// Выученное исправление: распознано `wrong` → правильно `right`.
#[derive(Clone, Debug)]
pub struct Correction {
    pub wrong: String,
    pub right: String,
}

/// Применить выученные исправления (регистронезависимая замена подстрок).
pub fn apply_corrections(text: &str, corrections: &[Correction]) -> String {
    let mut t = text.to_string();
    for c in corrections {
        let from = c.wrong.trim();
        if from.is_empty() {
            continue;
        }
        t = replace_ci(&t, from, &c.right);
    }
    t
}

fn replace_ci(text: &str, from: &str, to: &str) -> String {
    let lower_text = text.to_lowercase();
    let lower_from = from.to_lowercase();
    // Если лоуэркейс меняет длину в байтах — не рискуем смещением индексов.
    if lower_from.is_empty() || lower_text.len() != text.len() {
        return text.to_string();
    }
    let mut out = String::new();
    let mut i = 0;
    while let Some(pos) = lower_text.get(i..).and_then(|s| s.find(&lower_from)) {
        let abs = i + pos;
        out.push_str(&text[i..abs]);
        out.push_str(to);
        i = abs + lower_from.len();
    }
    out.push_str(text.get(i..).unwrap_or(""));
    out
}

/// Одно-словные паразиты (сверяются по нижнему регистру, без пунктуации).
///
/// Сюда входят ТОЛЬКО однозначные звуки-хезитации и слова, почти всегда являющиеся
/// паразитами в речи. Намеренно убраны «значит», «собственно», «блин»: это валидные
/// содержательные слова («это значит…», «собственно говоря», эмоц. «блин») — их
/// удаление портило смысл (FILLERS_ONE drops valid Russian words).
const FILLERS_ONE: &[&str] = &[
    "эм", "эээ", "ээ", "мм", "ммм", "типа", "короче",
    "uh", "um", "umm", "uhh", "err", "hmm",
];
/// Многословные паразиты (подстрока, с границами-пробелами).
const FILLERS_MULTI: &[&str] = &[
    "как бы", "в общем", "это самое", "так сказать", "то есть как бы",
    "you know", "i mean", "sort of", "kind of",
];

pub fn process(text: &str, s: &Settings, dict: &[Dict], snippets: &[Snippet]) -> String {
    let mut t = text.trim().to_string();
    if t.is_empty() {
        return t;
    }

    // 1) Сниппет на всю фразу (триггер == произнесённое, без хвостовой пунктуации).
    let bare = t.trim_matches(|c: char| ".,!?…".contains(c) || c.is_whitespace());
    for sn in snippets {
        if !sn.trigger.trim().is_empty() && bare.eq_ignore_ascii_case(sn.trigger.trim()) {
            return if sn.is_template {
                expand_template(&sn.content)
            } else {
                sn.content.clone()
            };
        }
    }

    if s.verbatim {
        return t;
    }

    // 2) Паразиты.
    if s.remove_fillers {
        t = remove_fillers(&t);
    }

    // 3) Словарь (замены терминов, регистронезависимо по первому слову).
    for d in dict {
        if !d.term.trim().is_empty() {
            t = replace_word_ci(&t, d.term.trim(), &d.replacement);
        }
    }

    // 4) Капитализация + чистка пробелов.
    if s.auto_punct {
        t = capitalize_sentences(&t);
    }
    normalize_spaces(&t)
}

/// Хезитация-междометие: дефисная редупликация ОДНОЙ буквы («а-а», «э-э-э»,
/// «м-м») либо повтор одной буквы («ээ», «ммм», «ааа»). GigaAM-v3 e2e
/// транскрибирует такие звуки дословно — режем их здесь. Реальные слова из
/// разных букв («мы», «ум», «что-то») не подпадают: требуется РОВНО одна
/// уникальная буква из набора хезитаций и длина >= 2.
fn is_hesitation(bare: &str) -> bool {
    let letters: Vec<char> = bare.chars().filter(|c| !DASH_CHARS.contains(*c)).collect();
    if letters.len() < 2 {
        return false;
    }
    let first = letters[0];
    "аэоумы".contains(first) && letters.iter().all(|&c| c == first)
}

/// Дефисы/тире, допустимые внутри хезитации («а-а», «э–э»).
const DASH_CHARS: &str = "-–—";

/// Паразиты: построчно — переводы строк (абзацы из GigaAM-финала) сохраняем.
fn remove_fillers(text: &str) -> String {
    text.split('\n')
        .map(remove_fillers_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn remove_fillers_line(text: &str) -> String {
    let mut t = format!(" {} ", text);
    // многословные — регистронезависимая подстрочная зачистка
    let lower = t.to_lowercase();
    for f in FILLERS_MULTI {
        let pat = format!(" {} ", f);
        let mut search_from = 0;
        let mut out = String::new();
        let lc = lower.clone();
        loop {
            if let Some(pos) = lc[search_from..].find(&pat) {
                let abs = search_from + pos;
                out.push_str(&t[search_from..abs]);
                out.push(' ');
                search_from = abs + pat.len();
            } else {
                out.push_str(&t[search_from..]);
                break;
            }
        }
        t = out;
    }
    // одно-словные — токенами
    let kept: Vec<&str> = t
        .split_whitespace()
        .filter(|w| {
            let bare = w
                .trim_matches(|c: char| !c.is_alphanumeric() && !DASH_CHARS.contains(c))
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase();
            !FILLERS_ONE.contains(&bare.as_str()) && !is_hesitation(&bare)
        })
        .collect();
    kept.join(" ")
}

/// Заменить целое слово `term` на `repl` (регистронезависимо).
fn replace_word_ci(text: &str, term: &str, repl: &str) -> String {
    let lower_text = text.to_lowercase();
    let lower_term = term.to_lowercase();
    let mut out = String::new();
    let mut i = 0;
    while let Some(pos) = lower_text[i..].find(&lower_term) {
        let abs = i + pos;
        let end = abs + lower_term.len();
        let before_ok = abs == 0
            || !text[..abs].chars().next_back().map(char::is_alphanumeric).unwrap_or(false);
        let after_ok = end >= text.len()
            || !text[end..].chars().next().map(char::is_alphanumeric).unwrap_or(false);
        out.push_str(&text[i..abs]);
        if before_ok && after_ok {
            out.push_str(repl);
        } else {
            out.push_str(&text[abs..end]);
        }
        i = end;
    }
    out.push_str(&text[i..]);
    out
}

fn capitalize_sentences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cap_next = true;
    for ch in text.chars() {
        if cap_next && ch.is_alphabetic() {
            out.extend(ch.to_uppercase());
            cap_next = false;
        } else {
            out.push(ch);
            // Новый абзац ('\n') — тоже начало предложения.
            if ".!?…".contains(ch) || ch == '\n' {
                cap_next = true;
            }
        }
    }
    out
}

/// Схлопнуть пробелы и убрать пробел перед пунктуацией. Публична, т.к. движок
/// (engine.rs) вызывает её ещё раз ФИНАЛЬНО — после apply_corrections и LLM-рерайта,
/// которые могут оставить лишние пробелы (C5).
pub fn normalize_spaces(text: &str) -> String {
    // C5: newline-aware. Нормализуем КАЖДУЮ строку отдельно и склеиваем через '\n',
    // чтобы не схлопывать многострочные сниппеты (подписи/шаблоны/код) в одну строку.
    let mut joined = text
        .split('\n')
        .map(normalize_line)
        .collect::<Vec<_>>()
        .join("\n");
    // 3+ подряд переводов строки -> ровно пустая строка-разделитель абзаца.
    while joined.contains("\n\n\n") {
        joined = joined.replace("\n\n\n", "\n\n");
    }
    joined
}

/// Нормализация ОДНОЙ строки: схлопнуть внутренние пробелы и убрать «прилипшие»
/// пробелы вокруг пунктуации, кавычек-ёлочек, скобок и тире, СОХРАНИВ ведущий
/// отступ (важно для многострочных сниппетов/кода).
fn normalize_line(line: &str) -> String {
    let indent_len = line.len() - line.trim_start().len();
    let indent = &line[..indent_len];
    let body = &line[indent_len..];
    // схлопнуть пробелы (split_whitespace убирает ведущие/хвостовые и схлопывает)
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    // нормализуем пробелы вокруг тире к единому виду " — " (whisper даёт вперемешку
    // " — ", "—слово", "слово —"); делаем это ДО посимвольной петли, чтобы не плодить
    // спецслучаи. Длинное — и среднее – приводим к длинному с пробелами по бокам.
    let dashed = normalize_dashes(&collapsed);
    // убрать пробел ПЕРЕД закрывающей пунктуацией/кавычкой/скобкой и пробел ПОСЛЕ
    // открывающей кавычки/скобки.
    const BEFORE: &str = ".,!?…:;»)]}"; // пробел перед этим — удалить
    const AFTER_OPEN: &str = "«([{"; // пробел сразу после этого — удалить
    let mut out = String::with_capacity(dashed.len());
    let mut chars = dashed.chars().peekable();
    let mut prev_nonspace: Option<char> = None;
    while let Some(c) = chars.next() {
        if c == ' ' {
            // пробел перед закрывающим знаком — пропустить
            if let Some(&n) = chars.peek() {
                if BEFORE.contains(n) {
                    continue;
                }
            }
            // пробел сразу после открывающей кавычки/скобки — пропустить
            if let Some(p) = prev_nonspace {
                if AFTER_OPEN.contains(p) {
                    continue;
                }
            }
        } else {
            prev_nonspace = Some(c);
        }
        out.push(c);
    }
    format!("{indent}{out}")
}

/// Привести тире к единому виду " — " (длинное тире с пробелами по бокам).
/// Покрывает варианты "слово—слово", "слово -слово", "слово – слово" и т.п.,
/// НЕ трогая дефис внутри слова (когда по обе стороны буквы/цифры без пробелов).
fn normalize_dashes(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        // средне/длинное тире — всегда отдельный знак: окружаем пробелами.
        if c == '—' || c == '–' {
            let trimmed = out.trim_end();
            out.truncate(trimmed.len());
            if !out.is_empty() {
                out.push(' ');
            }
            out.push('—');
            // пропускаем последующие пробелы — добавим один сами
            let mut j = i + 1;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            if j < chars.len() {
                out.push(' ');
            }
            i = j;
            continue;
        }
        // дефис-минус: тире ТОЛЬКО если это отдельное слово (пробелы по бокам),
        // иначе это дефис внутри слова (что-то, кто-то) — не трогаем.
        if c == '-'
            && i > 0
            && chars[i - 1] == ' '
            && i + 1 < chars.len()
            && chars[i + 1] == ' '
        {
            let trimmed = out.trim_end();
            out.truncate(trimmed.len());
            out.push_str(" — ");
            // пропускаем разделяющие пробелы после
            let mut j = i + 1;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Шаблон сниппета: {date} {time} {clipboard}.
fn expand_template(content: &str) -> String {
    let now = chrono::Local::now();
    let mut s = content.to_string();
    s = s.replace("{date}", &now.format("%d.%m.%Y").to_string());
    s = s.replace("{time}", &now.format("%H:%M").to_string());
    if s.contains("{clipboard}") {
        let clip = arboard::Clipboard::new()
            .ok()
            .and_then(|mut c| c.get_text().ok())
            .unwrap_or_default();
        s = s.replace("{clipboard}", &clip);
    }
    s
}

/// Короткая русская затравка по умолчанию. Даёт декодеру контекст языка/стиля
/// (грамотный русский с пунктуацией) и подавляет дрейф в латиницу, когда словарь
/// пользователя пуст. Намеренно нейтральная и КОРОТКАЯ, чтобы не «протекать» в
/// распознанный текст. Применяется только для русского языка (см. engine.rs).
pub const DEFAULT_RU_PROMPT: &str = "Распознавание русской речи. Грамотный текст с пунктуацией.";

/// Подсказка-biasing для whisper (initial prompt) из словаря пользователя.
///
/// `base` — необязательная языковая затравка (для ru — `DEFAULT_RU_PROMPT`),
/// чтобы и при ПУСТОМ словаре декодер получал контекст (раньше затравка была
/// пустой → бесполезной). Для не-русского языка `base` передаётся как None.
pub fn dict_bias_prompt(dict: &[Dict], base: Option<&str>) -> Option<String> {
    let terms: Vec<&str> = dict
        .iter()
        .map(|d| d.term.trim())
        .filter(|t| !t.is_empty())
        .collect();
    match (base, terms.is_empty()) {
        (Some(b), true) => Some(b.to_string()),
        (Some(b), false) => Some(format!("{b} Словарь: {}.", terms.join(", "))),
        (None, true) => None,
        (None, false) => Some(format!("Словарь: {}.", terms.join(", "))),
    }
}

#[cfg(test)]
mod filler_tests {
    use super::*;

    fn st(remove: bool, punct: bool) -> Settings {
        Settings {
            remove_fillers: remove,
            auto_punct: punct,
            verbatim: false,
            ..Settings::default()
        }
    }

    #[test]
    fn hyphenated_hesitations_removed() {
        let s = st(true, true);
        assert_eq!(
            process("определял, а-а, насколько это", &s, &[], &[]),
            "Определял, насколько это"
        );
        assert_eq!(process("Э-э, я не знаю почему.", &s, &[], &[]), "Я не знаю почему.");
        assert_eq!(process("м-м-м, хорошо", &s, &[], &[]), "Хорошо");
    }

    #[test]
    fn real_words_survive() {
        let s = st(true, false);
        assert_eq!(process("а я пошёл домой", &s, &[], &[]), "а я пошёл домой");
        assert_eq!(process("мы умные", &s, &[], &[]), "мы умные");
        assert_eq!(process("что-то кто-то", &s, &[], &[]), "что-то кто-то");
    }

    #[test]
    fn paragraphs_preserved_and_capitalized() {
        let s = st(true, true);
        assert_eq!(
            process("абзац один.\n\nабзац два, э-э, текст.", &s, &[], &[]),
            "Абзац один.\n\nАбзац два, текст."
        );
        assert_eq!(normalize_spaces("а\n\n\n\nб"), "а\n\nб");
    }
}
