//! Catalog name parsing — IPTV portal naming conventions → clean labels + audio language.
//!
//! Portals encode region / language in free text (patterns from real catalogs):
//! - categories: `|FR| ✪ SPORT ᵁᴴᴰ`, `FR| ✪ DAZN LALIGA ES`, `XXX | ✪ FOR ADULTS`,
//!   `✪ ORIGINAL NETFLIX  MULTI`, `|BE| ✪ VLAAMS`, `|CA| ✪ QUEBEC CINEMA`
//! - films: `FR - Title (2024)`, `AR - AR - Title - 2001`, `ES - Title - 2017_esp`,
//!   `FR - Title (2023) VOST`, `IN:CAM -Title - 2025`, `Pet.2016.1080p.WEB-DL`
//! - series: `Stranger Things_fr`, `Suburra - La Serie-it`, `Anne Boleyn (2021) VP`,
//!   `The Legend of Vox Machina (MULTI)-fr`
//! - channels: `|FR| MTV ᴴᴰ`, `ES: CANAL EXTREMADURA`, `NOW| SKY CINEMA`,
//!   `PRIME-DE | No Event`, `:AHL  15` (empty PPV slot)
//!
//! Region codes are provider-specific: here `AR` is Arabic (not Argentina), `AF` is
//! Canal+ Afrique, `EX` ex-Yugoslavia, `SW` Switzerland, `SL` Hebrew, `NOW` Sky/WOW DE.

/// Languages the viewer can pick: ISO 639-1 code + French label.
/// `sh` groups Serbo-Croatian/Bosnian/Montenegrin, `hi` the Indian-subcontinent catalogs.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("fr", "Français"),
    ("en", "Anglais"),
    ("de", "Allemand"),
    ("es", "Espagnol"),
    ("it", "Italien"),
    ("pt", "Portugais"),
    ("nl", "Néerlandais"),
    ("ar", "Arabe"),
    ("tr", "Turc"),
    ("pl", "Polonais"),
    ("sv", "Suédois"),
    ("no", "Norvégien"),
    ("da", "Danois"),
    ("fi", "Finnois"),
    ("el", "Grec"),
    ("ru", "Russe"),
    ("ro", "Roumain"),
    ("bg", "Bulgare"),
    ("hu", "Hongrois"),
    ("cs", "Tchèque"),
    ("sk", "Slovaque"),
    ("sq", "Albanais"),
    ("sh", "Serbo-croate"),
    ("uk", "Ukrainien"),
    ("hi", "Hindi / langues indiennes"),
    ("fa", "Persan"),
    ("ku", "Kurde"),
    ("he", "Hébreu"),
    ("ja", "Japonais"),
    ("ko", "Coréen"),
    ("zh", "Chinois"),
    ("th", "Thaï"),
    ("tl", "Filipino"),
    ("vi", "Vietnamien"),
    ("id", "Indonésien"),
    ("hy", "Arménien"),
    ("az", "Azéri"),
    ("lt", "Lituanien"),
    ("lv", "Letton"),
    ("et", "Estonien"),
    ("is", "Islandais"),
];

const NORDIC: &[&str] = &["sv", "no", "da", "fi"];

pub fn lang_label(code: &str) -> Option<&'static str> {
    LANGUAGES.iter().find(|(c, _)| *c == code).map(|(_, l)| *l)
}

/// Google Translate target code for a [`LANGUAGES`] entry.
pub fn translate_code(code: &str) -> &str {
    match code {
        "sh" => "hr",
        other => other,
    }
}

/// Bit set over [`LANGUAGES`].
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Langs(u64);

impl Langs {
    fn bit(code: &str) -> u64 {
        LANGUAGES
            .iter()
            .position(|(c, _)| *c == code)
            .map(|i| 1u64 << i)
            .unwrap_or(0)
    }

    pub fn of(codes: &[&str]) -> Self {
        Langs(codes.iter().fold(0, |acc, c| acc | Self::bit(c)))
    }

    pub fn has(self, code: &str) -> bool {
        let b = Self::bit(code);
        b != 0 && self.0 & b != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn add(&mut self, other: Langs) {
        self.0 |= other.0;
    }
}

/// What a name says about the audio language.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct LangInfo {
    pub langs: Langs,
    /// Multi-audio release (`MULTI`) with no single language named.
    pub multi: bool,
    pub adult: bool,
}

impl LangInfo {
    pub fn is_known(&self) -> bool {
        !self.langs.is_empty() || self.multi
    }

    /// `MULTI` only counts as "any language" when no language is named —
    /// `|TR| ✪ DUBLAJ MULTI` is still a Turkish catalog.
    pub fn matches(&self, lang: &str) -> bool {
        self.langs.has(lang) || (self.langs.is_empty() && self.multi)
    }

    fn merge(&mut self, other: LangInfo) {
        self.langs.add(other.langs);
        self.multi |= other.multi;
        self.adult |= other.adult;
    }
}

/// Sort rank for a category / item against the viewer language (lower first).
pub fn lang_rank(info: &LangInfo, pref: &str) -> u8 {
    if info.adult {
        3
    } else if pref.is_empty() {
        1
    } else if info.langs.has(pref) {
        0
    } else if info.langs.is_empty() && info.multi {
        1
    } else {
        2
    }
}

