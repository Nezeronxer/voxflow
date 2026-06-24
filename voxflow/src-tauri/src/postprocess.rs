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
        t = replace_word_ci(&t, from, &c.right);
    }
    t
}

/// Одно-словные паразиты (сверяются по нижнему регистру, без пунктуации).
///
/// Сюда входят ТОЛЬКО однозначные звуки-хезитации и слова, почти всегда являющиеся
/// паразитами в речи. Намеренно убраны «значит», «собственно», «блин»: это валидные
/// содержательные слова («это значит…», «собственно говоря», эмоц. «блин») — их
/// удаление портило смысл (FILLERS_ONE drops valid Russian words).
/// EN-часть (Parakeet) так же консервативна: только звуки-хезитации. Намеренно
/// НЕТ "err" («to err is human»), "hmm"/"well"/"like"/"you know" — легитимные слова.
const FILLERS_ONE: &[&str] = &[
    "эм",
    "эээ",
    "ээ",
    "ээм",
    "мм",
    "ммм",
    "мда",
    "хм",
    "ага",
    "типа",
    "короче",
    "um",
    "uh",
    "umm",
    "uhh",
    "erm",
    "mhm",
    "mm-hmm",
    "hmm",
];
/// Контекстные вводные: удаляем только в начале/после пунктуационной паузы,
/// чтобы не ломать смысл ("это значит", "вот это").
const FILLERS_CONTEXTUAL: &[&str] = &["ну", "вот", "значит"];
/// Многословные паразиты (подстрока, с границами-пробелами). EN-фраз тут нет
/// намеренно: "you know" / "i mean" / "kind of" / "sort of" часто несут смысл
/// («this kind of model») — подстрочная зачистка съедала легитимный текст.
const FILLERS_MULTI: &[&str] = &[
    "как бы",
    "в общем",
    "в общем-то",
    "это самое",
    "так сказать",
    "то есть как бы",
    "ну типа",
    "ну короче",
    "я не знаю честно",
    "честно говоря",
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

    // 3) Самоисправления: "нет, стоп", "точнее", "вернее" заменяют хвост фразы.
    t = collapse_stuttered_words(&t);
    t = collapse_inline_self_corrections(&t);
    t = apply_self_corrections(&t);

    // 4) Словарь (замены терминов, регистронезависимо по первому слову).
    for d in dict {
        if !d.term.trim().is_empty() {
            t = replace_word_ci(&t, d.term.trim(), &d.replacement);
        }
    }

    // 5) Капитализация + чистка пробелов.
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
    for f in FILLERS_MULTI {
        let lower = t.to_lowercase();
        let pat = format!(" {} ", f);
        let mut search_from = 0;
        let mut out = String::new();
        loop {
            if let Some(pos) = lower[search_from..].find(&pat) {
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
    let words: Vec<&str> = t.split_whitespace().collect();
    let kept: Vec<&str> = words
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let bare = (*w)
                .trim_matches(|c: char| !c.is_alphanumeric() && !DASH_CHARS.contains(c))
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase();
            let contextual = FILLERS_CONTEXTUAL.contains(&bare.as_str())
                && (i == 0 || words[i - 1].ends_with(|c: char| ",.!?…:;".contains(c)));
            if FILLERS_ONE.contains(&bare.as_str()) || contextual || is_hesitation(&bare) {
                None
            } else {
                Some(*w)
            }
        })
        .collect();
    kept.join(" ")
}

/// Срезать подряд идущие ПОЛНЫЕ повторы n-грамм из 2..=6 слов (заикания диктовки,
/// редкие RNNT-петли gigaam/parakeet и повторы облачного декодера), оставив одно
/// вхождение. Семантика согласована с asr::dedup_repeats (whisper): сравнение
/// регистронезависимое и без краевой пунктуации («Фраза.» == «фраза»), длинные
/// блоки пробуем первыми. Отличие — одиночные слова НЕ трогаем вовсе: «очень
/// очень», «да, да» — легитимные усиления, а не петля. Переводы строк (абзацы
/// GigaAM-финала) сохраняются — режем построчно.
pub fn dedup_repeated_ngrams(text: &str) -> String {
    text.split('\n')
        .map(dedup_ngrams_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// До фикспойнта: longest-first за один проход схлопывает чётное число копий лишь
/// наполовину («a b a b a b a b» → «a b a b»), повторный проход дожимает до одной.
fn dedup_ngrams_line(line: &str) -> String {
    let mut cur = line.to_string();
    loop {
        let next = dedup_ngrams_pass(&cur);
        if next == cur {
            return cur;
        }
        cur = next;
    }
}

fn dedup_ngrams_pass(line: &str) -> String {
    let toks: Vec<&str> = line.split_whitespace().collect();
    if toks.len() < 4 {
        return line.to_string(); // повтор 2-граммы требует минимум 4 токена
    }
    let norm = |t: &str| {
        t.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let mut out: Vec<&str> = Vec::with_capacity(toks.len());
    let mut i = 0usize;
    while i < toks.len() {
        let mut matched = false;
        // Длинные блоки первыми, чтобы не дробить фразу (как в asr::dedup_repeats).
        let max_n = ((toks.len() - i) / 2).min(6);
        for n in (2..=max_n).rev() {
            let block_eq = |j: usize| {
                toks[i..i + n]
                    .iter()
                    .map(|t| norm(t))
                    .eq(toks[j..j + n].iter().map(|t| norm(t)))
            };
            if block_eq(i + n) {
                // Дошагать до конца цепочки повторов и оставить ОДНУ копию.
                let mut j = i + 2 * n;
                while j + n <= toks.len() && block_eq(j) {
                    j += n;
                }
                out.extend_from_slice(&toks[i..i + n]);
                i = j;
                matched = true;
                break;
            }
        }
        if !matched {
            out.push(toks[i]);
            i += 1;
        }
    }
    out.join(" ")
}

#[derive(Clone)]
struct WordTok<'a> {
    raw: &'a str,
    bare: String,
}

const SELF_CORRECTION_MARKERS: &[&[&str]] = &[
    &["нет", "стоп"],
    &["нет"],
    &["ой"],
    &["погоди"],
    &["подожди"],
    &["стоп", "не", "то"],
    &["не", "так"],
    &["не", "точно"],
    &["точнее"],
    &["точнее", "говоря"],
    &["вернее"],
    &["в", "смысле"],
    &["то", "есть"],
    &["отмена"],
    &["забудь"],
    &["зачеркни"],
];

fn collapse_stuttered_words(text: &str) -> String {
    text.split('\n')
        .map(collapse_stuttered_words_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn collapse_stuttered_words_line(line: &str) -> String {
    let toks: Vec<&str> = line.split_whitespace().collect();
    if toks.len() < 3 {
        return line.to_string();
    }
    let norm = |t: &str| {
        t.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let mut out: Vec<&str> = Vec::with_capacity(toks.len());
    let mut i = 0usize;
    while i < toks.len() {
        let cur = norm(toks[i]);
        if cur.is_empty() {
            out.push(toks[i]);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < toks.len() && norm(toks[j]) == cur {
            j += 1;
        }
        if j - i >= 3 {
            out.push(toks[i]);
        } else {
            out.extend_from_slice(&toks[i..j]);
        }
        i = j;
    }
    out.join(" ")
}

fn collapse_inline_self_corrections(text: &str) -> String {
    text.split('\n')
        .map(collapse_inline_self_corrections_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn collapse_inline_self_corrections_line(line: &str) -> String {
    line.split_whitespace()
        .map(collapse_inline_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn collapse_inline_token(token: &str) -> String {
    let Some((sep_idx, sep)) = token
        .char_indices()
        .find(|(_, c)| *c == '-' || *c == '–' || *c == '—')
    else {
        return token.to_string();
    };
    let right_start = sep_idx + sep.len_utf8();
    let left = &token[..sep_idx];
    let right = &token[right_start..];
    let prefix: String = left.chars().take_while(|c| !c.is_alphanumeric()).collect();
    let suffix: String = right
        .chars()
        .rev()
        .take_while(|c| !c.is_alphanumeric())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let left_word = left
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let right_word = right
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if looks_like_inline_correction(&left_word, &right_word) {
        format!(
            "{prefix}{}{suffix}",
            right.trim_matches(|c: char| !c.is_alphanumeric())
        )
    } else {
        token.to_string()
    }
}

fn looks_like_inline_correction(left: &str, right: &str) -> bool {
    let lc = left.chars().count();
    let rc = right.chars().count();
    if lc < 4 || rc < 4 {
        return false;
    }
    if !left.chars().all(char::is_alphabetic) || !right.chars().all(char::is_alphabetic) {
        return false;
    }
    let common = left
        .chars()
        .zip(right.chars())
        .take_while(|(a, b)| a == b)
        .count();
    common >= 4 || (common >= 3 && rc > lc)
}

/// Применить устные самоисправления: "в пять, нет, стоп, в шесть" →
/// "в шесть" в хвосте той же фразы. Это локальная дешёвая версия backtrack,
/// до LLM: срабатывает только при явном маркере и наличии текста до/после него.
fn apply_self_corrections(text: &str) -> String {
    text.split('\n')
        .map(apply_self_corrections_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn apply_self_corrections_line(line: &str) -> String {
    let mut toks: Vec<WordTok<'_>> = line
        .split_whitespace()
        .map(|raw| WordTok {
            raw,
            bare: raw
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase(),
        })
        .collect();
    if toks.len() < 3 {
        return line.to_string();
    }

    loop {
        let Some((start, len)) = find_last_self_correction_marker(&toks) else {
            break;
        };
        if start == 0 || start + len >= toks.len() {
            break;
        }
        let left = &toks[..start];
        let right = &toks[start + len..];
        let cut = correction_cut_point(left, right);
        let mut next = Vec::with_capacity(cut + right.len());
        next.extend_from_slice(&left[..cut]);
        next.extend_from_slice(right);
        toks = next;
        if toks.len() < 3 {
            break;
        }
    }

    toks.iter().map(|t| t.raw).collect::<Vec<_>>().join(" ")
}

fn find_last_self_correction_marker(toks: &[WordTok<'_>]) -> Option<(usize, usize)> {
    let mut found = None;
    for i in 0..toks.len() {
        for marker in SELF_CORRECTION_MARKERS {
            if i + marker.len() <= toks.len()
                && marker
                    .iter()
                    .enumerate()
                    .all(|(j, w)| toks[i + j].bare == *w)
            {
                let better = found
                    .map(|(prev_i, prev_len)| {
                        i > prev_i || (i == prev_i && marker.len() > prev_len)
                    })
                    .unwrap_or(true);
                if better {
                    found = Some((i, marker.len()));
                }
            }
        }
    }
    found
}

fn correction_cut_point(left: &[WordTok<'_>], right: &[WordTok<'_>]) -> usize {
    let Some(first) = right
        .iter()
        .find(|t| !t.bare.is_empty())
        .map(|t| t.bare.as_str())
    else {
        return left.len();
    };
    if let Some(i) = left.iter().rposition(|t| t.bare == first) {
        return i;
    }
    let drop = right.len().clamp(1, 4).min(left.len());
    left.len() - drop
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
            || !text[..abs]
                .chars()
                .next_back()
                .map(char::is_alphanumeric)
                .unwrap_or(false);
        let after_ok = end >= text.len()
            || !text[end..]
                .chars()
                .next()
                .map(char::is_alphanumeric)
                .unwrap_or(false);
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
        if c == '-' && i > 0 && chars[i - 1] == ' ' && i + 1 < chars.len() && chars[i + 1] == ' ' {
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
        assert_eq!(
            process("Э-э, я не знаю почему.", &s, &[], &[]),
            "Я не знаю почему."
        );
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

    #[test]
    fn en_fillers_removed() {
        let s = st(true, true);
        assert_eq!(
            process("um, send the report", &s, &[], &[]),
            "Send the report"
        );
        assert_eq!(
            process("Uh, I think, erm, it works", &s, &[], &[]),
            "I think, it works"
        );
        assert_eq!(process("mm-hmm, okay", &s, &[], &[]), "Okay");
        assert_eq!(process("Umm... uhh... done", &s, &[], &[]), "Done");
    }

    #[test]
    fn en_legit_words_survive() {
        let s = st(true, false);
        // "you know" / "i mean" / "kind of" несут смысл — зачистка их не трогает.
        assert_eq!(
            process("you know what i mean", &s, &[], &[]),
            "you know what i mean"
        );
        assert_eq!(
            process("this kind of model works well", &s, &[], &[]),
            "this kind of model works well"
        );
        // "err" — легитимный глагол, не хезитация.
        assert_eq!(process("to err is human", &s, &[], &[]), "to err is human");
    }

    #[test]
    fn latin_sentences_capitalized() {
        let s = st(false, true);
        assert_eq!(
            process("hello world. this is fine! is it? yes", &s, &[], &[]),
            "Hello world. This is fine! Is it? Yes"
        );
        // '\n' — тоже начало предложения, как и в RU-кейсе.
        assert_eq!(
            process("first line.\n\nsecond line", &s, &[], &[]),
            "First line.\n\nSecond line"
        );
    }

    #[test]
    fn self_correction_replaces_tail_after_marker() {
        let s = st(true, true);
        assert_eq!(
            process("встретимся в пять, нет, стоп, в шесть", &s, &[], &[]),
            "Встретимся в шесть"
        );
        assert_eq!(
            process("сделай кнопку синей, точнее зелёной", &s, &[], &[]),
            "Сделай кнопку зелёной"
        );
        assert_eq!(
            process("напиши промт для опус, вернее для GPT-4", &s, &[], &[]),
            "Напиши промт для GPT-4"
        );
        assert_eq!(
            process("встреча завтра в пять, то есть в шесть", &s, &[], &[]),
            "Встреча завтра в шесть"
        );
        assert_eq!(process("красный, отмена, синий", &s, &[], &[]), "Синий");
        assert_eq!(
            process("сделай это завтра, нет, послезавтра", &s, &[], &[]),
            "Сделай это послезавтра"
        );
        assert_eq!(
            process(
                "поставь встречу на понедельник, ой, на вторник",
                &s,
                &[],
                &[]
            ),
            "Поставь встречу на вторник"
        );
    }

    #[test]
    fn repeated_starter_words_are_collapsed() {
        let s = st(true, true);
        assert_eq!(
            process("я я я думаю это сработает", &s, &[], &[]),
            "Я думаю это сработает"
        );
        assert_eq!(process("это это важно", &s, &[], &[]), "Это это важно");
    }

    #[test]
    fn inline_hyphen_self_correction_keeps_second_word() {
        let s = st(true, true);
        assert_eq!(
            process("чтобы работал-работали кнопки", &s, &[], &[]),
            "Чтобы работали кнопки"
        );
        assert_eq!(
            process("что-то кто-то важно", &s, &[], &[]),
            "Что-то кто-то важно"
        );
    }

    #[test]
    fn self_correction_preserves_lines() {
        let s = st(true, true);
        assert_eq!(
            process(
                "первая строка\nвстреча завтра, не так, послезавтра",
                &s,
                &[],
                &[]
            ),
            "Первая строка\nВстреча послезавтра"
        );
    }

    #[test]
    fn contextual_fillers_removed_without_eating_meaning() {
        let s = st(true, false);
        assert_eq!(process("ну я думаю", &s, &[], &[]), "я думаю");
        assert_eq!(
            process("привет, вот я пришёл", &s, &[], &[]),
            "привет, я пришёл"
        );
        assert_eq!(
            process("это значит многое", &s, &[], &[]),
            "это значит многое"
        );
        assert_eq!(process("вот это важно", &s, &[], &[]), "это важно");
    }

    #[test]
    fn learned_corrections_respect_word_boundaries() {
        assert_eq!(
            apply_corrections(
                "кот и котик",
                &[Correction {
                    wrong: "кот".into(),
                    right: "пёс".into()
                }]
            ),
            "пёс и котик"
        );
    }
}

#[cfg(test)]
mod dedup_tests {
    use super::*;

    #[test]
    fn stutter_phrase_collapsed() {
        assert_eq!(
            dedup_repeated_ngrams("please send please send please send the report"),
            "please send the report"
        );
        assert_eq!(
            dedup_repeated_ngrams("отправь отчёт отправь отчёт пожалуйста"),
            "отправь отчёт пожалуйста"
        );
    }

    #[test]
    fn even_copies_collapse_to_one() {
        // 4 копии «раз два»: longest-first за один проход оставил бы «раз два раз два»,
        // фикспойнт дожимает до одной копии.
        assert_eq!(
            dedup_repeated_ngrams("раз два раз два раз два раз два три"),
            "раз два три"
        );
    }

    #[test]
    fn single_word_repeats_untouched() {
        assert_eq!(dedup_repeated_ngrams("очень очень рад"), "очень очень рад");
        assert_eq!(dedup_repeated_ngrams("да, да"), "да, да");
        assert_eq!(
            dedup_repeated_ngrams("он шёл шёл шёл и пришёл"),
            "он шёл шёл шёл и пришёл"
        );
    }

    #[test]
    fn case_and_punct_tolerant() {
        // «Фраза.» == «фраза» — как в asr::dedup_repeats.
        assert_eq!(
            dedup_repeated_ngrams("Я пошёл домой. я пошёл домой."),
            "Я пошёл домой."
        );
    }

    #[test]
    fn legit_text_not_cut() {
        assert_eq!(
            dedup_repeated_ngrams("я знаю, что ты знаешь, что я знаю"),
            "я знаю, что ты знаешь, что я знаю"
        );
        assert_eq!(dedup_repeated_ngrams(""), "");
        assert_eq!(dedup_repeated_ngrams("слово"), "слово");
    }

    #[test]
    fn newlines_preserved() {
        assert_eq!(
            dedup_repeated_ngrams("абзац один абзац один\n\nабзац два"),
            "абзац один\n\nабзац два"
        );
    }
}
