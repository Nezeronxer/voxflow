//! Детерминированная (offline, без LLM) постобработка распознанного текста:
//! сниппеты → паразиты → словарь → капитализация. Verbatim отключает всё.

use crate::settings::Settings;
use std::collections::HashSet;

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
    let mut ordered: Vec<&Correction> = corrections.iter().collect();
    ordered.sort_by(|a, b| {
        let aw = a.wrong.split_whitespace().count();
        let bw = b.wrong.split_whitespace().count();
        bw.cmp(&aw)
            .then_with(|| b.wrong.chars().count().cmp(&a.wrong.chars().count()))
    });
    for c in ordered {
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

    // 1) Сниппет на всю фразу. Slash-триггеры принимают безопасные голосовые
    // варианты, но никогда не срабатывают внутри обычного предложения.
    if let Some(expanded) = expand_matching_snippet(&t, snippets) {
        return expanded;
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

    // 4) Словарь: longest match wins, а подставленный текст не проходит через
    // следующие правила повторно. Иначе `wispr flow -> Wispr Flow` мог затем
    // превратиться правилом `wispr -> X` в `X Flow`.
    t = apply_dictionary_once(&t, dict);

    // 5) ASR иногда вставляет произвольные переносы строк по сегментам/паузам.
    // Для диктовки это не смысловая разметка: случайный "\n" не должен превращать
    // середину фразы в новый абзац и новую заглавную букву.
    t = normalize_dictation_breaks(&t);

    // 6) Капитализация + чистка пробелов.
    if s.auto_punct {
        t = capitalize_sentences(&t);
    }
    normalize_spaces(&t)
}

fn apply_dictionary_once(text: &str, dict: &[Dict]) -> String {
    let mut ordered = dict
        .iter()
        .filter(|entry| !entry.term.trim().is_empty())
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| {
        b.term
            .split_whitespace()
            .count()
            .cmp(&a.term.split_whitespace().count())
            .then_with(|| b.term.chars().count().cmp(&a.term.chars().count()))
    });
    if ordered.is_empty() {
        return text.to_string();
    }

    // Use collision-free private-use markers while matching every rule against
    // the original hypothesis. Markers are selected outside the source,
    // terms, and replacements, so restoring them cannot rewrite user content.
    let mut reserved = text.chars().collect::<HashSet<_>>();
    for entry in &ordered {
        reserved.extend(entry.term.chars());
        reserved.extend(entry.replacement.chars());
    }
    let markers = (0xE000..=0xF8FF)
        .chain(0xF0000..=0xFFFFD)
        .chain(0x100000..=0x10FFFD)
        .filter_map(char::from_u32)
        .filter(|candidate| !reserved.contains(candidate))
        .take(ordered.len())
        .collect::<Vec<_>>();
    if markers.len() != ordered.len() {
        return text.to_string();
    }

    let mut out = text.to_string();
    let mut restorations = Vec::with_capacity(ordered.len());
    for (entry, marker) in ordered.into_iter().zip(markers) {
        let term = entry.term.trim();
        let replacement = entry.replacement.trim();
        let replacement = if replacement.is_empty() {
            term.to_string()
        } else {
            replacement.to_string()
        };
        out = replace_word_ci(&out, term, &marker.to_string());
        restorations.push((marker, replacement));
    }
    for (marker, replacement) in restorations {
        out = out.replace(marker, &replacement);
    }
    out
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
    // Match multi-word fillers as normalized tokens. Never transfer byte
    // offsets from a lowercased copy back to the original: Unicode case folding
    // may expand a character (`İ` -> `i` + combining dot).
    let source_words = text.split_whitespace().collect::<Vec<_>>();
    let normalized_words = source_words
        .iter()
        .map(|word| normalize_filler_word(word))
        .collect::<Vec<_>>();
    let filler_phrases = FILLERS_MULTI
        .iter()
        .map(|phrase| {
            phrase
                .split_whitespace()
                .map(normalize_filler_word)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut multi_filtered = Vec::with_capacity(source_words.len());
    let mut index = 0usize;
    while index < source_words.len() {
        let matched = filler_phrases
            .iter()
            .filter(|phrase| {
                !phrase.is_empty()
                    && index + phrase.len() <= normalized_words.len()
                    && normalized_words[index..index + phrase.len()] == phrase[..]
            })
            .map(Vec::len)
            .max()
            .unwrap_or(0);
        if matched > 0 {
            index += matched;
        } else {
            multi_filtered.push(source_words[index]);
            index += 1;
        }
    }
    let t = multi_filtered.join(" ");
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

fn normalize_filler_word(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric() && !DASH_CHARS.contains(c))
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Срезать только явно зациклившиеся ПОЛНЫЕ повторы n-грамм из 2..=6 слов
/// (редкие RNNT-петли gigaam/parakeet и повторы облачного декодера), оставив
/// одно вхождение. Две копии сохраняем: пользователь вправе намеренно повторить
/// предложение, а одна пауза не доказывает ошибку декодера. Петлёй считаем лишь
/// три и более соседних копии. Сравнение регистронезависимое и без краевой
/// пунктуации («Фраза.» == «фраза»); одиночные слова тоже не трогаем.
/// Переводы строк сохраняются — режем построчно.
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
        let max_n = ((toks.len() - i) / 2).min(6);
        let mut best: Option<(usize, usize, usize)> = None;
        for n in 2..=max_n {
            let block_eq = |j: usize| {
                toks[i..i + n]
                    .iter()
                    .map(|t| norm(t))
                    .eq(toks[j..j + n].iter().map(|t| norm(t)))
            };
            if block_eq(i + n) {
                // Дошагать до конца цепочки. Две копии могут быть намеренной
                // речью; схлопываем только доказанную цепочку из 3+ копий.
                let mut j = i + 2 * n;
                let mut copies = 2usize;
                while j + n <= toks.len() && block_eq(j) {
                    j += n;
                    copies += 1;
                }
                if copies < 3 {
                    continue;
                }
                let span = j - i;
                let replace_best = best
                    .map(|(best_n, _, best_span)| {
                        span > best_span || (span == best_span && n < best_n)
                    })
                    .unwrap_or(true);
                if replace_best {
                    best = Some((n, j, span));
                }
            }
        }
        if let Some((n, end, _)) = best {
            // Prefer the primitive repeated phrase when a composite block spans
            // the same loop (6× "a b" must become one "a b", not two copies).
            out.extend_from_slice(&toks[i..i + n]);
            i = end;
            matched = true;
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
        if j - i >= 3 || (j - i == 2 && collapse_double_starter_repeat(&cur)) {
            out.push(toks[i]);
        } else {
            out.extend_from_slice(&toks[i..j]);
        }
        i = j;
    }
    out.join(" ")
}

fn collapse_double_starter_repeat(word: &str) -> bool {
    matches!(
        word,
        "я" | "мы"
            | "ты"
            | "вы"
            | "он"
            | "она"
            | "они"
            | "оно"
            | "это"
            | "этот"
            | "эта"
            | "эти"
            | "то"
            | "что"
            | "как"
            | "и"
            | "а"
            | "но"
            | "вот"
            | "там"
            | "тут"
            | "здесь"
            | "просто"
    )
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

    while let Some((start, len)) = find_last_self_correction_marker(&toks) {
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
    let lower_term = term.to_lowercase();
    if lower_term.is_empty() {
        return text.to_string();
    }

    // Never reuse offsets from `text.to_lowercase()` against `text`: Unicode
    // lowercasing may change byte length (`İ` -> `i` + combining dot), which
    // previously produced a non-char-boundary slice and panicked. Instead,
    // grow each candidate over original char boundaries while folding it.
    let mut out = String::with_capacity(text.len());
    let mut copied_until = 0usize;
    let mut search_at = 0usize;
    while search_at < text.len() {
        let mut folded = String::new();
        let mut matched_end = None;
        for (relative, ch) in text[search_at..].char_indices() {
            folded.extend(ch.to_lowercase());
            let end = search_at + relative + ch.len_utf8();
            if folded == lower_term {
                matched_end = Some(end);
                break;
            }
            if !lower_term.starts_with(&folded) {
                break;
            }
        }

        let Some(end) = matched_end else {
            search_at += text[search_at..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
            continue;
        };
        let before_ok = search_at == 0
            || !text[..search_at]
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
        if before_ok && after_ok {
            out.push_str(&text[copied_until..search_at]);
            out.push_str(repl);
            copied_until = end;
            search_at = end;
            continue;
        }
        search_at += text[search_at..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
    }
    out.push_str(&text[copied_until..]);
    out
}

fn normalize_dictation_breaks(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.contains('\n') {
        return normalized;
    }
    let lines: Vec<&str> = normalized
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    if looks_like_list_block(&lines) {
        return lines.join("\n");
    }
    lines.join(" ")
}

fn looks_like_list_block(lines: &[&str]) -> bool {
    let list_items = lines
        .iter()
        .filter(|line| starts_with_list_marker(line))
        .count();
    list_items >= 2
        || (list_items >= 1
            && lines
                .first()
                .map(|line| line.ends_with(':'))
                .unwrap_or(false))
}

fn starts_with_list_marker(line: &str) -> bool {
    let s = line.trim_start();
    if s.starts_with("- ") || s.starts_with("* ") || s.starts_with("• ") {
        return true;
    }
    let mut chars = s.char_indices();
    let mut last_digit_end = 0usize;
    let mut has_digit = false;
    for (idx, ch) in chars.by_ref() {
        if ch.is_ascii_digit() {
            has_digit = true;
            last_digit_end = idx + ch.len_utf8();
            continue;
        }
        break;
    }
    if !has_digit {
        return false;
    }
    let rest = &s[last_digit_end..];
    let Some(marker) = rest.chars().next() else {
        return false;
    };
    if marker != '.' && marker != ')' {
        return false;
    }
    rest[marker.len_utf8()..]
        .chars()
        .next()
        .map(char::is_whitespace)
        .unwrap_or(false)
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
            if ".!?…".contains(ch) {
                cap_next = true;
            }
        }
    }
    out
}

/// High-confidence signal that the speaker stopped while the current clause is
/// still grammatically open. Recording boundaries are transport events, not
/// sentence boundaries: Whisper/cloud punctuation may still append a period at
/// every Stop, so the final pipeline needs an independent, deterministic guard.
///
/// This intentionally stays conservative. It recognizes dangling conjunctions,
/// subordinators/prepositions, a subordinator with at most one following word,
/// unmatched brackets and an ellipsis. Ambiguous complete fragments such as
/// "Я хотел." are left to the recognizer instead of being rewritten here.
pub fn looks_unfinished_utterance(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    // An explicit question/exclamation is a strong completion signal even when
    // its final word also happens to be a conjunction (for example "Что?").
    let terminal_view = trimmed.trim_end_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | ')' | ']' | '}' | '»' | '”')
    });
    let significant = terminal_view.chars().last();
    if matches!(significant, Some('?' | '!')) {
        return false;
    }
    // Ellipsis alone is ambiguous: it may be hesitation inside a sentence or
    // an intentional soft ending ("Ну, не знаю…"). Keep evaluating the words;
    // only another dangling grammatical signal makes it high-confidence open.
    if has_unclosed_brackets(trimmed) {
        return true;
    }

    let words = semantic_words(trimmed);
    if words.is_empty() {
        return false;
    }
    let joined = words.join(" ");
    const OPEN_TAILS: &[&str] = &[
        // Russian logical glue.
        "и",
        "или",
        "либо",
        "а",
        "но",
        "чтобы",
        "для того чтобы",
        "из за того что",
        // Russian prepositions which cannot normally finish a clause.
        "в",
        "во",
        "на",
        "к",
        "ко",
        "от",
        "до",
        "для",
        "по",
        "из",
        "с",
        "со",
        "без",
        "у",
        "о",
        "об",
        "обо",
        "над",
        "под",
        "между",
        "через",
        "перед",
        // English equivalents for the multilingual default.
        "and",
        "or",
        "but",
        // English clause-final prepositions and pronouns are deliberately not
        // listed: "Log in.", "I want to." and "I wanted that." are complete.
        "because of",
        "so that",
    ];
    if OPEN_TAILS
        .iter()
        .any(|tail| joined == *tail || joined.ends_with(&format!(" {tail}")))
    {
        return true;
    }

    // These subordinators are only a high-confidence open tail when the same
    // clause contains an explicit comma before them. Standalone conversational
    // answers such as "Потому что." / "Just because." stay complete.
    let clause_view = terminal_view
        .trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, '.' | '…'))
        .to_lowercase();
    if [", что", ", потому что", ", так как", ", поскольку"]
        .iter()
        .any(|tail| clause_view.ends_with(tail))
    {
        return true;
    }
    // English usually omits a comma before `because`. A main clause with at
    // least two preceding words is a strong open-tail signal, while common
    // standalone answers (`Because.`, `Just because.`) remain complete.
    if words.last().is_some_and(|word| word == "because")
        && words.len() >= 3
        && words
            .get(words.len() - 2)
            .is_some_and(|word| word != "just")
    {
        return true;
    }

    // "Я хочу, чтобы ты." / "I think that we." — the recognizer may add a
    // period although the dependent clause contains only its pronominal
    // subject. Do not treat every one-word dependent clause as unfinished:
    // "Я думаю, что да." and "I know that works." can be complete.
    const RECENT_SUBORDINATORS: &[&str] = &[
        "что",
        "чтобы",
        "если",
        "когда",
        "который",
        "которая",
        "которое",
        "которые",
        "that",
        "if",
        "when",
        "which",
        "who",
        "where",
        "because",
    ];
    const DANGLING_SUBJECTS: &[&str] = &[
        "я", "ты", "вы", "он", "она", "оно", "мы", "они", "i", "you", "he", "she", "it", "we",
        "they",
    ];
    words.iter().enumerate().any(|(index, word)| {
        // A sentence-initial "That works." / "Если можно." is not enough
        // evidence. This rule is for a newly opened dependent clause after an
        // already present main clause: "Я хочу, чтобы ты.".
        index > 0
            && RECENT_SUBORDINATORS.contains(&word.as_str())
            && words.len().saturating_sub(index) == 2
            && DANGLING_SUBJECTS.contains(&words[index + 1].as_str())
    })
}