/// Region / language code (uppercase) → languages. `None` for unknown codes.
fn code_info(code: &str) -> Option<LangInfo> {
    let code = code.trim().to_ascii_uppercase();
    let langs: &[&str] = match code.as_str() {
        "XXX" | "XX" | "+18" | "18+" | "ADULT" | "ADULTS" => {
            return Some(LangInfo {
                adult: true,
                ..Default::default()
            })
        }
        "MULTI" => {
            return Some(LangInfo {
                multi: true,
                ..Default::default()
            })
        }
        "FR" | "VF" | "VOSTFR" | "VOST" | "AF" | "SN" | "QC" => &["fr"],
        "BE" => &["fr", "nl"],
        "CA" => &["en", "fr"],
        "CH" | "SW" => &["de", "fr", "it"],
        "MA" => &["ar", "fr"],
        "EN" | "ENG" | "UK" | "GB" | "US" | "USA" | "AU" | "AUS" | "NZ" | "IE" => &["en"],
        "DE" | "AT" | "NOW" => &["de"],
        "ES" | "ESP" | "LAT" | "MX" | "LATAM" => &["es"],
        "IT" => &["it"],
        "PT" | "BR" => &["pt"],
        "NL" => &["nl"],
        "AR" | "ARA" => &["ar"],
        "TR" => &["tr"],
        "PL" => &["pl"],
        "SE" => &["sv"],
        "NO" => &["no"],
        "DK" => &["da"],
        "FI" => &["fi"],
        "NORDEC" | "NORDIC" | "NRC" | "SC" => NORDIC,
        "GR" | "CY" => &["el"],
        "RU" => &["ru"],
        "RO" => &["ro"],
        "BG" => &["bg"],
        "HU" => &["hu"],
        "CZ" => &["cs"],
        "SK" => &["sk"],
        "SLO" => &["sk", "cs"],
        "AL" | "ALB" => &["sq"],
        "EX" | "EX-YU" | "EXYU" | "YU" | "ME" | "RS" | "HR" | "BA" => &["sh"],
        "UA" => &["uk"],
        "IN" | "PK" | "HI" | "BANGLA" => &["hi"],
        "IR" => &["fa"],
        "KD" | "KU" => &["ku"],
        "ISL" | "ISR" | "IL" | "SL" => &["he"],
        "JP" => &["ja"],
        "KO" | "KR" => &["ko"],
        "CN" | "TW" => &["zh"],
        "TH" => &["th"],
        "PH" => &["tl"],
        "VT" | "VE" | "VN" => &["vi"],
        "AM" => &["hy"],
        "AZ" => &["az"],
        "LT" => &["lt"],
        "LV" => &["lv"],
        "EST" | "EE" => &["et"],
        "IC" | "IS" => &["is"],
        _ => {
            // Compound codes: `PRIME-DE`, `IN-EN`, `IN:CAM`, `UK.VIP`.
            let parts: Vec<&str> = code
                .split(['-', ':', '.'])
                .filter(|p| !p.is_empty())
                .collect();
            if parts.len() > 1 {
                let mut out = LangInfo::default();
                for p in parts {
                    if let Some(i) = code_info(p) {
                        out.merge(i);
                    }
                }
                return out.is_known().then_some(out);
            }
            return None;
        }
    };
    Some(LangInfo {
        langs: Langs::of(langs),
        ..Default::default()
    })
}

/// Platform / quality tags seen as item prefixes (`NF - `, `DS - `, `XXX - `).
fn is_platform_tag(code: &str) -> bool {
    matches!(
        code.to_ascii_uppercase().as_str(),
        "NF" | "AP" | "DS" | "DST" | "TOD" | "DSTV" | "HBO" | "OSN" | "CAM" | "TS" | "4K" | "4KL" | "HD"
            | "UHD" | "FHD" | "SD" | "AMZ" | "TB" | "VP" | "ATSP" | "SUB" | "VO" | "RAW"
    )
}

