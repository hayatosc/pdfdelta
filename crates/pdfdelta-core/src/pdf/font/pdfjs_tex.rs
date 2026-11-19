// TeX/AMS glyph-name mappings from Mozilla pdf.js.
//
// Source: https://github.com/mozilla/pdf.js
// Commit: fc0822a07ea0b2fde125dec4754ce4da4c52e956
// Path: src/core/glyphlist.js
// SHA-256: 48636549cbad67889b0fd5f59bc3e0bb732c0af545868f5db004749dcdf3012c
// License: Apache License 2.0; see THIRD-PARTY-NOTICES.md.
//
// This table contains only entries whose names the existing Adobe Glyph
// List, `uni`/`u` hex rules, suffix stripping, and underscore composition
// do not already resolve. Entries whose value is a private-use code point
// (U+E000..=U+F8FF) or a control character are deliberately omitted; those
// names keep the placeholder identity from the font evidence.
//
// Generated from the pinned file by a script kept outside the repository:
// it parses only the `getGlyphsUnicode` table (not the ZapfDingbats table),
// compares names against `agl.rs`, and enforces the filters above.
// Selection: 4,328 pdf.js entries; 4,200 already resolvable, 6 shape-skipped
// (`f_f`, `f_f_i`, `f_f_l`, `f_i`, `f_l`, `.notdef`), 1 control-value-skipped
// (`controlNULL` U+0000); 121 remain. All 192 PUA-valued entries overlap
// AGL names and were removed by the AGL filter before the value filter.

pub(super) fn lookup(name: &[u8]) -> Option<&'static str> {
    let index = TEX_GLYPHS
        .binary_search_by(|(candidate, _)| candidate.as_bytes().cmp(name))
        .ok()?;
    usable(TEX_GLYPHS[index].1)
}