/// A dangling Russian preposition often introduces a proper name in the next
/// recording ("приехал в" + "Москву"). In that case ASR capitalization is
/// meaningful and must not be lowered as if it were a synthetic sentence start.
pub fn continuation_may_start_with_proper_name(previous: &str, next: &str) -> bool {
    let words = semantic_words(previous);
    let Some(last) = words.last().map(String::as_str) else {
        return false;
    };
    if !matches!(
        last,
        "в" | "во"
            | "на"
            | "к"
            | "ко"
            | "от"
            | "до"
            | "для"
            | "по"
            | "из"
            | "с"
            | "со"
            | "без"
            | "у"
            | "о"
            | "об"
            | "обо"
            | "над"
            | "под"
            | "между"
            | "через"
            | "после"
            | "перед"
            | "вокруг"
    ) {
        return false;
    }

    let original_words = next
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let Some(first) = original_words.first().copied() else {
        return false;
    };
    let starts_uppercase = first.chars().next().is_some_and(char::is_uppercase);
    if !starts_uppercase {
        return false;
    }
    // Two title-cased words are a useful proper-name signal: "Нижний
    // Новгород". A single title-cased word is not: every independent ASR
    // result starts that way. Preserve it only for an acronym or when the
    // preceding clause itself signals a person/place name ("приехал в").
    if original_words
        .get(1)
        .and_then(|word| word.chars().next())
        .is_some_and(char::is_uppercase)
    {
        return true;
    }
    let is_acronym = first.chars().filter(|ch| ch.is_alphabetic()).count() > 1
        && first
            .chars()
            .filter(|ch| ch.is_alphabetic())
            .all(char::is_uppercase);
    const NAMED_ENTITY_CONTEXT_STEMS: &[&str] = &[
        // Motion/location.
        "приех",
        "поех",
        "прилет",
        "улетел",
        "прибы",
        "родил",
        "переех",
        "верну",
        "отправ",
        "направ",
        "пойд",
        // People/organisations.
        "встрет",
        "поговор",
        "говор",
        "позвон",
        "связ",
    ];
    const NAMED_ENTITY_CONTEXT_WORDS: &[&str] = &[
        "едем",
        "едет",
        "едут",
        "ехал",
        "ехала",
        "ехали",
        "иду",
        "идем",
        "идём",
        "идет",
        "идёт",
        "идут",
        "лечу",
        "летим",
        "летит",
        "летят",
        "летел",
        "летела",
        "летели",
        "живу",
        "живем",
        "живём",
        "живет",
        "живёт",
        "живут",
        "жил",
        "жила",
        "жили",
    ];
    let governor_index = words.len().saturating_sub(2);
    let governor = words.get(governor_index);
    let first_person_ride = governor.is_some_and(|word| word == "еду")
        && words
            .get(governor_index.saturating_sub(1))
            .is_some_and(|word| word == "я");
    is_acronym
        || first_person_ride
        || governor.is_some_and(|word| {
            NAMED_ENTITY_CONTEXT_WORDS.contains(&word.as_str())
                || NAMED_ENTITY_CONTEXT_STEMS
                    .iter()
                    .any(|stem| word.starts_with(stem))
        })
}