/// Category-name words that name a language (checked on ASCII-folded uppercase words).
fn keyword_langs(word: &str) -> Option<&'static [&'static str]> {
    Some(match word {
        "FRENCH" | "FRANCAIS" | "FRANCAISE" | "FRANCAISES" | "QUEBEC" | "QUEBECOIS"
        | "QUEBECOISE" => &["fr"],
        "VLAAMS" | "FLEMISH" => &["nl"],
        "ENGLISH" => &["en"],
        "ARABIC" => &["ar"],
        "SPANISH" | "ESPANOL" | "ESPANOLAS" | "LATINO" => &["es"],
        "GERMAN" | "DEUTSCH" => &["de"],
        "ITALIAN" | "ITALIANA" | "ITALIANO" => &["it"],
        "TURKISH" => &["tr"],
        "HINDI" | "PUNJABI" | "TAMIL" | "TELUGU" | "BANGLA" | "BANGALA" | "BENGALI"
        | "MALAYALAM" | "URDU" | "PAKISTAN" | "PAKISTANI" | "INDIA" | "KANNADA" | "MARATHI"
        | "GUJARAT" | "GUJARATI" => &["hi"],
        "INDONESIA" => &["id"],
        "CHINA" | "CHINESE" | "TAIWAN" => &["zh"],
        "KOREA" | "KOREAN" => &["ko"],
        "JAPAN" | "JAPANESE" => &["ja"],
        "VIETNAM" | "VETNAM" => &["vi"],
        "THAILAND" => &["th"],
        "PHILIPINE" | "PHILIPPINE" | "FILIPINO" => &["tl"],
        "HEBREW" => &["he"],
        "KURDISH" | "KURDISTAN" | "SORANI" => &["ku"],
        "ICELAND" => &["is"],
        "ARMENIA" => &["hy"],
        "NORDIC" => NORDIC,
        "SVENSKA" => &["sv"],
        "DANSKE" | "DANISH" => &["da"],
        "NORDKA" | "NORSK" => &["no"],
        "SUOMALAINEN" | "SUOMI" => &["fi"],
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

/// Modifier letters used as quality badges (`ᵁᴴᴰ`, `ᴴᴰ`, `ˢᴰ`, `ᶠᴴᴰ`).
fn desuperscript(c: char) -> Option<char> {
    Some(match c {
        'ᴬ' => 'A',
        'ᴮ' => 'B',
        'ᶜ' => 'C',
        'ᴰ' => 'D',
        'ᴱ' => 'E',
        'ᶠ' => 'F',
        'ᴳ' => 'G',
        'ᴴ' => 'H',
        'ᴵ' => 'I',
        'ᴶ' => 'J',
        'ᴷ' => 'K',
        'ᴸ' => 'L',
        'ᴹ' => 'M',
        'ᴺ' => 'N',
        'ᴼ' => 'O',
        'ᴾ' => 'P',
        'ᴿ' => 'R',
        'ˢ' => 'S',
        'ᵀ' => 'T',
        'ᵁ' => 'U',
        'ⱽ' => 'V',
        'ᵂ' => 'W',
        '⁰' => '0',
        '¹' => '1',
        '²' => '2',
        '³' => '3',
        '⁴' => '4',
        '⁵' => '5',
        '⁶' => '6',
        '⁷' => '7',
        '⁸' => '8',
        '⁹' => '9',
        _ => return None,
    })
}

/// Decorations → plain text: superscript badges, `✪`, `▎`, runs of spaces.
fn clean_decorations(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_sup = false;
    for c in s.chars() {
        if let Some(plain) = desuperscript(c) {
            if !in_sup && !out.ends_with([' ', '/']) && !out.is_empty() {
                out.push(' ');
            }
            in_sup = true;
            out.push(plain);
            continue;
        }
        in_sup = false;
        match c {
            '✪' | '★' | '☆' | '▎' | '•' | '●' | '◉' | '\u{FE0F}' => out.push(' '),
            '_' => out.push(' '),
            _ => out.push(c),
        }
    }
    collapse_ws(&out)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fold_upper(word: &str) -> String {
    word.chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ä' | 'À' | 'Á' | 'Â' | 'Ä' => 'A',
            'ç' | 'Ç' => 'C',
            'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'E',
            'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => 'I',
            'ñ' | 'Ñ' => 'N',
            'ò' | 'ó' | 'ô' | 'ö' | 'Ò' | 'Ó' | 'Ô' | 'Ö' => 'O',
            'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => 'U',
            c => c.to_ascii_uppercase(),
        })
        .collect()
}

/// Short words that read better title-cased (`FOR ADULTS` → `For Adults`).
const SHORT_WORDS: &[&str] = &[
    "THE", "AND", "FOR", "ALL", "NEW", "TOP", "OF", "LES", "DES", "DU", "DE", "LA", "LE",
    "ET", "EL", "LOS", "DEL", "MY", "ONE", "BOX", "FUN", "KID", "AL", "DI", "DA", "UND",
    "DER", "DIE", "DAS", "MIX", "LIVE",
];

/// Acronyms / brands kept as-is when a label is title-cased.
fn fixed_case(word_upper: &str) -> Option<&'static str> {
    Some(match word_upper {
        "BEIN" => "beIN",
        "DAZN" => "DAZN",
        "ESPN" => "ESPN",
        "UEFA" => "UEFA",
        "FIFA" => "FIFA",
        "VOST" => "VOST",
        "VOSTFR" => "VOSTFR",
        "NASCAR" => "NASCAR",
        "WWE" => "WWE",
        "MBC" => "MBC",
        "OSN" => "OSN",
        "RTL" => "RTL",
        "RTL+" => "RTL+",
        "UHD" => "UHD",
        "FHD" => "FHD",
        "DSTV" => "DStv",
        "IMAX" => "IMAX",
        "BLURAY" => "Blu-ray",
        _ => return None,
    })
}

/// Title-case a label only when it is SHOUTED (portal habit); mixed case is kept.
pub fn titlecase_if_shouting(s: &str) -> String {
    let (mut up, mut low) = (0usize, 0usize);
    for c in s.chars() {
        if c.is_uppercase() {
            up += 1;
        } else if c.is_lowercase() {
            low += 1;
        }
    }
    if up < 4 || low * 5 > up {
        return s.to_string();
    }
    s.split(' ')
        .map(|w| {
            let letters = w.chars().filter(|c| c.is_alphabetic()).count();
            let upper = fold_upper(w);
            let bare = upper.trim_matches(|c: char| !c.is_alphanumeric() && c != '+');
            if let Some(f) = fixed_case(bare) {
                return w.replacen(bare, f, 1);
            }
            let badge = bare.contains('/')
                && bare
                    .split('/')
                    .all(|p| p.len() <= 3 || fixed_case(p).is_some());
            if badge
                || letters == 0
                || w.chars().any(|c| c.is_ascii_digit())
                || (letters <= 3 && !SHORT_WORDS.contains(&bare))
            {
                return w.to_string();
            }
            let mut out = String::with_capacity(w.len());
            let mut boundary = true;
            for c in w.chars() {
                if c.is_alphabetic() {
                    if boundary {
                        out.extend(c.to_uppercase());
                    } else if c == 'İ' {
                        // Turkish dotted capital: `to_lowercase` adds a combining dot.
                        out.push('i');
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    boundary = false;
                } else {
                    out.push(c);
                    boundary = matches!(c, '-' | '/' | '(' | '.' | '&' | '"' | '|');
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_code_token(s: &str) -> bool {
    let n = s.chars().count();
    (1..=8).contains(&n)
        && s
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '-' | '+'))
        && s.chars().any(|c| c.is_ascii_alphabetic() || c == '+')
}

/// `|FR| rest`, `FR| rest`, `FR|| rest`, `XXX | rest` → (`FR`, `rest`).
fn split_pipe_code(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    fn after_code(r: &str) -> &str {
        r.trim_start_matches(|c: char| c == '|' || c.is_whitespace())
    }
    if s.starts_with('|') {
        let inner = s.trim_start_matches('|');
        let j = inner.find('|')?;
        let code = inner[..j].trim();
        let rest = after_code(&inner[j + 1..]);
        // Also region words: `|Venezuela|`, `|SIERRALEONE|`, `|UK.VIP|`, `|MYᴴᴰ|`.
        let region_word = (2..=16).contains(&code.chars().count())
            && code
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '+'))
            && code.chars().any(char::is_alphabetic);
        let code_ok = region_word || is_code_token(&code.to_ascii_uppercase());
        return code_ok.then_some((code, rest));
    }
    let j = s.find('|')?;
    if j > 10 {
        return None;
    }
    let code = s[..j].trim();
    let rest = after_code(&s[j + 1..]);
    is_code_token(code).then_some((code, rest))
}

// ---------------------------------------------------------------------------
// Categories
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryInfo {
    /// Region / language code as written by the portal (`FR`, `EX-Yu`, `+18`).
    pub code: Option<String>,
    /// Decorations removed, title-cased (`Chaines Francaises UHD`).
    pub title: String,
    pub lang: LangInfo,
}

pub fn parse_category(raw: &str) -> CategoryInfo {
    let (code, rest) = match split_pipe_code(raw) {
        Some((c, r)) => (Some(c.to_string()), r),
        None => (None, raw),
    };
    let cleaned = clean_decorations(rest);
    let mut lang = code.as_deref().and_then(code_info).unwrap_or_default();

    let mut kw = LangInfo::default();
    let words: Vec<String> = cleaned
        .split(|c: char| !c.is_alphanumeric() && c != '+')
        .filter(|w| !w.is_empty())
        .map(fold_upper)
        .collect();
    for (i, w) in words.iter().enumerate() {
        if let Some(l) = keyword_langs(w) {
            kw.langs.add(Langs::of(l));
        }
        match w.as_str() {
            // `MULTI SUB` = subtitles only, audio stays the category language.
            "MULTI" if words.get(i + 1).map(|n| n == "SUB").unwrap_or(false) => {}
            "MULTI" => lang.multi = true,
            "ADULT" | "ADULTS" | "XXX" | "HANIME" => lang.adult = true,
            _ => {}
        }
    }
    // A language word narrows a multi-language region (`|BE| ✪ VLAAMS`, `|CA| ✪ QUEBEC`).
    if !kw.langs.is_empty() {
        lang.langs = kw.langs;
    }

    let title = titlecase_if_shouting(cleaned.trim_matches(|c: char| c == '-' || c == '|' || c == ' '));
    CategoryInfo {
        code,
        title,
        lang,
    }
}

/// Sidebar label: `FR · Sport UHD`, `Original Netflix Multi`, `18+ · Adults`.
pub fn category_label(raw: &str) -> String {
    let info = parse_category(raw);
    if info.title.is_empty() {
        return collapse_ws(raw);
    }
    match info.code.as_deref() {
        Some(c) if c == "+18" => format!("18+ · {}", info.title),
        Some(c) => format!("{} · {}", c.to_ascii_uppercase(), info.title),
        None => info.title,
    }
}

// ---------------------------------------------------------------------------
// Films / series
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TitleInfo {
    pub title: String,
    pub year: Option<String>,
    pub lang: LangInfo,
}

fn year_of(t: &str) -> Option<String> {
    let t = t.trim();
    if t.len() == 4 && t.chars().all(|c| c.is_ascii_digit()) {
        let y: u16 = t.parse().ok()?;
        if (1900..=2100).contains(&y) {
            return Some(t.to_string());
        }
    }
    None
}

/// Item prefix `FR - `, `IN:CAM -`, `XXX - `, `FR -4KL ` → (code, rest).
fn split_item_prefix(s: &str) -> Option<(&str, &str)> {
    let mut j = s.find('-')?;
    // Glued sub-region: `IN-TE - Title` (Telugu), `IN-TA - Title`.
    if !s[..j].ends_with(' ') {
        if let Some(k) = s[j + 1..].find(" -").map(|k| k + j + 1) {
            let sub = &s[j + 1..k];
            if (2..=3).contains(&sub.len()) && is_code_token(sub) && code_info(&s[..j]).is_some()
            {
                j = k + 1;
            }
        }
    }
    let head = s[..j].trim();
    if head.is_empty() || head.len() > 7 {
        return None;
    }
    let valid = head.split(':').all(|p| {
        is_code_token(p)
            && (code_info(p).is_some()
                || is_platform_tag(p)
                || p.split_once('-').is_some_and(|(a, _)| code_info(a).is_some()))
    });
    if !valid {
        return None;
    }
    let rest = s[j + 1..].trim_start_matches('-').trim_start();
    (!rest.is_empty()).then_some((head, rest))
}

/// Strip a repeated prefix written in another case: `RU - Ru - Title`.
fn strip_repeated_prefix<'a>(s: &'a str, head: &str) -> &'a str {
    let n = head.len();
    match (s.get(..n), s.get(n..)) {
        (Some(h), Some(rest)) if h.eq_ignore_ascii_case(head) && rest.starts_with(" - ") => {
            rest[3..].trim_start()
        }
        _ => s,
    }
}

/// Glued suffixes (`_esp`, `-fr`, `--it`, `(MULTI)`) and their language.
const GLUED_SUFFIXES: &[(&str, &str)] = &[
    ("_vostfr", "fr"),
    ("-vostfr", "fr"),
    ("(vostfr)", "fr"),
    ("(vost)", "fr"),
    ("(vf)", "fr"),
    ("_multi", "multi"),
    ("-multi", "multi"),
    ("(multi)", "multi"),
    ("[multi-sub]", ""),
    ("_sub", ""),
    ("-sub", ""),
    ("_fr", "fr"),
    ("-fr", "fr"),
    ("_vf", "fr"),
    ("-vf", "fr"),
    ("_vo", ""),
    ("-vo", ""),
    ("_eng", "en"),
    ("-eng", "en"),
    ("_en", "en"),
    ("_esp", "es"),
    ("-esp", "es"),
    ("_es", "es"),
    ("-es", "es"),
    ("_mx", "es"),
    ("-mx", "es"),
    ("--it", "it"),
    ("-it", "it"),
    ("_it", "it"),
    ("--de", "de"),
    ("-de", "de"),
    ("_de", "de"),
    ("_tr", "tr"),
    ("-hi", "hi"),
    ("_hi", "hi"),
    ("_ind", "hi"),
    ("_qb", "fr"),
    ("-qb", "fr"),
    ("_nl", "nl"),
    ("_pl", "pl"),
    ("_se", "sv"),
    ("_msub", ""),
    ("_hd", ""),
    ("(la)", "es"),
    ("_as", ""),
];

/// Glued suffixes only recognised in uppercase (`Doom Patrol-SE`).
const GLUED_UPPER_SUFFIXES: &[(&str, &str)] = &[
    ("-SE", "sv"),
    ("-NL", "nl"),
    ("-PL", "pl"),
    ("-NO", "no"),
    ("-DK", "da"),
    ("-FI", "fi"),
];

/// Trailing space-separated tags. 2-letter language codes only when UPPERCASE
/// (a title may legitimately end with "it" or "de").
fn tail_tag(tok: &str) -> Option<&'static str> {
    let upper_only = tok.chars().all(|c| !c.is_lowercase());
    Some(match tok.to_ascii_lowercase().as_str() {
        "vostfr" | "vost" => "fr",
        "multi" => "multi",
        "4k" | "4kl" | "uhd" | "fhd" | "sub" | "+18" => "",
        "vf" | "fr" if upper_only => "fr",
        "es" | "esp" | "mx" if upper_only => "es",
        "en" | "eng" if upper_only => "en",
        "de" if upper_only => "de",
        "it" if upper_only => "it",
        "tr" if upper_only => "tr",
        "hi" if upper_only => "hi",
        "hd" | "tb" | "vp" | "nf" | "amz" | "atsp" | "cam" | "hbo" | "dub" if upper_only => "",
        _ => return None,
    })
}

