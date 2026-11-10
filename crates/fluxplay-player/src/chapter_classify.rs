//! Chapter-title heuristics for intro / recap / credits skip pills.
//!
//! Pure string logic — safe to proptest / libFuzzer without a media backend.
//! Optional extras come from the app [`PolicyPack`] via [`set_chapter_keyword_extras`].

use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterClass {
    Intro,
    Recap,
    Credits,
    Other,
}

#[derive(Default)]
struct KeywordExtras {
    intro: Vec<String>,
    credits: Vec<String>,
    recap: Vec<String>,
}

fn extras() -> &'static Mutex<KeywordExtras> {
    static E: OnceLock<Mutex<KeywordExtras>> = OnceLock::new();
    E.get_or_init(|| Mutex::new(KeywordExtras::default()))
}

/// Merge policy-provided keywords (already lowercased preferred). Empty clears extras.
pub fn set_chapter_keyword_extras(intro: Vec<String>, credits: Vec<String>, recap: Vec<String>) {
    if let Ok(mut g) = extras().lock() {
        g.intro = intro
            .into_iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        g.credits = credits
            .into_iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        g.recap = recap
            .into_iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
    }
}

fn extra_hit(bucket: &[String], folded: &str) -> bool {
    bucket.iter().any(|k| folded.contains(k.as_str()))
}

fn fold_chapter_title(title: &str) -> String {
    let mut folded = String::new();
    for c in title.chars() {
        let c = match c {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'à' | 'â' | 'ä' => 'a',
            'ù' | 'û' | 'ü' => 'u',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ç' => 'c',
            other => other.to_ascii_lowercase(),
        };
        folded.push(c);
    }
    format!(" {folded} ")
}

/// Classify a chapter / marker title into skip-related buckets.
pub fn classify_chapter_title(title: &str) -> ChapterClass {
    let t = fold_chapter_title(title);
    // "Opening Credits" / "Main Title" are intros — check before bare "credits".
    const INTRO: &[&str] = &[
        " intro",
        "intro ",
        "opening",
        "generique",
        "generic",
        " op ",
        "o.p",
        "opening theme",
        "theme song",
        "main title",
        "title sequence",
        "logo",
        "studio",
        "prologue",
        "cold open",
    ];
    let extras_guard = extras().lock().ok();
    let (ex_intro, ex_credits, ex_recap) = extras_guard
        .as_ref()
        .map(|g| (g.intro.as_slice(), g.credits.as_slice(), g.recap.as_slice()))
        .unwrap_or((&[], &[], &[]));

    if INTRO.iter().any(|k| t.contains(k)) || extra_hit(ex_intro, &t) || t.trim() == "op" {
        // "Générique de fin" also contains "generique" — keep as credits.
        const FIN: &[&str] = &[
            " de fin",
            "ending",
            "outro",
            "closing",
            "end credit",
            " ed ",
        ];
        if !(FIN.iter().any(|k| t.contains(k)) || t.trim() == "ed") {
            return ChapterClass::Intro;
        }
    }
    const CREDITS: &[&str] = &[
        "ending",
        "outro",
        "end credit",
        "endcredit",
        "closing credit",
        "credits de fin",
        "credit de fin",
        "generique de fin",
        "generic de fin",
        " ed ",
        "e.d",
        "ending theme",
        "closing theme",
        " end credits",
        " credits",
    ];
    if CREDITS.iter().any(|k| t.contains(k)) || extra_hit(ex_credits, &t) || t.trim() == "ed" {
        return ChapterClass::Credits;
    }
    const RECAP: &[&str] = &[
        "recap",
        "previously",
        "previously on",
        "last time",
        "resume precedent",
        "resume de l'episode",
        "avant l'episode",
    ];
    if RECAP.iter().any(|k| t.contains(k)) || extra_hit(ex_recap, &t) {
        return ChapterClass::Recap;
    }
    ChapterClass::Other
}

/// Best early intro/recap window from a chapter list. Returns `(start, end, class)`.
pub fn opening_from_chapters(
    chapters: &[(f64, String)],
    series: bool,
) -> Option<(f64, f64, ChapterClass)> {
    let guess = if series { 90.0 } else { 25.0 };
    // Prefer an early intro; fall back to a recap ("Previously on…").
    let mut best: Option<(f64, f64, ChapterClass)> = None;
    for (i, (start, title)) in chapters.iter().enumerate() {
        let kind = match classify_chapter_title(title) {
            ChapterClass::Intro => ChapterClass::Intro,
            ChapterClass::Recap => ChapterClass::Recap,
            _ => continue,
        };
        let next = chapters.get(i + 1).map(|(t, _)| *t);
        let end = next.unwrap_or(start + guess).min(start + 150.0);
        if end <= start + 3.0 {
            continue;
        }
        let span = (*start, end, kind);
        let replace = match best {
            None => true,
            Some((bs, _, bk)) => match kind {
                ChapterClass::Intro => bk == ChapterClass::Recap || span.0 < bs,
                ChapterClass::Recap => bk == ChapterClass::Recap && span.0 < bs,
                _ => false,
            },
        };
        if replace {
            best = Some(span);
        }
    }
    best
}

/// Earliest credits marker in the last half of the file (when duration known).
pub fn credits_from_chapters(chapters: &[(f64, String)], duration: f64) -> Option<f64> {
    let mut best: Option<f64> = None;
    for (start, title) in chapters {
        if !matches!(classify_chapter_title(title), ChapterClass::Credits) {
            continue;
        }
        if duration > 60.0 && *start < duration * 0.55 {
            continue;
        }
        best = Some(best.map_or(*start, |b| b.min(*start)));
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_basics() {
        assert_eq!(classify_chapter_title("Opening Credits"), ChapterClass::Intro);
        assert_eq!(classify_chapter_title("Générique"), ChapterClass::Intro);
        assert_eq!(classify_chapter_title("OP"), ChapterClass::Intro);
        assert_eq!(classify_chapter_title("Previously on"), ChapterClass::Recap);
        assert_eq!(classify_chapter_title("End Credits"), ChapterClass::Credits);
        assert_eq!(classify_chapter_title("Générique de fin"), ChapterClass::Credits);
        assert_eq!(classify_chapter_title("ED"), ChapterClass::Credits);
        assert_eq!(classify_chapter_title("Chapter 3"), ChapterClass::Other);
    }

    #[test]
    fn opening_prefers_intro() {
        let chapters = vec![
            (0.0, "Previously on".into()),
            (45.0, "Opening".into()),
            (120.0, "Act 1".into()),
        ];
        let (s, e, k) = opening_from_chapters(&chapters, true).unwrap();
        assert_eq!(k, ChapterClass::Intro);
        assert!((s - 45.0).abs() < f64::EPSILON);
        assert!(e > s);
    }
}
