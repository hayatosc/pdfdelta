# Legibility of the retained 72 dpi page rasters

Measured while implementing the visual fallback, to decide whether a
higher-resolution rendering profile is needed and what it would cost. This is a
measurement of the current profile, not a proposal that has been implemented.

## What was measured

A two-revision pair whose only difference is `10 days` → `20 days`, set in
4 pt Helvetica on a US Letter page. Both sides were compared with the default
channels, exported with `--agent-review`, and the affected case was rendered
with `review render`.

- Page raster: 612 × 792 samples, profile
  `hayro/0.7.1/page-rgb-white-72dpi-annotations-v1`
- Crop produced for the changed line: **122 × 12 pixels**
- Stroke height available for a 4 pt glyph at 72 dpi: about **3 pixels**

At that size the line's shape is visible but individual digits are not
separable, which is exactly the distinction the case asks about. The crop is a
faithful cut of the samples the comparison examined; it is the sampling density,
not the cropping, that is insufficient.

Enlarging this crop would not add evidence. Resampling invents no detail, and
presenting a magnified 3-pixel glyph as a clearer reading would misrepresent
what was observed.

## What a higher-resolution profile would cost

US Letter, RGB, one page, against the current per-page limits
(`MAX_PIXELS = 8,000,000` in the render worker and
`EvidenceLimits::max_raster_bytes = 256 MiB` retained per input):

| Profile | Samples | Pixels | RGB bytes | Within the current limits |
| --- | --- | --- | --- | --- |
| 72 dpi (current) | 612 × 792 | 484,704 | 1.4 MiB | yes |
| 150 dpi | 1275 × 1650 | 2,103,750 | 6.0 MiB | yes |
| 288 dpi | 2448 × 3168 | 7,755,264 | 22.2 MiB | at the pixel limit |
| 300 dpi | 2550 × 3300 | 8,415,000 | 24.1 MiB | **over** the pixel limit |
| 600 dpi | 5100 × 6600 | 33,660,000 | 96.3 MiB | over |

The worker's width and height arguments are `u16`, so they do not bind until
about 65,000 samples per side; the pixel limit binds first. The retained raster
budget also binds sooner in page count than in resolution: at 150 dpi, 256 MiB
holds roughly 42 pages of this size, against roughly 180 at 72 dpi.

## Consequence for the current build

- 150 dpi is the largest step that stays inside every existing per-page limit
  for this page size, and it quadruples the samples a 4 pt glyph receives.
- Anything at or above 288 dpi requires raising the render worker's pixel limit,
  which is a separate decision about worker resource bounds.
- A higher-resolution rendering must be declared as its own profile and its own
  backend identity. It is a different observation of the document, not a better
  view of the existing raster, and a packet must not present the two as
  interchangeable.
- Until such a profile exists, a case whose text cannot be read at 72 dpi is
  answerable only as undetermined. That is the correct outcome: the evidence to
  settle it was not acquired.