/// Trailing country codes that are also words (`NO`, `SE`): only after `(2023)`.
fn tail_tag_after_year(tok: &str) -> Option<&'static str> {
    if tok.chars().any(|c| c.is_lowercase()) {
        return None;
    }
    Some(match tok {
        "NO" => "no",
        "SE" => "sv",
        "DK" => "da",
        "FI" => "fi",
        "NL" => "nl",
        "PL" => "pl",
        "PT" => "pt",
        "RU" => "ru",
        _ => return None,
    })
}

fn apply_tag(lang: &mut LangInfo, tag: &str) {
    match tag {
        "" => {}
        "multi" => lang.multi = true,
        code => lang.langs.add(Langs::of(&[code])),
    }
}

fn is_scene_noise(tok: &str) -> bool {
    matches!(
        tok.to_ascii_lowercase().as_str(),
        "1080p" | "720p" | "480p" | "2160p" | "web-dl" | "webrip" | "web" | "bluray" | "bdrip"
            | "hdtv" | "dvdrip" | "x264" | "x265" | "h" | "hevc" | "aac2" | "ddp2" | "dual"
    )
}

/// Dotted release names: `Bookworm.2024.1080p.WEB-DL` → `Bookworm` (+ year),
/// `Kadının.Senfonisi` → `Kadının Senfonisi`. Acronyms (`S.W.A.T.`) are left alone.
fn undot_release_name(t: &str) -> Option<(String, Option<String>)> {
    if t.contains(' ') || !t.contains('.') {
        return None;
    }
    let mut kept = Vec::new();
    let mut year = None;
    let mut stopped = false;
    for seg in t.split('.').filter(|p| !p.is_empty()) {
        if let Some(y) = year_of(seg) {
            if !kept.is_empty() {
                year = Some(y);
                stopped = true;
                break;
            }
        }
        if is_scene_noise(seg) {
            stopped = true;
            break;
        }
        kept.push(seg);
    }
    let single_letters = kept
        .iter()
        .filter(|p| p.chars().count() == 1 && p.chars().all(char::is_alphabetic))
        .count();
    let words_ok = single_letters * 2 < kept.len();
    (words_ok && !kept.is_empty() && (stopped || kept.len() >= 2))
        .then(|| (kept.join(" "), year))
}

