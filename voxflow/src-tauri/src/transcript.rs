//! Санитарная обработка текста распознавания: схлопнуть пробелы и петли
//! повторов, выбросить ответ из одних пометок не-речи (`[music]`, `♪`).
//! Остаток модуля whisper.cpp: облачный STT отдаёт такие же ответы (D-065).

/// Единая дешёвая санитарная обработка финального ASR-текста.
/// Она не переписывает фразу: только схлопывает пробелы/повторы и убирает
/// ответы, целиком состоящие из non-speech аннотаций (`[music]`, `(noise)`, `♪`).
pub(crate) fn sanitize_transcript_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let deduped = dedup_repeats(&collapsed);
    let normalized = crate::postprocess::normalize_spaces(&deduped);
    if is_non_speech_only(&normalized) {
        String::new()
    } else {
        normalized
    }
}

fn is_non_speech_only(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return true;
    }

    let mut saw_music = false;
    let only_music_or_punctuation = text.chars().all(|c| {
        if matches!(c, '♪' | '♫' | '♬' | '♩' | '🎵' | '🎶') {
            saw_music = true;
            true
        } else {
            c.is_whitespace()
                || c.is_ascii_punctuation()
                || matches!(c, '…' | '—' | '–' | '·' | '•')
        }
    });
    if saw_music && only_music_or_punctuation {
        return true;
    }

    // Whisper часто обозначает не-речь скобками, но скобки сами по себе не
    // означают шум: `[TODO]`, `(важно)` и `<div>` — нормальная диктовка.
    // Отклоняем только полностью аннотационный ответ из известного списка.
    let mut chars = text.chars().peekable();
    let mut saw_group = false;
    loop {
        while chars.peek().is_some_and(|c| {
            c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '-' | '—' | '–' | '…')
        }) {
            chars.next();
        }
        let Some(open) = chars.next() else {
            return saw_group;
        };
        let close = match open {
            '[' => ']',
            '(' => ')',
            '<' => '>',
            '【' => '】',
            _ => return false,
        };
        let mut closed = false;
        let mut content = String::new();
        for c in chars.by_ref() {
            if c == close {
                closed = true;
                break;
            }
            content.push(c);
        }
        if !closed || !is_known_non_speech_marker(&content) {
            return false;
        }
        saw_group = true;
    }
}

fn is_known_non_speech_marker(content: &str) -> bool {
    let normalized = content
        .trim()
        .chars()
        .map(|c| if matches!(c, '_' | '-') { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    matches!(
        normalized.as_str(),
        "blank audio"
            | "no audio"
            | "silence"
            | "silent"
            | "noise"
            | "background noise"
            | "music"
            | "applause"
            | "laughter"
            | "laughing"
            | "breathing"
            | "inaudible"
            | "unintelligible"
            | "sound effect"
            | "sound effects"
            | "нет звука"
            | "тишина"
            | "шум"
            | "фоновый шум"
            | "музыка"
            | "аплодисменты"
            | "смех"
            | "дыхание"
            | "неразборчиво"
            | "неразборчивая речь"
            | "звуковой эффект"
    )
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
fn dedup_repeats(text: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_rejects_only_annotation_only_transcripts() {
        for text in [
            "[BLANK_AUDIO]",
            "(music)",
            "♪ ♪",
            "[noise], (applause)",
            "【фоновый шум】",
        ] {
            assert!(sanitize_transcript_text(text).is_empty(), "{text}");
        }
        assert_eq!(
            sanitize_transcript_text("Начало текста [шум]"),
            "Начало текста [шум]"
        );
        for text in ["[TODO]", "(важно)", "<div>", "【проверить позже】"] {
            assert_eq!(sanitize_transcript_text(text), text, "{text}");
        }
    }
}
