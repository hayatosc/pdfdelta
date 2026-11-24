//! Static, non-owning review bundles. The final HTML is the completion marker;
//! failed writes leave an explicitly incomplete directory and never overwrite it.

use std::{
    io::{self, Read},
    path::Path,
};

use pdfdelta_core::document::{DocumentView, DocumentViewComparison};
use pdfdelta_flow::FlowError;
use serde::{Serialize, Serializer, ser::SerializeMap};

pub(crate) mod bundle;
mod html;

use bundle::{MAX_BYTES, file};

/// Flow evidence a review bundle presents beside the shared comparison.
pub(super) struct FlowReview<'a> {
    pub flow: Option<&'a crate::flow_cmd::FlowComparison>,
    pub verdicts: Option<&'a [crate::flow_exit::ChannelVerdict]>,
}

pub(super) struct Input<'a> {
    pub view: DocumentView<'a>,
    pub name: &'a Path,
    /// The exact bounded bytes used for acquisition, including standard input.
    pub bytes: &'a [u8],
}

struct Sources<'a>(DocumentView<'a>);

impl Serialize for Sources<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let store = self.0.evidence;
        let mut map = serializer.serialize_map(Some(8))?;
        map.serialize_entry(
            "summary",
            &crate::evidence_compare::EvidenceSummary::new(store),
        )?;
        map.serialize_entry("pages", &store.pages)?;
        map.serialize_entry("native", &store.native)?;
        map.serialize_entry("structured", &store.structured)?;
        map.serialize_entry("inventories", &store.inventories)?;
        map.serialize_entry("key_inventories", &store.key_inventories)?;
        map.serialize_entry("native_structures", &store.native_structures)?;
        map.serialize_entry("graph", self.0.graph)?;
        map.end()
    }
}

#[derive(Serialize)]
struct SourceIndex<'a> {
    version: u32,
    old: Sources<'a>,
    new: Sources<'a>,
}

/// Page previews retained by one static review.
///
/// The release bound is 1,024. Integration tests run the debug binary and can
/// lower the bound so the CLI trace path can be exercised without building a
/// 1,025-page document; release builds never read the environment.
fn page_preview_limit() -> usize {
    #[cfg(debug_assertions)]
    if let Some(limit) = std::env::var("PDFDELTA_REVIEW_PREVIEW_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|limit| *limit > 0)
    {
        return limit;
    }
    1024
}

/// Reads the source index from a review bundle's `sources.json.gz`.
///
/// The current writer publishes one gzip stream. Bundles written before the
/// manifest version 3 bump are a gzip member inside another gzip member, and
/// the oldest bundles used an uncompressed `sources.json`. This reader accepts
/// all three shapes and returns the JSON bytes unchanged.
///
/// The CLI's review query commands read the separate `--agent-review` bundle
/// layout; this is the compatibility path for consumers that read the static
/// HTML bundle's source artifact. It is currently exercised by tests that pin
/// both historical shapes.
#[allow(dead_code)]
pub(crate) fn read_sources(bytes: &[u8]) -> io::Result<Vec<u8>> {
    fn gunzip(bytes: &[u8]) -> io::Result<Vec<u8>> {
        let mut decoder = flate2::read::GzDecoder::new(bytes);
        let mut output = Vec::new();
        decoder.read_to_end(&mut output)?;
        Ok(output)
    }

    if bytes.starts_with(&[0x1f, 0x8b]) {
        let first = gunzip(bytes)?;
        if first.starts_with(&[0x1f, 0x8b]) {
            return gunzip(&first);
        }
        return Ok(first);
    }
    Ok(bytes.to_vec())
}

pub(super) fn write(
    directory: &Path,
    report: &impl Serialize,
    comparison: &DocumentViewComparison,
    images: Option<&crate::evidence_compare::ImageReport>,
    complete: bool,
    flow: FlowReview<'_>,
    (old, new): (Input<'_>, Input<'_>),
) -> Result<(), FlowError> {
    bundle::create_directory(directory)?;
    let result = (|| -> Result<(), FlowError> {
        let mut remaining = MAX_BYTES;
        let sources = SourceIndex {
            version: 1,
            old: Sources(old.view),
            new: Sources(new.view),
        };
        // Flow reviews serialize a much larger source index than a native-text
        // bundle on the same pair. Store it losslessly compressed so the 512
        // MiB bundle budget bounds output size, not the evidence itself.
        let mut artifacts = vec![
            file(directory, "old.pdf", &mut remaining, |out| {
                out.write_all(old.bytes)
            })?,
            file(directory, "new.pdf", &mut remaining, |out| {
                out.write_all(new.bytes)
            })?,
            file(directory, "comparison.json", &mut remaining, |out| {
                serde_json::to_writer_pretty(out, report).map_err(io::Error::other)
            })?,
            file(directory, "sources.json.gz", &mut remaining, |out| {
                // The shared output publisher compresses a `.gz` destination
                // exactly once. Writing a gzip member here would be compressed
                // again by the publisher; older bundles used that nested
                // double-gzip shape, which the reader below still accepts.
                serde_json::to_writer(out, &sources).map_err(io::Error::other)
            })?,
        ];
        let preview_limit = page_preview_limit();
        let mut previews = 0usize;
        for (side, input) in [("old", &old), ("new", &new)] {
            for region in input
                .view
                .evidence
                .rendered
                .iter()
                .filter(|region| region.composited_page)
            {
                previews += 1;
                if previews > preview_limit {
                    return Err(FlowError::limit_exceeded(
                        "static review page previews",
                        preview_limit,
                        format!("static review exceeds its {preview_limit} page-preview limit"),
                    ));
                }
                artifacts.push(file(
                    directory,
                    &format!("{side}-region-{}.png", region.id),
                    &mut remaining,
                    |out| {
                        bundle::write_png(
                            out,
                            region.raster.width,
                            region.raster.height,
                            &region.raster.rgb,
                        )
                    },
                )?);
            }
        }
        file(directory, "manifest.json", &mut remaining, |out| {
            serde_json::to_writer_pretty(out, &serde_json::json!({
                "version": 3,
                "completion_marker": "index.html",
                "max_bundle_bytes": MAX_BYTES,
                "max_page_previews": preview_limit,
                "source_export": "Retained glyphs, vectors, marked content, graph and source metadata. Composited page samples are losslessly exported as PNG; exported normalization certificates are not reusable comparison evidence.",
                "files": artifacts,
            })).map_err(io::Error::other)
        })?;
        file(directory, "index.html", &mut remaining, |out| {
            html::write(out, comparison, images, complete, &flow, &old, &new)
        })?;
        Ok(())
    })();
    result.map_err(|error| {
        error.with_suffix(format!(
            "review directory {} is incomplete without index.html",
            directory.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use flate2::{Compression, write::GzEncoder};

    use super::read_sources;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).expect("gzip payload");
        encoder.finish().expect("gzip trailer")
    }

    #[test]
    fn source_reader_accepts_plain_single_and_double_gzip() {
        let json = br#"{"version":1,"old":{},"new":{}}"#;
        let single = gzip(json);
        let double = gzip(&single);
        assert_eq!(read_sources(json).expect("plain source index"), json);
        assert_eq!(read_sources(&single).expect("single gzip"), json);
        assert_eq!(read_sources(&double).expect("double gzip"), json);
    }
}