/// Peel locale tags, quality tags and the year from the end until stable.
fn peel_tail(t: &mut String, lang: &mut LangInfo, year: &mut Option<String>) {
    for _ in 0..10 {
        let trimmed_len = t.trim_end_matches([' ', '_']).len();
        t.truncate(trimmed_len);
        let lower = t.to_ascii_lowercase();
        if let Some((suf, tag)) = GLUED_SUFFIXES.iter().find(|(suf, _)| lower.ends_with(suf)) {
            if t.len() > suf.len() {
                t.truncate(t.len() - suf.len());
                apply_tag(lang, tag);
                continue;
            }
        }
        if let Some((suf, tag)) = GLUED_UPPER_SUFFIXES.iter().find(|(suf, _)| t.ends_with(suf)) {
            if t.len() > suf.len() {
                t.truncate(t.len() - suf.len());
                apply_tag(lang, tag);
                continue;
            }
        }
        if let Some((head, tok)) = t.rsplit_once(' ') {
            let after_year = head.trim_end().ends_with(')');
            // `(UHD)`: quality only — `(TR)` / `(US)` name the origin, not the audio.
            let bare = tok.trim_matches(|c| matches!(c, '(' | ')' | '[' | ']'));
            let tag = if bare.len() + 2 == tok.len() {
                tail_tag(bare).filter(|t| t.is_empty())
            } else {
                tail_tag(tok).or_else(|| after_year.then(|| tail_tag_after_year(tok)).flatten())
            };
            if let Some(tag) = tag {
                if !head.trim().is_empty() {
                    apply_tag(lang, tag);
                    let keep = head.len();
                    t.truncate(keep);
                    continue;
                }
            }
        }
        // `(2024)` / `[2024]`
        if t.ends_with(')') || t.ends_with(']') {
            let open = if t.ends_with(')') { '(' } else { '[' };
            if let Some(i) = t.rfind(open) {
                if let Some(y) = year_of(&t[i + 1..t.len() - 1]) {
                    if !t[..i].trim().is_empty() {
                        year.get_or_insert(y);
                        t.truncate(i);
                        continue;
                    }
                }
            }
        }
        // ` - 2024` / ` | 2024`
        if let Some(i) = t.rfind([' ']) {
            let head = t[..i].trim_end();
            if let Some(y) = year_of(&t[i + 1..]) {
                if let Some(h) = head.strip_suffix(['-', '|']) {
                    if !h.trim().is_empty() {
                        year.get_or_insert(y);
                        let keep = h.trim_end().len();
                        t.truncate(keep);
                        continue;
                    }
                }
            }
        }
        break;
    }
}

/// Dashes unified, superscript badges spelled out, `Hannah*s` → `Hannah's`.
fn normalize_item_chars(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '\u{2013}' | '\u{2014}' => out.push('-'),
            '*' if i > 0
                && chars[i - 1].is_alphabetic()
                && chars.get(i + 1).is_some_and(|n| n.is_alphabetic()) =>
            {
                out.push('\'')
            }
            c => out.push(desuperscript(c).unwrap_or(c)),
        }
    }
    out
}

