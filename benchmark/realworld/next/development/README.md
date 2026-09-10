# Development input registration

`selection.json` records 24 initial publication-series slots and two replacement
attempts selected before comparison output. `inputs.json` freezes 24 active pairs
and the SHA-256 hashes of all 48 available PDFs. This freezes development inputs,
not implementation or blind evaluation. No comparison of this set has run at
registration time; source annotations and concrete presentation-trait inspection
remain pending. Family labels record intended coverage, not verified capabilities.

CLIP (2103.00020) and LLaMA (2302.13971) have only v1 in their arXiv submission
histories; requested v2 downloads returned HTTP 404. Their original records remain,
with DDPM (2006.11239 v1/v2) and Llama 2 (2307.09288 v1/v2) as replacements.
The corresponding arXiv abstract pages provide the revision histories.

`acquisition-attempts.json` retains failed URLs and successful retries, including
HTTP 429 and the obsolete NIST PDF location. Publisher-hosted bytes are cached at
`benchmark/realworld/cache/next-dev/<pair-id>-<old|new>.pdf` and are not committed.
Reproduction must verify the recorded hash, not assume a stable URL serves stable
bytes. The care-skills original-series PDF is the October 2020 update, while the
new file is the March 2025 revision. Curriculum years and NASA revision years name
the publication series; downloaded corrected editions must be distinguished during
source annotation rather than treated as the first bytes published that year.

Preserve unsupported extraction and unresolved annotations in later denominators.
These records are development data and must never be relabeled as blind.