/// Remove only a model-added single final period from a high-confidence open
/// clause. Question/exclamation marks, ellipses and explicitly spoken terminal
/// punctuation are preserved. Closing quotes/brackets may follow the period.
pub fn preserve_unfinished_ending(text: &str, raw_hypothesis: &str) -> String {
    if utterance_has_explicit_terminal_punctuation(raw_hypothesis)
        || !looks_unfinished_utterance(text)
    {
        return text.to_string();
    }

    let mut chars = text.chars().collect::<Vec<_>>();
    let mut cursor = chars.len();
    while cursor > 0
        && (chars[cursor - 1].is_whitespace()
            || matches!(chars[cursor - 1], '"' | '\'' | ')' | ']' | '}' | '»' | '”'))
    {
        cursor -= 1;
    }
    if cursor == 0 || chars[cursor - 1] != '.' {
        return text.to_string();
    }
    // Three ASCII dots are an intentional/open ellipsis, not an auto-period.
    if cursor >= 2 && chars[cursor - 2] == '.' {
        return text.to_string();
    }
    chars.remove(cursor - 1);
    chars.into_iter().collect()
}

fn semantic_words(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect()
}

pub fn utterance_has_explicit_terminal_punctuation(raw: &str) -> bool {
    let joined = semantic_words(raw).join(" ");
    const SPOKEN_TERMINALS: &[&str] = &[
        "точка",
        "вопросительный знак",
        "восклицательный знак",
        "многоточие",
        "period",
        "full stop",
        "question mark",
        "exclamation mark",
        "ellipsis",
    ];
    SPOKEN_TERMINALS
        .iter()
        .any(|tail| joined == *tail || joined.ends_with(&format!(" {tail}")))
}

