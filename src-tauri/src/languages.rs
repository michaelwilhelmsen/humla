// Backend mirror of src/lib/languages.ts.
//
// We keep two copies — the frontend one drives the picker UI (with native
// names for display), and this one drives prompt-time language directives
// for the summary path. Code-to-English-name lookup is all we need server
// side; the UI layer never sees these strings.
//
// Source: openai/whisper tokenizer.py LANGUAGES dict, plus Cantonese (yue)
// added in large-v3. 99 entries — no "auto", which is handled separately by
// callers as a "let the model decide" sentinel.

pub fn english_name(code: &str) -> &'static str {
    LANGUAGES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, name)| *name)
        .unwrap_or("English")
}

/// Reverse of `english_name`: OpenAI's `verbose_json` reports the detected
/// language as an English *name* (`"english"`, `"norwegian"`) rather than a
/// code. Case-insensitive.
///
/// Deliberately returns `None` for an unrecognised name instead of
/// borrowing `english_name`'s `unwrap_or("English")` fallback — a
/// detection we can't map is an absent detection, not an English one.
pub fn code_for_english_name(name: &str) -> Option<&'static str> {
    let needle = name.trim();
    if needle.is_empty() {
        return None;
    }
    LANGUAGES
        .iter()
        .find(|(_, n)| n.eq_ignore_ascii_case(needle))
        .map(|(code, _)| *code)
}

const LANGUAGES: &[(&str, &str)] = &[
    ("af", "Afrikaans"),
    ("sq", "Albanian"),
    ("am", "Amharic"),
    ("ar", "Arabic"),
    ("hy", "Armenian"),
    ("as", "Assamese"),
    ("az", "Azerbaijani"),
    ("ba", "Bashkir"),
    ("eu", "Basque"),
    ("be", "Belarusian"),
    ("bn", "Bengali"),
    ("bs", "Bosnian"),
    ("br", "Breton"),
    ("bg", "Bulgarian"),
    ("my", "Burmese"),
    ("yue", "Cantonese"),
    ("ca", "Catalan"),
    ("zh", "Chinese"),
    ("hr", "Croatian"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("nl", "Dutch"),
    ("en", "English"),
    ("et", "Estonian"),
    ("fo", "Faroese"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("gl", "Galician"),
    ("ka", "Georgian"),
    ("de", "German"),
    ("el", "Greek"),
    ("gu", "Gujarati"),
    ("ht", "Haitian Creole"),
    ("ha", "Hausa"),
    ("haw", "Hawaiian"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("hu", "Hungarian"),
    ("is", "Icelandic"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("jw", "Javanese"),
    ("kn", "Kannada"),
    ("kk", "Kazakh"),
    ("km", "Khmer"),
    ("ko", "Korean"),
    ("lo", "Lao"),
    ("la", "Latin"),
    ("lv", "Latvian"),
    ("ln", "Lingala"),
    ("lt", "Lithuanian"),
    ("lb", "Luxembourgish"),
    ("mk", "Macedonian"),
    ("mg", "Malagasy"),
    ("ms", "Malay"),
    ("ml", "Malayalam"),
    ("mt", "Maltese"),
    ("mi", "Maori"),
    ("mr", "Marathi"),
    ("mn", "Mongolian"),
    ("ne", "Nepali"),
    ("no", "Norwegian"),
    ("nn", "Nynorsk"),
    ("oc", "Occitan"),
    ("ps", "Pashto"),
    ("fa", "Persian"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("pa", "Punjabi"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("sa", "Sanskrit"),
    ("sr", "Serbian"),
    ("sn", "Shona"),
    ("sd", "Sindhi"),
    ("si", "Sinhala"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("so", "Somali"),
    ("es", "Spanish"),
    ("su", "Sundanese"),
    ("sw", "Swahili"),
    ("sv", "Swedish"),
    ("tl", "Tagalog"),
    ("tg", "Tajik"),
    ("ta", "Tamil"),
    ("tt", "Tatar"),
    ("te", "Telugu"),
    ("th", "Thai"),
    ("bo", "Tibetan"),
    ("tr", "Turkish"),
    ("tk", "Turkmen"),
    ("uk", "Ukrainian"),
    ("ur", "Urdu"),
    ("uz", "Uzbek"),
    ("vi", "Vietnamese"),
    ("cy", "Welsh"),
    ("yi", "Yiddish"),
    ("yo", "Yoruba"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_names_map_back_to_their_codes() {
        assert_eq!(code_for_english_name("english"), Some("en"));
        assert_eq!(code_for_english_name("Norwegian"), Some("no"));
        assert_eq!(code_for_english_name("  SWEDISH  "), Some("sv"));
    }

    // The whole point of a separate fn: `english_name` answers an unknown
    // code with "English", which is a sane display fallback and a terrible
    // detection result. Storing a silent "English" as if Whisper had
    // detected it would summarise a Klingon meeting in English on purpose.
    #[test]
    fn an_unknown_name_is_none_not_english() {
        assert_eq!(code_for_english_name("Klingon"), None);
        assert_eq!(code_for_english_name(""), None);
    }
}
