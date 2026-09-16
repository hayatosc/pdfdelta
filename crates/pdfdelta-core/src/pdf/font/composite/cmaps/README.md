# Bundled Adobe character maps

These files are unmodified upstream resources. Their headers retain the complete
Adobe BSD-3-Clause copyright notices and redistribution terms. Include these
notices with binary distributions that contain the resources.

| File | Version | Upstream revision | SHA-256 |
| --- | --- | --- | --- |
| `UniJIS-UTF16-H` | 1.027 | `f5cf3bca7fdfeaceb77aa82847e974f2306c20b4` | `5cf77ed38c25262dba845676738a887afe45e44425c3dd2489334c4d8c510fcb` |
| `Adobe-Japan1-UCS2` | 10.002 | `2dd5e53fb74a01718b9dfd448a0d1cce6fff2aa5` | `6a9693361647a37996312cc57071bb79f8c06411207be7c730a83fda1254cd82` |

Sources:

- [Character-code to CID resource](https://github.com/adobe-type-tools/cmap-resources/blob/f5cf3bca7fdfeaceb77aa82847e974f2306c20b4/Adobe-Japan1-7/CMap/UniJIS-UTF16-H)
- [CID to Unicode resource](https://github.com/adobe-type-tools/mapping-resources-pdf/blob/2dd5e53fb74a01718b9dfd448a0d1cce6fff2aa5/pdf2unicode/Adobe-Japan1-UCS2)
- [Adobe-Japan1 collection and supplement CID bounds](https://github.com/adobe-type-tools/Adobe-Japan1)

The first resource has 15892 ordinary CID mappings, 32 notdef codes and three
codespaces. Its 15927 expanded entries and 188225 bytes count against the font's
existing limits before parsing. The second resource has 294111 bytes; when used,
its entries and Unicode scalars are charged by the existing bounded ToUnicode
parser. Neither resource is downloaded at runtime.

Only this fixed encoding resource is passed to `hayro-cmap`; PDF-provided CMaps
continue through the bounded PDF font parser. Source codes are segmented as
UTF-16BE, mapped to CIDs for widths, then mapped through the second resource for
text when no explicit ToUnicode exists. Canonical Unicode sequences, including
variation selectors, remain intact. Notdef codes do not become textual spaces.
Registry and ordering must match Adobe-Japan1, and each decoded CID must lie
within the font's declared supported supplement.