fn has_unclosed_brackets(text: &str) -> bool {
    let mut stack = Vec::new();
    for ch in text.chars() {
        match ch {
            '(' | '[' | '{' => stack.push(ch),
            ')' | ']' | '}' => {
                let expected = match ch {
                    ')' => '(',
                    ']' => '[',
                    '}' => '{',
                    _ => unreachable!(),
                };
                if stack.last().copied() == Some(expected) {
                    stack.pop();
                }
            }
            _ => {}
        }
    }
    !stack.is_empty()
}

/// Смягчить типичный артефакт диктовки: ASR ставит точку после короткой паузы,
/// а следующий дискурсивный маркер начинает с заглавной ("... . То есть ...").
/// Формальные профили решают это выше по контексту; функция публична, чтобы движок
/// включал её только для разговорных/нейтральных целей.
pub fn soften_false_sentence_breaks(text: &str) -> String {
    const STARTERS: &[&str] = &[
        "то есть",
        "потому что",
        "а",
        "и",
        "но",
        "чтобы",
        "если",
        "когда",
        "поэтому",
        "видишь",
        "допустим",
        "наверное",
        "просто",
        "ещё",
    ];

    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while let Some(rel) = text[i..].find(". ") {
        let dot = i + rel;
        let after = dot + 2;
        out.push_str(&text[i..dot]);
        if let Some((matched_len, replacement)) = false_break_starter(&text[after..], STARTERS) {
            out.push_str(", ");
            out.push_str(replacement);
            i = after + matched_len;
        } else {
            out.push_str(". ");
            i = after;
        }
    }
    out.push_str(&text[i..]);
    out
}

