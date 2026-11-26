//! Display names as people see them (review item S3).
//!
//! A display name is chosen by its owner (or written by an Admin) and synced
//! as is; it is never an identifier. Before one is shown anywhere (desktop,
//! the `serve` API, notifications) it goes through [`clean_display_name`],
//! which removes the characters that let a name look like something else:
//! bidirectional overrides and isolates (a name that renders "Ada" while
//! reading "adA" or that reorders the text after it), zero-width and other
//! invisible format characters, and control characters. Signed events keep
//! the bytes their author signed; only what is rendered is cleaned.
//!
//! [`name_skeleton`] folds a cleaned name to a comparison form (case and
//! common cross-script look-alikes), so a list of people can flag two
//! different accounts whose names look the same.

/// Whether `c` is removed from a displayed name: bidi embeddings, overrides,
/// isolates and marks, zero-width and other invisible format characters,
/// fillers that render as blank, and control characters.
pub fn is_hidden_name_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'                  // soft hyphen
        | '\u{034F}'                // combining grapheme joiner
        | '\u{061C}'                // Arabic letter mark
        | '\u{115F}' | '\u{1160}'   // Hangul fillers
        | '\u{17B4}' | '\u{17B5}'   // Khmer inherent vowels
        | '\u{180B}'..='\u{180F}'   // Mongolian selectors and vowel separator
        | '\u{200B}'..='\u{200F}'   // zero-width space, (non-)joiner, LRM, RLM
        | '\u{202A}'..='\u{202E}'   // LRE, RLE, PDF, LRO, RLO
        | '\u{2060}'..='\u{206F}'   // word joiner, invisible operators, LRI..PDI, deprecated format
        | '\u{3164}'                // Hangul filler
        | '\u{FE00}'..='\u{FE0F}'   // variation selectors
        | '\u{FEFF}'                // zero-width no-break space
        | '\u{FFA0}'                // halfwidth Hangul filler
        | '\u{FFF9}'..='\u{FFFB}'   // interlinear annotations
        | '\u{1D173}'..='\u{1D17A}' // musical format characters
        | '\u{E0000}'..='\u{E0FFF}' // tags, variation selectors supplement, unassigned format block
    ) || c.is_control()
}

/// The name as it is shown: hidden and control characters removed, runs of
/// whitespace collapsed to one space, trimmed.
pub fn clean_display_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut space = false;
    for c in name.chars().filter(|c| !is_hidden_name_char(*c)) {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

/// A comparison form of a name: cleaned, lowercased, spaces removed, and
/// common look-alike letters (Cyrillic, Greek, digits, `rn`) folded to their
/// Latin twin. Two different people whose skeletons are equal have names
/// that look alike.
pub fn name_skeleton(name: &str) -> String {
    // A capital I looks like a small l in most UI fonts: fold it first.
    let lower: String = clean_display_name(name)
        .chars()
        .map(|c| if c == 'I' { 'l' } else { c })
        .flat_map(char::to_lowercase)
        .collect();
    let folded: String = lower
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '.' | '_' | '-' | '\u{00B7}'))
        .map(|c| match c {
            'а' | 'α' | 'ɑ' => 'a',
            'в' | 'β' => 'b',
            'с' | 'ϲ' => 'c',
            'ԁ' => 'd',
            'е' | 'ε' | 'ё' => 'e',
            'ɡ' => 'g',
            'һ' => 'h',
            'і' | 'ι' | 'ı' | '1' | '|' | 'ӏ' | 'l' => 'l',
            'ј' => 'j',
            'κ' | 'к' => 'k',
            'м' => 'm',
            'η' | 'п' => 'n',
            'о' | 'ο' | '0' | 'σ' => 'o',
            'р' | 'ρ' => 'p',
            'ԛ' => 'q',
            'ѕ' | '5' => 's',
            'т' | 'τ' => 't',
            'υ' | 'ս' => 'u',
            'ν' | 'ѵ' => 'v',
            'ԝ' | 'ω' => 'w',
            'х' | 'χ' => 'x',
            'у' | 'γ' => 'y',
            'z' => 'z',
            other => other,
        })
        .collect();
    folded.replace("rn", "m").replace("vv", "w")
}

/// `serialize_with` for a display-name field of a DTO.
pub fn ser_clean<S: serde::Serializer>(name: &str, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&clean_display_name(name))
}

/// [`ser_clean`] for an optional name.
pub fn ser_clean_opt<S: serde::Serializer>(name: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    match name {
        Some(n) => s.serialize_some(&clean_display_name(n)),
        None => s.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// S3: names lose the characters that reorder or hide text, and nothing
    /// else.
    #[test]
    fn display_name_bidi_controls_stripped() {
        // "Ada" with a right-to-left override and a pop: renders reversed.
        assert_eq!(clean_display_name("\u{202E}adA\u{202C}"), "adA");
        assert_eq!(
            clean_display_name("A\u{200B}d\u{200D}a\u{2066}\u{2069}"),
            "Ada"
        );
        assert_eq!(
            clean_display_name("\u{FEFF}Ada\u{2028}Lovelace\u{0007}"),
            "Ada Lovelace"
        );
        assert_eq!(clean_display_name("  Ada \t\n Lovelace  "), "Ada Lovelace");
        assert_eq!(clean_display_name("\u{3164}"), "");
        assert_eq!(clean_display_name("Zoë Ångström 李"), "Zoë Ångström 李");
        for c in [
            '\u{202A}', '\u{202B}', '\u{202D}', '\u{2067}', '\u{2068}', '\u{200E}', '\u{200F}',
            '\u{061C}',
        ] {
            assert_eq!(clean_display_name(&format!("x{c}y")), "xy", "{:?}", c);
        }
    }

    #[test]
    fn look_alike_names_share_a_skeleton() {
        assert_eq!(name_skeleton("Ada"), name_skeleton("аdа")); // Cyrillic а
        assert_eq!(name_skeleton("Ada Lovelace"), name_skeleton("ada lovelace"));
        assert_eq!(name_skeleton("bob"), name_skeleton("b\u{200B}ob"));
        assert_eq!(name_skeleton("modern"), name_skeleton("rnodern"));
        assert_eq!(name_skeleton("Bill"), name_skeleton("BiII"));
        assert_ne!(name_skeleton("Ada"), name_skeleton("Bob"));
    }

    #[test]
    fn serializer_cleans() {
        #[derive(serde::Serialize)]
        struct D {
            #[serde(serialize_with = "ser_clean")]
            n: String,
        }
        let j = serde_json::to_string(&D {
            n: "\u{202E}x".into(),
        })
        .unwrap();
        assert_eq!(j, r#"{"n":"x"}"#);
    }
}
