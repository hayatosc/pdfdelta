# CSF final ownership probe

The probe ran with `PipelineOptions::default` against the two cached CSF PDFs and compared the four independently located sides of the two reviewed replacement entries. The reviewed old/new pairing is identified by the expected change ID.

| Expected change | Side | Source block and canonical range | Existing final owner | Counterpart owner |
| --- | --- | --- | --- | --- |
| `all-sector-scope-emphasized` | old | block 92, `764..937` | deletion 32, block 92, `764..937` | none; this is a one-sided final deletion |
| `all-sector-scope-emphasized` | new | block 43, `0..204` | insertion 553, blocks 41–43, group `0..230`; quote overlap `26..230` | none; this is a one-sided final insertion |
| `core-expanded-from-five-to-six-functions` | old | block 166, `314..430` | deletion 81, block 166, `314..430` | none; this is a one-sided final deletion |
| `core-expanded-from-five-to-six-functions` | new | block 98, `0..138` | insertion 610, blocks 97–98, group `8..165`; quote overlap `27..165` | none; this is a one-sided final insertion |

Every quote is fully covered by its listed final change, and none overlaps a final unresolved region. All four quoted block IDs are members of original alignment span 0; side-correct projection succeeds and fully overlaps each quote. That span is `Unresolved` with `ReadingOrderUnknown` on both old and new sides. The result therefore indicates one-sided ownership splitting or reassignment, rather than a stable old/new owner pair that was merely hidden by an unresolved final region.

The run reported 790 changes and 1,320 unresolved regions. Old coverage was `84973/125351` (`0.6778805115`); new coverage was `49065/68333` (`0.7180278928`).

## Provenance

SHA-256 values pin the exact binary inputs, expected manifest, probe source, and generated record:

| Artifact | SHA-256 |
| --- | --- |
| `benchmark/realworld/cache/nist-csf-v1-1-to-v2-0-old.pdf` | `0f3ca796610ab024cbc3484cbd6799e19b4d9159d3634972a2a76af83f15fb92` |
| `benchmark/realworld/cache/nist-csf-v1-1-to-v2-0-new.pdf` | `3c31f46fee98cac0c4323453e5109291a213b4de7fef8c058af9bf67f717433c` |
| `benchmark/realworld/expected/nist-csf-v1-1-to-v2-0.json` | `1766100c1be51afe2e2bf62edf0e804d23a4946999ab74acdb9a2109d837a4ac` |
| `target/issue12-csf-final-ownership-probe/src/main.rs` | `71a3efcdb32da8abe995abb57b69237d80fe1b0b2b7b5e08c87206566e14bd04` |
| `target/issue12-csf-final-ownership-probe/ownership.json` | `3d0216a6a3b1def9c640307594995568629ed55a4d7ad221aef459ad2f5e9ed7` |
