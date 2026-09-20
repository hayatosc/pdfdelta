//! Static, non-owning review bundles. The final HTML is the completion marker;
//! failed writes leave an explicitly incomplete directory and never overwrite it.

use std::{io, path::Path};

use pdfdelta_core::document::{DocumentView, DocumentViewComparison};
use serde::{Serialize, Serializer, ser::SerializeMap};

pub(crate) mod bundle;
mod html;

use bundle::{MAX_BYTES, file};

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

pub(super) fn write(
    directory: &Path,
    report: &impl Serialize,
    comparison: &DocumentViewComparison,
    images: Option<&crate::evidence_compare::ImageReport>,
    complete: bool,
    old: Input<'_>,
    new: Input<'_>,
) -> Result<(), String> {
    bundle::create_directory(directory)?;
    let result = (|| {
        let mut remaining = MAX_BYTES;
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
            file(directory, "sources.json", &mut remaining, |out| {
                serde_json::to_writer(
                    out,
                    &SourceIndex {
                        version: 1,
                        old: Sources(old.view),
                        new: Sources(new.view),
                    },
                )
                .map_err(io::Error::other)
            })?,
        ];
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
                if previews > 1024 {
                    return Err("static review exceeds its 1024 page-preview limit".into());
                }
                artifacts.push(file(
                    directory,
                    &format!("{side}-region-{}.png", region.id),
                    &mut remaining,
                    |out| {
                        let mut encoder =
                            png::Encoder::new(out, region.raster.width, region.raster.height);
                        encoder.set_color(png::ColorType::Rgb);
                        encoder.set_depth(png::BitDepth::Eight);
                        let mut writer = encoder.write_header().map_err(io::Error::other)?;
                        writer
                            .write_image_data(&region.raster.rgb)
                            .map_err(io::Error::other)?;
                        writer.finish().map_err(io::Error::other)
                    },
                )?);
            }
        }
        file(directory, "manifest.json", &mut remaining, |out| {
            serde_json::to_writer_pretty(out, &serde_json::json!({
                "version": 1,
                "completion_marker": "index.html",
                "max_bundle_bytes": MAX_BYTES,
                "max_page_previews": 1024,
                "source_export": "Retained glyphs, vectors, marked content, graph and source metadata. Composited page samples are losslessly exported as PNG; exported normalization certificates are not reusable source proofs.",
                "files": artifacts,
            })).map_err(io::Error::other)
        })?;
        file(directory, "index.html", &mut remaining, |out| {
            html::write(out, comparison, images, complete, &old, &new)
        })?;
        Ok(())
    })();
    result.map_err(|error: String| {
        format!(
            "{error}; review directory {} is incomplete without index.html",
            directory.display()
        )
    })
}