/// Rejects a value that is not exactly one non-control, non-private-use
/// Unicode scalar. The generated table already applies this filter; the
/// runtime check keeps the contract if the table is ever regenerated.
fn usable(text: &'static str) -> Option<&'static str> {
    let mut scalars = text.chars();
    let scalar = scalars.next()?;
    if scalars.next().is_some() || scalar.is_control() {
        return None;
    }
    if (0xE000..=0xF8FF).contains(&u32::from(scalar)) {
        return None;
    }
    Some(text)
}

static TEX_GLYPHS: &[(&str, &str)] = &[
    ("angbracketleft", "\u{3008}"),
    ("angbracketleftBig", "\u{2329}"),
    ("angbracketleftBigg", "\u{2329}"),
    ("angbracketleftbig", "\u{2329}"),
    ("angbracketleftbigg", "\u{2329}"),
    ("angbracketright", "\u{3009}"),
    ("angbracketrightBig", "\u{232A}"),
    ("angbracketrightBigg", "\u{232A}"),
    ("angbracketrightbig", "\u{232A}"),
    ("angbracketrightbigg", "\u{232A}"),
    ("arrowhookleft", "\u{21AA}"),
    ("arrowhookright", "\u{21A9}"),
    ("arrowleftbothalf", "\u{21BD}"),
    ("arrowlefttophalf", "\u{21BC}"),
    ("arrownortheast", "\u{2197}"),
    ("arrownorthwest", "\u{2196}"),
    ("arrowrightbothalf", "\u{21C1}"),
    ("arrowrighttophalf", "\u{21C0}"),
    ("arrowsoutheast", "\u{2198}"),
    ("arrowsouthwest", "\u{2199}"),
    ("backslashBig", "\u{2216}"),
    ("backslashBigg", "\u{2216}"),
    ("backslashbig", "\u{2216}"),
    ("backslashbigg", "\u{2216}"),
    ("bardbl", "\u{2016}"),
    ("bracehtipdownleft", "\u{FE37}"),
    ("bracehtipdownright", "\u{FE37}"),
    ("bracehtipupleft", "\u{FE38}"),
    ("bracehtipupright", "\u{FE38}"),
    ("braceleftBig", "\u{7B}"),
    ("braceleftBigg", "\u{7B}"),
    ("braceleftbig", "\u{7B}"),
    ("braceleftbigg", "\u{7B}"),
    ("bracerightBig", "\u{7D}"),
    ("bracerightBigg", "\u{7D}"),
    ("bracerightbig", "\u{7D}"),
    ("bracerightbigg", "\u{7D}"),
    ("bracketleftBig", "\u{5B}"),
    ("bracketleftBigg", "\u{5B}"),
    ("bracketleftbig", "\u{5B}"),
    ("bracketleftbigg", "\u{5B}"),
    ("bracketrightBig", "\u{5D}"),
    ("bracketrightBigg", "\u{5D}"),
    ("bracketrightbig", "\u{5D}"),
    ("bracketrightbigg", "\u{5D}"),
    ("ceilingleftBig", "\u{2308}"),
    ("ceilingleftBigg", "\u{2308}"),
    ("ceilingleftbig", "\u{2308}"),
    ("ceilingleftbigg", "\u{2308}"),
    ("ceilingrightBig", "\u{2309}"),
    ("ceilingrightBigg", "\u{2309}"),
    ("ceilingrightbig", "\u{2309}"),
    ("ceilingrightbigg", "\u{2309}"),
    ("circlecopyrt", "\u{A9}"),
    ("circledotdisplay", "\u{2299}"),
    ("circledottext", "\u{2299}"),
    ("circlemultiplydisplay", "\u{2297}"),
    ("circlemultiplytext", "\u{2297}"),
    ("circleplusdisplay", "\u{2295}"),
    ("circleplustext", "\u{2295}"),
    ("contintegraldisplay", "\u{222E}"),
    ("contintegraltext", "\u{222E}"),
    ("coproductdisplay", "\u{2210}"),
    ("coproducttext", "\u{2210}"),
    ("floorleftBig", "\u{230A}"),
    ("floorleftBigg", "\u{230A}"),
    ("floorleftbig", "\u{230A}"),
    ("floorleftbigg", "\u{230A}"),
    ("floorrightBig", "\u{230B}"),
    ("floorrightBigg", "\u{230B}"),
    ("floorrightbig", "\u{230B}"),
    ("floorrightbigg", "\u{230B}"),
    ("hatwide", "\u{302}"),
    ("hatwider", "\u{302}"),
    ("hatwidest", "\u{302}"),
    ("integraldisplay", "\u{222B}"),
    ("integraltext", "\u{222B}"),
    ("intercal", "\u{1D40}"),
    ("intersectiondisplay", "\u{22C2}"),
    ("intersectiontext", "\u{22C2}"),
    ("logicalanddisplay", "\u{2227}"),
    ("logicalandtext", "\u{2227}"),
    ("logicalordisplay", "\u{2228}"),
    ("logicalortext", "\u{2228}"),
    ("parenleftBig", "\u{28}"),
    ("parenleftBigg", "\u{28}"),
    ("parenleftbig", "\u{28}"),
    ("parenleftbigg", "\u{28}"),
    ("parenrightBig", "\u{29}"),
    ("parenrightBigg", "\u{29}"),
    ("parenrightbig", "\u{29}"),
    ("parenrightbigg", "\u{29}"),
    ("planckover2pi", "\u{210F}"),
    ("planckover2pi1", "\u{210F}"),
    ("prime", "\u{2032}"),
    ("productdisplay", "\u{220F}"),
    ("producttext", "\u{220F}"),
    ("radicalBig", "\u{221A}"),
    ("radicalBigg", "\u{221A}"),
    ("radicalbig", "\u{221A}"),
    ("radicalbigg", "\u{221A}"),
    ("radicalbt", "\u{221A}"),
    ("radicaltp", "\u{221A}"),
    ("radicalvertex", "\u{221A}"),
    ("slashBig", "\u{2F}"),
    ("slashBigg", "\u{2F}"),
    ("slashbig", "\u{2F}"),
    ("slashbigg", "\u{2F}"),
    ("summationdisplay", "\u{2211}"),
    ("summationtext", "\u{2211}"),
    ("tildewide", "\u{2DC}"),
    ("tildewider", "\u{2DC}"),
    ("tildewidest", "\u{2DC}"),
    ("uniondisplay", "\u{22C3}"),
    ("unionmultidisplay", "\u{228E}"),
    ("unionmultitext", "\u{228E}"),
    ("unionsqdisplay", "\u{2294}"),
    ("unionsqtext", "\u{2294}"),
    ("uniontext", "\u{22C3}"),
    ("vextenddouble", "\u{2225}"),
    ("vextendsingle", "\u{2223}"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_unique() {
        assert!(TEX_GLYPHS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn every_value_is_one_non_control_non_private_use_scalar() {
        for (name, value) in TEX_GLYPHS {
            assert_eq!(usable(value), Some(*value), "{name}");
        }
    }

    #[test]
    fn private_use_and_control_values_are_rejected() {
        assert_eq!(usable("\u{E000}"), None);
        assert_eq!(usable("\u{0}"), None);
        assert_eq!(usable("ab"), None);
        assert_eq!(usable("("), Some("("));
    }

    #[test]
    fn known_tex_names_map() {
        assert_eq!(lookup(b"parenleftbig"), Some("("));
        assert_eq!(lookup(b"summationtext"), Some("\u{2211}"));
    }

    #[test]
    fn supplement_does_not_duplicate_agl_or_carry_private_use_names() {
        assert!(lookup(b"Asmall").is_none());
        assert!(lookup(b"controlNULL").is_none());
        for (name, _) in TEX_GLYPHS {
            assert!(
                super::super::agl::lookup(name.as_bytes()).is_none(),
                "{name}"
            );
        }
    }
}