/// Quality glued after the prefix: `4KL American Sniper`, `uhd Hala`.
fn strip_leading_quality(mut t: &str) -> &str {
    loop {
        let first = t.split_whitespace().next().unwrap_or("");
        if !t.trim().contains(' ')
            || !matches!(first.to_ascii_lowercase().as_str(), "4k" | "4kl" | "uhd" | "hd" | "fhd")
        {
            return t;
        }
        t = t.trim_start()[first.len()..].trim_start();
    }
}

pub fn parse_item_title(raw: &str) -> TitleInfo {
    let mut lang = LangInfo::default();
    let mut year = None;
    let normalized = normalize_item_chars(raw.trim());
    let mut s: &str = &normalized;

    // `AR - AR - Title` / `EN -4k EN - Title` carry the prefix twice.
    let mut last_head: Option<&str> = None;
    for _ in 0..3 {
        if let Some(head) = last_head {
            s = strip_repeated_prefix(s, head);
        }
        let Some((head, rest)) = split_item_prefix(s) else {
            break;
        };
        for p in head.split(':') {
            let p = p.split_once('-').map_or(p, |(a, _)| a);
            if let Some(i) = code_info(p) {
                lang.merge(i);
            }
        }
        last_head = Some(head);
        s = strip_leading_quality(rest);
    }

    let mut t = strip_leading_quality(s).to_string();
    peel_tail(&mut t, &mut lang, &mut year);
    // `She_Gives_Me_What_You_Can_t`
    if !t.contains(' ') && t.contains('_') {
        t = t.replace('_', " ");
        peel_tail(&mut t, &mut lang, &mut year);
    }
    if let Some((joined, y)) = undot_release_name(&t) {
        t = joined;
        if year.is_none() {
            year = y;
        }
        peel_tail(&mut t, &mut lang, &mut year);
    }

    let title = collapse_ws(t.trim_matches(|c: char| c == '-' || c == ' ' || c == '_'));
    TitleInfo {
        title: if title.is_empty() {
            collapse_ws(raw)
        } else {
            title
        },
        year,
        lang,
    }
}

/// Title for tiles / detail pages: portal prefix, locale tags and year removed.
pub fn display_title(raw: &str) -> String {
    parse_item_title(raw).title
}

fn is_episode_token(tok: &str) -> bool {
    let t = tok.to_ascii_lowercase();
    let t = t.trim_matches(|c: char| matches!(c, '-' | ':' | '|' | '.' | '[' | ']' | '(' | ')'));
    let digits_after = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if let Some(rest) = t.strip_prefix('s') {
        return match rest.split_once('e') {
            Some((s, e)) => digits_after(s) && digits_after(e),
            None => digits_after(rest),
        };
    }
    if let Some(rest) = t.strip_prefix("ep") {
        return digits_after(rest.trim_start_matches('.'));
    }
    if let Some(rest) = t.strip_prefix('e') {
        return digits_after(rest);
    }
    matches!(t.split_once('x'), Some((a, b)) if digits_after(a) && digits_after(b))
}

/// Episode row title: series name and `S01E01` markers removed
/// (`Stranger Things - S01E01 - Chapter One` → `Chapter One`).
pub fn episode_display(series: &str, raw: &str, episode_num: u32) -> String {
    let fallback = || format!("Épisode {episode_num}");
    let mut t = raw.trim();
    let clean_series = display_title(series);
    for prefix in [series.trim(), clean_series.as_str()] {
        if prefix.is_empty() || t.len() <= prefix.len() {
            continue;
        }
        if t.get(..prefix.len()).map(|h| h.eq_ignore_ascii_case(prefix)).unwrap_or(false) {
            t = &t[prefix.len()..];
            break;
        }
    }
    let seps = |c: char| c.is_whitespace() || matches!(c, '-' | ':' | '|' | '–' | '.');
    let mut t = t.trim_start_matches(seps);
    loop {
        let tok = t.split_whitespace().next().unwrap_or("");
        if tok.is_empty() || !is_episode_token(tok) {
            break;
        }
        t = t[tok.len()..].trim_start_matches(seps);
    }
    let t = collapse_ws(t.trim_end_matches(seps));
    let lower = t.to_lowercase();
    if t.is_empty()
        || lower == format!("episode {episode_num}")
        || lower == format!("épisode {episode_num}")
        || lower == "episode"
    {
        return fallback();
    }
    t
}

// ---------------------------------------------------------------------------
// Live channels
// ---------------------------------------------------------------------------

