# Preserve the page of a recoverable Form failure

The DDPM development pair exposed a loss of acquisition provenance: a failed
Form invocation before the first retained glyph on page 1 retained a glyph gap
between the last glyph on page 0 and the first glyph on page 1. Inferring the failure page from
these neighbors returned no page. The native evidence adapter consequently
marked every text inventory incomplete, including unrelated later pages.

The content interpreter knows which page invoked the Form. `PageGlyphGap`
preserves that page together with the retained-glyph boundary. The evidence
adapter preserves both instead of inferring the page from neighboring glyphs.
The page is an explicit provider assertion from extraction; it is not a
cross-revision identity or an estimate of missing geometry. Validation checks
the retained neighbors, known page membership and agreement between the issue
and boundary page. Programmatic providers must supply the actual failure page.

The failed page stays incomplete. Comparisons crossing its glyph gap retain
the same local extraction dependency. Unscoped `GlyphGap`, `PageGap` and
document failures keep their previous conservative behavior. This change
does not remove uncertain text, ignore non-text paint, change normalization,
promote inferred parents, or assign changed ownership to review context.

The native JSON report keeps the `glyph_gap` scope and retained count, adding
the already-defined page field when known. The shared evidence boundary uses
the distinct `page_glyph_gap` tag, so source-provided page scope cannot be
confused with page scope inferred from adjacent retained glyphs. Old serialized
unscoped gaps remain conservative.

Regression cases cover a failed Form before any retained glyph on its page
and after the last glyph on that page, followed by an unaffected page. They
check inventory isolation, unchanged boundary neighbors, serialized evidence
validation and rejection of a mismatched issue page. Existing unscoped
cross-page cases still leave both page inventories incomplete. Reporting
checks preserve both the page and glyph boundary rather than replacing one
with the other.