fn false_break_starter<'a>(rest: &str, starters: &'a [&'a str]) -> Option<(usize, &'a str)> {
    for starter in starters {
        let chars = starter.chars().count();
        let candidate: String = rest.chars().take(chars).collect();
        if candidate.chars().count() != chars || candidate.to_lowercase() != *starter {
            continue;
        }
        let matched_len = candidate.len();
        let boundary_ok = rest[matched_len..]
            .chars()
            .next()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true);
        if boundary_ok {
            return Some((matched_len, *starter));
        }
    }
    None
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

fn normalize_snippet_phrase(value: &str) -> String {
    value
        .trim_matches(|c: char| c.is_whitespace() || ".,!?…:;\"'«»()[]{}".contains(c))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn snippet_trigger_matches(spoken: &str, trigger: &str) -> bool {
    let spoken = normalize_snippet_phrase(spoken);
    let trigger = normalize_snippet_phrase(trigger);
    if trigger.is_empty() {
        return false;
    }
    if spoken == trigger {
        return true;
    }
    let Some(short) = trigger.strip_prefix('/').map(str::trim) else {
        return false;
    };
    if short.is_empty() || spoken == short {
        return !short.is_empty();
    }
    for prefix in ["слэш", "слеш", "slash", "косая черта", "сниппет", "снипет"]
    {
        let Some(rest) = spoken.strip_prefix(prefix) else {
            continue;
        };
        let separated = rest
            .chars()
            .next()
            .map(|c| c.is_whitespace() || "-/".contains(c))
            .unwrap_or(false);
        if separated
            && rest.trim_start_matches(|c: char| c.is_whitespace() || "-/".contains(c)) == short
        {
            return true;
        }
    }
    false
}

/// Expand an exact whole-utterance snippet once. The engine uses this signal to
/// keep the resulting body out of corrections and LLM rewrite.
pub fn expand_matching_snippet(text: &str, snippets: &[Snippet]) -> Option<String> {
    let spoken = text.trim();
    if spoken.is_empty() {
        return None;
    }
    snippets.iter().find_map(|snippet| {
        snippet_trigger_matches(spoken, &snippet.trigger).then(|| {
            if snippet.is_template {
                expand_template(&snippet.content)
            } else {
                snippet.content.clone()
            }
        })
    })
}

/// Шаблон сниппета: {date}/{дата}, {time}/{время},
/// {clipboard}/{буфер}. Неизвестные и временно недоступные placeholders
/// сохраняются дословно вместо тихой потери текста.
fn expand_template(content: &str) -> String {
    let now = chrono::Local::now();
    let date = now.format("%d.%m.%Y").to_string();
    let time = now.format("%H:%M").to_string();
    let clipboard = arboard::Clipboard::new()
        .ok()
        .and_then(|mut c| c.get_text().ok());
    expand_template_with(content, &date, &time, clipboard.as_deref())
}

fn expand_template_with(content: &str, date: &str, time: &str, clipboard: Option<&str>) -> String {
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' && chars.get(i + 1) == Some(&'{') {
            out.push('{');
            i += 2;
            continue;
        }
        if chars[i] == '}' && chars.get(i + 1) == Some(&'}') {
            out.push('}');
            i += 2;
            continue;
        }
        if chars[i] != '{' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let Some(rel_end) = chars[i + 1..].iter().position(|&c| c == '}') else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        let end = i + 1 + rel_end;
        let name: String = chars[i + 1..end].iter().collect::<String>().to_lowercase();
        match name.as_str() {
            "date" | "дата" => out.push_str(date),
            "time" | "время" => out.push_str(time),
            "clipboard" | "буфер" => {
                if let Some(value) = clipboard {
                    out.push_str(value);
                } else {
                    out.extend(chars[i..=end].iter());
                }
            }
            _ => out.extend(chars[i..=end].iter()),
        }
        i = end + 1;
    }
    out
}

/// Короткая русская затравка по умолчанию. Даёт декодеру контекст языка/стиля
/// (грамотный русский с пунктуацией) и подавляет дрейф в латиницу, когда словарь
/// пользователя пуст. Намеренно нейтральная и КОРОТКАЯ, чтобы не «протекать» в
/// распознанный текст. Применяется только для русского языка (см. engine.rs).
pub const DEFAULT_RU_PROMPT: &str = "Распознавание русской речи. Грамотный текст с пунктуацией. Окончание записи не означает окончание предложения: не ставь финальную точку, если фраза грамматически не закончена.";

