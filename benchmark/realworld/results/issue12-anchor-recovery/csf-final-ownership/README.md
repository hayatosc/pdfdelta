# Final source ownership of the remaining CSF expectations

Both reviewed replacement expectations are still unmatched as replacements. Their four quoted sides are completely covered by final deletion or insertion events, and none overlaps a final unresolved region. All four source ranges belong to initial alignment span 0 with `ReadingOrderUnknown`. The initial alignment evidence therefore does not describe the final ownership of these ranges.

The old Core sentence is deletion 81 at block 166, canonical range `314..430`. Its proposed new counterpart is block 98, `0..138`, inside insertion 610 spanning blocks 97–98, group range `8..165`. The extra insertion prefix contains caption text. Adding a second replacement without splitting that existing insertion would duplicate ownership. `ownership-summary.md` and `ownership.json` retain all four source ranges and input/source hashes.

The diagnostic repair uses final ownership to distinguish an unmatched replacement relation from a surviving unresolved source region. It leaves both expected changes in the failure list and does not increase reviewed recall. A final unresolved reading-order region still takes precedence.

The probe initially selected the wrong side when projecting initial alignment spans, incorrectly reporting no new-side overlap. The retained version fixes that bug, records membership by block ID first, and explicitly retains projection failures. Its final result contains complete, side-correct alignment overlap for all four quotes.

`section-context.json` finds no matching enclosing heading title for either the intended Core pair or the competing profile/glossary pair. `head-prototype/` retains a separate, unadopted textual experiment: reciprocal list/head scoring selects the intended Core pair among six captured candidates. It has no production integration, ownership transfer, or corpus-wide validation and is not counted as a recovered change.

To reproduce the ownership probe, copy `Cargo.toml`, `Cargo.lock`, and `probe.rs.txt` to `target/issue12-csf-final-ownership-probe/`, with the source named `src/main.rs`, then run its standalone crate from the repository root. The relative dependency points to `../../crates/pdfdelta-core`. The program reads the cached PDFs and the unchanged expected manifest; it does not mutate either.