/// Channel code prefix: `|FR| X`, `FR|| X`, `NOW| X`, `ES: X`, `DE ▎ X`, `PRIME-DE | X`.
fn split_channel_code(s: &str) -> Option<(&str, &str)> {
    if let Some((code, rest)) = split_pipe_code(s) {
        return Some((code, rest.trim_start()));
    }
    for sep in [':', '▎'] {
        if let Some(j) = s.find(sep) {
            let code = s[..j].trim();
            if (2..=3).contains(&code.len()) && is_code_token(code) && code_info(code).is_some() {
                return Some((code, s[j + sep.len_utf8()..].trim_start()));
            }
        }
    }
    None
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelInfo {
    pub name: String,
    pub lang: LangInfo,
}

pub fn parse_channel(raw: &str) -> ChannelInfo {
    let (lang, rest) = match split_channel_code(raw) {
        Some((code, rest)) if !rest.trim().is_empty() => {
            (code_info(code).unwrap_or_default(), rest)
        }
        _ => (LangInfo::default(), raw),
    };
    let name = clean_decorations(rest.trim_start().trim_start_matches([':', '|']));
    ChannelInfo {
        name: if name.is_empty() {
            collapse_ws(raw)
        } else {
            name
        },
        lang,
    }
}

pub fn display_channel(raw: &str) -> String {
    parse_channel(raw).name
}

/// Empty PPV / event slots (`No Event`, `:AHL  15`, `MonoMax | Event 4`) — listed last.
pub fn is_placeholder_channel(raw: &str) -> bool {
    let l = raw.trim().to_lowercase();
    if l.starts_with(':') || l.contains("no event") || l.contains("no streaming") {
        return true;
    }
    let mut toks = l.split_whitespace().rev();
    let last = toks.next().unwrap_or("");
    let prev = toks.next().unwrap_or("");
    last.chars().all(|c| c.is_ascii_digit())
        && !last.is_empty()
        && matches!(prev, "event" | "stream" | "evento" | "événement")
}

/// Effective language of an item: its own tags win, else its category.
pub fn item_matches(item: LangInfo, category: LangInfo, pref: &str) -> bool {
    if item.is_known() {
        item.matches(pref)
    } else {
        category.matches(pref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_labels() {
        assert_eq!(category_label("|FR| ✪ SPORT"), "FR · Sport");
        assert_eq!(
            category_label("|FR| ✪ CHAINES FRANCAISES ᵁᴴᴰ"),
            "FR · Chaines Francaises UHD"
        );
        assert_eq!(category_label("|AR| ✪ BEIN SPORT HD"), "AR · beIN Sport HD");
        assert_eq!(category_label("XXX | ✪ FOR ADULTS"), "XXX · For Adults");
        assert_eq!(category_label("|+18| ✪ ADULTS"), "18+ · Adults");
        assert_eq!(
            category_label("✪ ORIGINAL NETFLIX  MULTI"),
            "Original Netflix Multi"
        );
        assert_eq!(category_label("|IT| ✪ Anno 2010 2019"), "IT · Anno 2010 2019");
        assert_eq!(
            category_label("|EN| ✪ PREMIER LEAGUE ᵁᴴᴰ/ᴴᴰ"),
            "EN · Premier League UHD/HD"
        );
    }

    #[test]
    fn category_languages() {
        let fr = parse_category("|FR| ✪ SPORT").lang;
        assert!(fr.matches("fr") && !fr.matches("en"));
        let vlaams = parse_category("|BE| ✪ VLAAMS").lang;
        assert!(vlaams.matches("nl") && !vlaams.matches("fr"));
        let be = parse_category("|BE| ✪ SPORT").lang;
        assert!(be.matches("fr") && be.matches("nl"));
        let qc = parse_category("|CA| ✪ QUEBEC CINEMA").lang;
        assert!(qc.matches("fr") && !qc.matches("en"));
        let multi = parse_category("✪ ORIGINAL DISNEY+  MULTI").lang;
        assert!(multi.matches("fr") && multi.matches("de"));
        let dub = parse_category("|TR| ✪ DUBLAJ MULTI").lang;
        assert!(dub.matches("tr") && !dub.matches("fr"));
        let sub = parse_category("|EN| ✪ MULTI SUB").lang;
        assert!(sub.matches("en") && !sub.matches("fr"));
        let adult = parse_category("XXX | ✪ FOR ADULTS").lang;
        assert!(adult.adult && !adult.matches("fr"));
        let nordic = parse_category("✪ NORDIC MULTI").lang;
        assert!(nordic.matches("sv") && !nordic.matches("fr"));
        assert!(parse_category("|AF| ✪ C+  AF CINEMA").lang.matches("fr"));
        assert!(parse_category("|AR| ✪ BEIN SPORT HD").lang.matches("ar"));
    }

    #[test]
    fn film_titles() {
        let t = parse_item_title("FR - Olga (2021)");
        assert_eq!((t.title.as_str(), t.year.as_deref()), ("Olga", Some("2021")));
        assert!(t.lang.matches("fr"));

        let t = parse_item_title("AR - AR - Sweet November  (2001)");
        assert_eq!(t.title, "Sweet November");
        assert!(t.lang.matches("ar"));

        let t = parse_item_title("ES - He Even Has Your Eyes - 2017_esp");
        assert_eq!((t.title.as_str(), t.year.as_deref()), ("He Even Has Your Eyes", Some("2017")));

        assert_eq!(display_title("FR - The Grill (2024) VOST"), "The Grill");
        assert_eq!(display_title("FR - Love Me (2025)  MULTI"), "Love Me");
        assert_eq!(display_title("DE - Rules Don't Apply (2016)_de"), "Rules Don't Apply");
        assert_eq!(
            display_title("NO - Beck 45 - 58 Minutes - 2022"),
            "Beck 45 - 58 Minutes"
        );
        assert_eq!(display_title("FR -4KL American Sniper"), "American Sniper");
        assert_eq!(display_title("FR -uhd Matrix"), "Matrix");
        assert_eq!(display_title("Beasts of No Nation - 2015"), "Beasts of No Nation");
        assert_eq!(
            display_title("EN - Pokémon: Pikachu's Winter Vacation (1998)"),
            "Pokémon: Pikachu's Winter Vacation"
        );
        assert_eq!(display_title("مستر كاراتيه"), "مستر كاراتيه");
        assert_eq!(display_title("Spider-Man (2002)"), "Spider-Man");
        assert_eq!(display_title("WALL-E"), "WALL-E");
        let t = parse_item_title("XXX - The Erotic Dreams of Cleopatra - 1985");
        assert!(t.lang.adult);
    }

    #[test]
    fn series_titles() {
        let t = parse_item_title("Stranger Things_fr");
        assert_eq!(t.title, "Stranger Things");
        assert!(t.lang.matches("fr"));
        assert_eq!(display_title("Suburra - La Serie-it"), "Suburra - La Serie");
        assert_eq!(display_title("Anne Boleyn (2021) VP"), "Anne Boleyn");
        let t = parse_item_title("The Legend of Vox Machina (MULTI)-fr");
        assert_eq!(t.title, "The Legend of Vox Machina");
        assert!(t.lang.multi && t.lang.langs.has("fr"));
        assert_eq!(display_title("The English  4k (2022) FR"), "The English");
        assert_eq!(display_title("Kadının.Senfonisi TB"), "Kadının Senfonisi");
        assert_eq!(display_title("Hello Tomorrow! (US)_sub"), "Hello Tomorrow! (US)");
        assert_eq!(display_title("S.W.A.T."), "S.W.A.T.");
        assert!(!parse_item_title("Chosen").lang.is_known());
    }

    #[test]
    fn episodes() {
        assert_eq!(
            episode_display("Stranger Things_fr", "Stranger Things - S01E01 - Chapter One", 1),
            "Chapter One"
        );
        assert_eq!(episode_display("Dark", "S02E03", 3), "Épisode 3");
        assert_eq!(episode_display("Dark", "Episode 4", 4), "Épisode 4");
        assert_eq!(episode_display("Dark", "Dark S01 E05 Vérités", 5), "Vérités");
        assert_eq!(episode_display("Lupin", "Chapitre 1", 1), "Chapitre 1");
        assert_eq!(episode_display("Dark", "Episode", 2), "Épisode 2");
    }

    #[test]
    fn channels() {
        let c = parse_channel("|FR| RMC Story ᵁᴴᴰ");
        assert_eq!(c.name, "RMC Story UHD");
        assert!(c.lang.matches("fr"));
        assert_eq!(display_channel("ES: CANAL EXTREMADURA"), "CANAL EXTREMADURA");
        assert_eq!(display_channel("FR|| L'ÉQUIPE LIVE 5 FHD"), "L'ÉQUIPE LIVE 5 FHD");
        assert_eq!(display_channel("NOW| SKY CINEMA SUSPENSE ᵁᴴᴰ"), "SKY CINEMA SUSPENSE UHD");
        assert_eq!(display_channel("DE ▎ PROTIME MOVIE"), "PROTIME MOVIE");
        assert!(parse_channel("PRIME-DE | No Event").lang.matches("de"));
        assert_eq!(display_channel("Bein Sports 5 |-12H|"), "Bein Sports 5 |-12H|");
        assert_eq!(
            display_channel("NASCAR: No 19 Chase Briscoe"),
            "NASCAR: No 19 Chase Briscoe"
        );
    }

    #[test]
    fn placeholders() {
        assert!(is_placeholder_channel("PRIME-DE | No Event"));
        assert!(is_placeholder_channel(":AHL  15"));
        assert!(is_placeholder_channel("MonoMax | Event 4"));
        assert!(is_placeholder_channel("DE: SportDeutschland Event 14"));
        assert!(!is_placeholder_channel("|FR| MTV ᴴᴰ"));
        assert!(!is_placeholder_channel("Sports Replay 3"));
        assert_eq!(display_channel(":Flo College  52"), "Flo College 52");
        assert_eq!(display_channel("||CN| Channel 2 FHD"), "Channel 2 FHD");
        assert_eq!(display_channel("|RU| | TV 1000 Ruskino"), "TV 1000 Ruskino");
        assert_eq!(display_channel("|MYᴴᴰ| MBC Drama ᴴᴰ"), "MBC Drama HD");
        assert_eq!(display_channel("|Venezuela| TVES"), "TVES");
        assert!(parse_channel("|UK.VIP|SKY MOVIES PREMIERE ᵁᴴᴰ").lang.matches("en"));
        assert_eq!(display_channel("|قصص النساء في القرآن |قصص"), "قصص النساء في القرآن |قصص");
        assert_eq!(display_channel("|EN KIDZ 2"), "EN KIDZ 2");
    }

    #[test]
    fn corpus_regressions() {
        assert_eq!(display_title("IN-TE - Oka Chinna Prema Katha  (2020)"), "Oka Chinna Prema Katha");
        assert!(parse_item_title("IN-TA - Sabdham - 2025").lang.matches("hi"));
        assert_eq!(display_title("DST - Spud - 2010"), "Spud");
        assert_eq!(
            display_title("EN -4k EN - Bad Boys 3 Bad Boys For Life 4K (2020)"),
            "Bad Boys 3 Bad Boys For Life"
        );
        assert_eq!(display_title("RU - Ru - Only Me is Normal (2021)"), "Only Me is Normal");
        assert_eq!(display_title("IT -uhd Hala (ᵁᴴᴰ)"), "Hala");
        assert_eq!(display_title("IT - I tre moschettieri | 1948"), "I tre moschettieri");
        assert_eq!(display_title("BG - Hannah*s Law (2012)"), "Hannah's Law");
        assert_eq!(display_title("ES - Agente Z - Misterio En El Zoo (2020) (LA)"), "Agente Z - Misterio En El Zoo");
        assert_eq!(
            display_title("NO - Hur.många.lingon.finns.det.i.världen.2011.1080p.TV2.WEB-DL"),
            "Hur många lingon finns det i världen"
        );
        assert_eq!(display_title("She_Gives_Me_What_You_Can_t"), "She Gives Me What You Can t");
        let t = parse_item_title("MOCRO MAFFIA: KOMT GOED (2021)_NL");
        assert_eq!((t.title.as_str(), t.year.as_deref()), ("MOCRO MAFFIA: KOMT GOED", Some("2021")));
        assert!(t.lang.matches("nl"));
        assert!(parse_item_title("Être famille d'accueil_qb").lang.matches("fr"));
        assert_eq!(display_title("Doom Patrol-SE"), "Doom Patrol");
        assert_eq!(display_title("Dancing Queens (2023) NO"), "Dancing Queens");
        assert_eq!(display_title("JUST SAY NO"), "JUST SAY NO");
        assert_eq!(display_title("Sol Yanım (SUB) -eng"), "Sol Yanım");
        // Origin tags stay: they tell versions apart and are not the audio language.
        assert_eq!(display_title("هل ستكون منا؟ (TR)"), "هل ستكون منا؟ (TR)");
        assert_eq!(display_title("House of Cards (US)_msub"), "House of Cards (US)");
        assert_eq!(category_label("|TR| ✪ ROMANTİK"), "TR · Romantik");
        assert_eq!(category_label("|EN| ✪ GANGSTER|MAFIA"), "EN · Gangster|Mafia");
        assert!(parse_category("|AS| ✪ KANNADA").lang.matches("hi"));
    }

    #[test]
    fn item_vs_category() {
        let cat = parse_category("|AR| ✪ مسلسلات مترجمه").lang;
        let item = parse_item_title("Hello Tomorrow! (US)_sub").lang;
        assert!(!item_matches(item, cat, "fr"));
        let netflix = parse_category("✪ ORIGINAL NETFLIX  MULTI").lang;
        assert!(item_matches(parse_item_title("Your Life is a Joke (2021) NF").lang, netflix, "fr"));
        let fr_in_multi = parse_item_title("DE - Chosen").lang;
        assert!(!item_matches(fr_in_multi, netflix, "fr"));
    }
}