/// Короткий local-ASR bias из желаемых словарных форм и голосовых триггеров.
/// Тела сниппетов не включаются, чтобы decoder не галлюцинировал их в тишине.
///
/// `base` — необязательная языковая затравка (для ru — `DEFAULT_RU_PROMPT`),
/// чтобы и при ПУСТОМ словаре декодер получал контекст (раньше затравка была
/// пустой → бесполезной). Для не-русского языка `base` передаётся как None.
pub fn asr_bias_prompt(dict: &[Dict], snippets: &[Snippet], base: Option<&str>) -> Option<String> {
    const TERM_LIMIT: usize = 32;
    const TRIGGER_LIMIT: usize = 12;
    const MAX_CHARS: usize = 900;

    let mut terms = Vec::new();
    let mut seen = HashSet::new();
    for entry in dict {
        for value in [&entry.replacement, &entry.term] {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            if seen.insert(value.to_lowercase()) {
                terms.push(value);
                if terms.len() >= TERM_LIMIT {
                    break;
                }
            }
        }
        if terms.len() >= TERM_LIMIT {
            break;
        }
    }

    let mut triggers = Vec::new();
    let mut seen_triggers = HashSet::new();
    for snippet in snippets {
        let trigger = snippet.trigger.trim();
        if trigger.is_empty() {
            continue;
        }
        for value in [Some(trigger), trigger.strip_prefix('/').map(str::trim)]
            .into_iter()
            .flatten()
        {
            if !value.is_empty() && seen_triggers.insert(value.to_lowercase()) {
                triggers.push(value);
                if triggers.len() >= TRIGGER_LIMIT {
                    break;
                }
            }
        }
        if triggers.len() >= TRIGGER_LIMIT {
            break;
        }
    }

    let mut parts = Vec::new();
    if let Some(base) = base.map(str::trim).filter(|value| !value.is_empty()) {
        parts.push(base.to_string());
    }
    if !terms.is_empty() {
        parts.push(format!("Словарь: {}.", terms.join(", ")));
    }
    if !triggers.is_empty() {
        parts.push(format!("Голосовые триггеры: {}.", triggers.join(", ")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" ").chars().take(MAX_CHARS).collect())
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
    fn incidental_asr_breaks_do_not_create_paragraphs() {
        let s = st(true, true);
        assert_eq!(
            process("абзац один.\n\nабзац два, э-э, текст.", &s, &[], &[]),
            "Абзац один. Абзац два, текст."
        );
        assert_eq!(
            process("это должно\nбыть одним предложением", &s, &[], &[]),
            "Это должно быть одним предложением"
        );
        assert_eq!(normalize_spaces("а\n\n\n\nб"), "а\n\nб");
    }

    #[test]
    fn list_like_breaks_survive() {
        let s = st(false, true);
        assert_eq!(
            process("План:\n1. первое\n2. второе", &s, &[], &[]),
            "План:\n1. Первое\n2. Второе"
        );
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
        assert_eq!(
            process("first line.\n\nsecond line", &s, &[], &[]),
            "First line. Second line"
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
        assert_eq!(process("это это важно", &s, &[], &[]), "Это важно");
        assert_eq!(process("очень очень рад", &s, &[], &[]), "Очень очень рад");
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
    fn self_correction_collapses_incidental_lines() {
        let s = st(true, true);
        assert_eq!(
            process(
                "первая строка\nвстреча завтра, не так, послезавтра",
                &s,
                &[],
                &[]
            ),
            "Первая строка встреча послезавтра"
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
    fn unicode_case_expansion_before_multi_filler_is_boundary_safe() {
        let s = st(true, false);
        assert_eq!(process("İ как бы тест", &s, &[], &[]), "İ тест");
        assert_eq!(
            process("İ потому что тест", &s, &[], &[]),
            "İ потому что тест"
        );
    }

    #[test]
    fn false_sentence_breaks_after_short_pause_are_softened() {
        assert_eq!(
            soften_false_sentence_breaks("Я остановился. То есть продолжил мысль."),
            "Я остановился, то есть продолжил мысль."
        );
        assert_eq!(
            soften_false_sentence_breaks("Пауза была короткой. А текст пошёл дальше."),
            "Пауза была короткой, а текст пошёл дальше."
        );
        assert_eq!(
            soften_false_sentence_breaks("Готово. Следующая тема."),
            "Готово. Следующая тема."
        );
    }

    #[test]
    fn unfinished_clause_rejects_recognizer_added_period() {
        for (final_text, raw, expected) in [
            (
                "Я хотел сказать, что.",
                "я хотел сказать что",
                "Я хотел сказать, что",
            ),
            ("Я хочу, чтобы ты.", "я хочу чтобы ты", "Я хочу, чтобы ты"),
            (
                "Открой документ в.",
                "открой документ в",
                "Открой документ в",
            ),
            ("I think that we.", "I think that we", "I think that we"),
            (
                "Сравни варианты A и.",
                "сравни варианты A и",
                "Сравни варианты A и",
            ),
            (
                "Я остался, потому что.",
                "я остался потому что",
                "Я остался, потому что",
            ),
            (
                "I stopped because.",
                "I stopped because",
                "I stopped because",
            ),
        ] {
            assert_eq!(preserve_unfinished_ending(final_text, raw), expected);
            assert!(looks_unfinished_utterance(final_text));
        }
    }

    #[test]
    fn complete_or_explicit_end_is_never_removed() {
        assert_eq!(
            preserve_unfinished_ending("Релиз полностью готов.", "релиз полностью готов"),
            "Релиз полностью готов."
        );
        assert_eq!(
            preserve_unfinished_ending("Я хотел сказать, что.", "я хотел сказать что точка"),
            "Я хотел сказать, что."
        );
        assert_eq!(preserve_unfinished_ending("Что?", "что"), "Что?");
        assert!(!looks_unfinished_utterance("Что?"));
        assert!(!looks_unfinished_utterance(""));
        assert!(!looks_unfinished_utterance("."));
        assert!(!looks_unfinished_utterance("!!!"));
        for fragment in [
            "Я за.",
            "Если можно.",
            "Потому что я устал.",
            "Я думаю, что да.",
            "Я знаю, что делать.",
            "That works.",
            "I know that works.",
            "Log in.",
            "I'm in.",
            "I wanted that.",
            "I know where.",
            "I want to.",
            "The person I was with.",
            "Stay for a while.",
            "I don't know when.",
            "Я не знаю когда.",
            "Just because.",
            "Because.",
            "Потому что.",
            "Ну и что.",
            "Пока.",
            "До и после.",
            "Посмотри вокруг.",
        ] {
            assert!(!looks_unfinished_utterance(fragment), "{fragment}");
        }
    }

    #[test]
    fn proper_name_continuation_requires_positive_context() {
        assert!(continuation_may_start_with_proper_name(
            "Я приехал в",
            "Москву вчера"
        ));
        assert!(continuation_may_start_with_proper_name(
            "Я приехал в",
            "Нижний Новгород"
        ));
        assert!(continuation_may_start_with_proper_name(
            "Я еду в",
            "Москву завтра"
        ));
        for ordinary in ["Новой вкладке", "Старой версии", "Текущей папке"]
        {
            assert!(!continuation_may_start_with_proper_name(
                "Открой файл в",
                ordinary
            ));
        }
        assert!(!continuation_may_start_with_proper_name(
            "Напиши текст в",
            "Старой версии"
        ));
        assert!(!continuation_may_start_with_proper_name(
            "Я приехал вчера и открыл файл в",
            "Старой версии"
        ));
        assert!(!continuation_may_start_with_proper_name(
            "Запиши идею в",
            "Новом документе"
        ));
        assert!(!continuation_may_start_with_proper_name(
            "Положи еду в",
            "Старую коробку"
        ));
    }

    #[test]
    fn ellipsis_and_unclosed_brackets_remain_open_without_being_destroyed() {
        assert!(!looks_unfinished_utterance("Я ещё думаю…"));
        assert!(!looks_unfinished_utterance("«Я ещё думаю…»"));
        assert!(looks_unfinished_utterance("Я хотел сказать, что…"));
        assert!(looks_unfinished_utterance("Я хотел сказать, что..."));
        assert_eq!(
            preserve_unfinished_ending("Я хотел сказать, что…", "я хотел сказать что"),
            "Я хотел сказать, что…"
        );
        assert_eq!(
            preserve_unfinished_ending("Я хотел сказать, что...", "я хотел сказать что"),
            "Я хотел сказать, что..."
        );
        assert!(looks_unfinished_utterance("Проверь функцию (которая."));
        assert_eq!(
            preserve_unfinished_ending("Проверь функцию (которая.", "проверь функцию которая"),
            "Проверь функцию (которая"
        );
        for fragment in [
            "Проверь список [в котором.",
            "Проверь объект {который.",
            "Проверь ([вложенную конструкцию.",
        ] {
            assert!(looks_unfinished_utterance(fragment), "{fragment}");
        }
        for fragment in [
            "Проверь функцию (готово).",
            "Проверь список [готово].",
            "Проверь объект {готово}.",
        ] {
            assert!(!looks_unfinished_utterance(fragment), "{fragment}");
        }
    }

    #[test]
    fn unfinished_period_before_closers_is_removed_but_spoken_english_marks_survive() {
        assert_eq!(
            preserve_unfinished_ending("Я хотел сказать, что.»  ", "я хотел сказать что"),
            "Я хотел сказать, что»  "
        );
        for raw in [
            "i wanted to say that period",
            "i wanted to say that full stop",
            "i wanted to say that question mark",
            "i wanted to say that exclamation mark",
            "i wanted to say that ellipsis",
        ] {
            assert_eq!(
                preserve_unfinished_ending("I wanted to say that.", raw),
                "I wanted to say that.",
                "{raw}"
            );
        }
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

    #[test]
    fn unicode_case_expansion_never_uses_folded_byte_offsets_on_original_text() {
        assert_eq!(
            apply_corrections(
                "İ",
                &[Correction {
                    wrong: "i".into(),
                    right: "safe".into(),
                }]
            ),
            "İ"
        );
        assert_eq!(
            apply_corrections(
                "İ",
                &[Correction {
                    wrong: "İ".into(),
                    right: "Istanbul".into(),
                }]
            ),
            "Istanbul"
        );
    }

    #[test]
    fn learned_phrase_corrections_win_before_single_words() {
        let corrections = vec![
            Correction {
                wrong: "Виспа".into(),
                right: "Wispr".into(),
            },
            Correction {
                wrong: "Виспа Фолл".into(),
                right: "Wispr Flow".into(),
            },
        ];

        assert_eq!(
            apply_corrections("открой виспа фолл", &corrections),
            "открой Wispr Flow"
        );
    }

    #[test]
    fn unicode_and_spoken_slash_snippet_triggers_execute_whole_phrase_only() {
        let s = st(false, false);
        let snippets = vec![Snippet {
            trigger: "/Адрес".into(),
            content: "Москва".into(),
            is_template: false,
        }];
        assert_eq!(process("АДРЕС", &s, &[], &snippets), "Москва");
        assert_eq!(process("слэш адрес.", &s, &[], &snippets), "Москва");
        assert_eq!(process("«слеш-адрес»", &s, &[], &snippets), "Москва");
        assert_eq!(process("косая черта адрес", &s, &[], &snippets), "Москва");
        assert_eq!(process("сниппет адрес", &s, &[], &snippets), "Москва");
        assert_eq!(process("покажи адрес", &s, &[], &snippets), "покажи адрес");
    }

    #[test]
    fn exact_snippet_body_is_returned_without_postprocessing() {
        let snippets = vec![Snippet {
            trigger: "/raw".into(),
            content: "отмена\n  RAW {unknown}".into(),
            is_template: false,
        }];
        assert_eq!(
            expand_matching_snippet("slash raw", &snippets).as_deref(),
            Some("отмена\n  RAW {unknown}")
        );
    }

    #[test]
    fn template_expansion_is_deterministic_and_lossless() {
        assert_eq!(
            expand_template_with(
                "{DATE} {time} {clipboard} {unknown} {{date}}",
                "14.07.2026",
                "12:34",
                Some("исходный текст")
            ),
            "14.07.2026 12:34 исходный текст {unknown} {date}"
        );
        assert_eq!(
            expand_template_with(
                "{дата} {время} {буфер}",
                "14.07.2026",
                "12:34",
                Some("исходный текст")
            ),
            "14.07.2026 12:34 исходный текст"
        );
        assert_eq!(
            expand_template_with("X {clipboard} Y", "d", "t", None),
            "X {clipboard} Y"
        );
    }

    #[test]
    fn blank_dictionary_replacement_preserves_preferred_spelling() {
        let s = st(false, false);
        let dict = vec![Dict {
            term: "Wispr Flow".into(),
            replacement: String::new(),
        }];
        assert_eq!(
            process("открой wispr flow", &s, &dict, &[]),
            "открой Wispr Flow"
        );
    }

    #[test]
    fn overlapping_dictionary_terms_use_longest_match_without_cascading() {
        let s = st(false, false);
        let dict = vec![
            Dict {
                term: "wispr".into(),
                replacement: "X".into(),
            },
            Dict {
                term: "wispr flow".into(),
                replacement: "Wispr Flow".into(),
            },
        ];

        assert_eq!(
            process("wispr flow and wispr", &s, &dict, &[]),
            "Wispr Flow and X"
        );
    }

    #[test]
    fn local_asr_bias_contains_terms_and_triggers_but_not_snippet_body() {
        let dict = vec![
            Dict {
                term: "виспр флоу".into(),
                replacement: "Wispr Flow".into(),
            },
            Dict {
                term: "WISPR FLOW".into(),
                replacement: "Wispr Flow".into(),
            },
        ];
        let snippets = vec![Snippet {
            trigger: "/адрес".into(),
            content: "Секретное тело сниппета".into(),
            is_template: false,
        }];
        let prompt = asr_bias_prompt(&dict, &snippets, Some(DEFAULT_RU_PROMPT)).expect("prompt");
        assert!(prompt.contains("Wispr Flow"));
        assert!(prompt.contains("виспр флоу"));
        assert_eq!(prompt.matches("Wispr Flow").count(), 1);
        assert!(prompt.contains("/адрес"));
        assert!(prompt.contains("адрес"));
        assert!(!prompt.contains("Секретное тело сниппета"));
        assert!(prompt.chars().count() <= 900);
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
            dedup_repeated_ngrams("отправь отчёт отправь отчёт отправь отчёт пожалуйста"),
            "отправь отчёт пожалуйста"
        );
    }

    #[test]
    fn even_copies_collapse_to_one() {
        assert_eq!(
            dedup_repeated_ngrams("раз два раз два раз два раз два три"),
            "раз два три"
        );
    }

    #[test]
    fn long_decoder_loops_collapse_to_the_primitive_phrase() {
        assert_eq!(
            dedup_repeated_ngrams("раз два раз два раз два раз два раз два раз два три"),
            "раз два три"
        );
        assert_eq!(
            dedup_repeated_ngrams(
                "раз два раз два раз два раз два раз два раз два раз два раз два три"
            ),
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
    fn two_complete_sentences_are_not_treated_as_a_decoder_loop() {
        // A deliberate repetition is user speech, not proof of a decoder loop.
        assert_eq!(
            dedup_repeated_ngrams("Я пошёл домой. я пошёл домой."),
            "Я пошёл домой. я пошёл домой."
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
            "абзац один абзац один\n\nабзац два"
        );
    }
}
