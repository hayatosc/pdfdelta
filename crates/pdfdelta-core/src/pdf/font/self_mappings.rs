// Self-authored glyph-name mappings.
//
// This table was written by the pdfdelta project from the glyph names and
// standard TeX font documentation (the Computer Modern math symbol fonts).
// It is released under the same MIT license as the rest of the project; no
// third-party data is copied here.
//
// A name is included only when its meaning is one Unicode character that does
// not depend on an adjacent glyph. Comments give the Unicode character name.
//
// Deliberately excluded:
// - `negationslash`: U+0338 COMBINING LONG SOLIDUS OVERLAY is a combining
//   overlay whose attachment to the following/preceding relation is not
//   encoded by the glyph sequence; mapping it would invent text.
// - `arrowrightabove` (U+20D7 COMBINING RIGHT ARROW ABOVE), `circleencl`
//   (U+20DD COMBINING ENCLOSING CIRCLE), `circleslashencl` (U+20E0 COMBINING
//   ENCLOSING CIRCLE BACKSLASH), and `diamondencl` (U+20DF COMBINING
//   ENCLOSING DIAMOND): the same combining/enclosing ambiguity.
// - `U0C95_U0CCB.blwf`: a script-specific base+combining sequence with a
//   below-base-form suffix, not one fixed character.
// - `g109`, `g1093`, `g111`, `g131`, `g132`, `g133`, `g1397`, `g14`, `g210`,
//   `g218`, `g3`, `glyph296`: generic or glyph-index names with no meaning.
// - `modi_letter_ha`, `modi_letter_ka`, `modi_letter_ra`, `modi_letter_sa`,
//   `modi_vowel_sign_aa`, `modi_vowel_sign_e`, `tibetangha`,
//   `tibetangnyisshad`, `tibetanintertsheg`, `tibetanka`, `tibetankha`,
//   `tibetanshad`: script-specific letter/mark names. They stay placeholders
//   because no authoritative public mapping exists for these font encodings,
//   so script-specific names remain unresolved placeholders.

/// Looks up a self-authored unambiguous glyph name.
pub(super) fn lookup(name: &[u8]) -> Option<&'static str> {
    let index = MAPPINGS
        .binary_search_by(|(candidate, _)| candidate.as_bytes().cmp(name))
        .ok()?;
    Some(MAPPINGS[index].1)
}

static MAPPINGS: &[(&str, &str)] = &[
    ("asciigrave", "\u{0060}"),   // GRAVE ACCENT
    ("ceilingleft", "\u{2308}"),  // LEFT CEILING
    ("ceilingright", "\u{2309}"), // RIGHT CEILING
    ("floorleft", "\u{230A}"),    // LEFT FLOOR
    ("floorright", "\u{230B}"),   // RIGHT FLOOR
    ("greatermuch", "\u{226B}"),  // MUCH GREATER-THAN
    ("lessmuch", "\u{226A}"),     // MUCH LESS-THAN
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_each_value_is_one_scalar() {
        assert!(MAPPINGS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for (name, value) in MAPPINGS {
            assert_eq!(value.chars().count(), 1, "{name}");
        }
    }

    #[test]
    fn known_tex_names_map() {
        assert_eq!(lookup(b"lessmuch"), Some("\u{226A}"));
        assert_eq!(lookup(b"greatermuch"), Some("\u{226B}"));
        assert_eq!(lookup(b"floorleft"), Some("\u{230A}"));
        assert_eq!(lookup(b"floorright"), Some("\u{230B}"));
        assert_eq!(lookup(b"ceilingleft"), Some("\u{2308}"));
        assert_eq!(lookup(b"ceilingright"), Some("\u{2309}"));
        assert_eq!(lookup(b"asciigrave"), Some("\u{0060}"));
    }

    #[test]
    fn ambiguous_and_generic_names_are_not_mapped() {
        for name in [
            b"negationslash".as_slice(),
            b"arrowrightabove",
            b"circleencl",
            b"circleslashencl",
            b"diamondencl",
            b"U0C95_U0CCB.blwf",
            b"g111",
            b"glyph296",
            b"modi_letter_ka",
            b"tibetanintertsheg",
            b"pdfjsname",
        ] {
            assert_eq!(lookup(name), None, "{name:?}");
        }
    }

    #[test]
    fn does_not_duplicate_agl_or_the_pdfjs_supplement() {
        for name in [b"parenleftbig".as_slice(), b"Euro", b"parenleftBig"] {
            assert_eq!(lookup(name), None, "{name:?}");
        }
    }
}
