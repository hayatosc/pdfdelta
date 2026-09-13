//! Static, non-owning review bundles. The final HTML is the completion marker;
//! failed writes leave an explicitly incomplete directory and never overwrite it.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use pdfdelta_core::document::{DocumentView, DocumentViewComparison};
use serde::{Serialize, Serializer, ser::SerializeMap};
use sha2::{Digest, Sha256};

mod html;

const MAX_BYTES: usize = 512 * 1024 * 1024;

pub(super) struct Input<'a> {
    pub view: DocumentView<'a>,
    pub name: &'a Path,
    /// The exact bounded bytes used for acquisition, including standard input.
    pub bytes: &'a [u8],
}

pub(super) fn validate_destination(
    directory: &Path,
    outputs: &[Option<&Path>],
) -> Result<(), String> {
    match fs::symlink_metadata(directory) {
        Ok(_) => {
            return Err(format!(
                "refusing existing review directory {}",
                directory.display()
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot inspect review directory {}: {error}",
                directory.display()
            ));
        }
    }
    let destination = crate::fs::normalized_destination(directory, "review directory")?;
    for output in outputs.iter().flatten() {
        if crate::fs::normalized_destination(output, "review output")?.starts_with(&destination) {
            return Err(format!(
                "output {} collides with the review directory",
                output.display()
            ));
        }
    }
    Ok(())
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

#[derive(Serialize)]
struct Artifact {
    name: String,
    bytes: usize,
    sha256: String,
}

struct BoundedWriter<'a> {
    inner: &'a mut dyn Write,
    remaining: &'a mut usize,
    hash: &'a mut Sha256,
}

impl Write for BoundedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > *self.remaining {
            return Err(io::Error::other(
                "static review exceeds its 512 MiB output budget",
            ));
        }
        self.inner.write_all(bytes)?;
        self.hash.update(bytes);
        *self.remaining -= bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn file(
    directory: &Path,
    name: &str,
    remaining: &mut usize,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<Artifact, String> {
    let before = *remaining;
    let mut hash = Sha256::new();
    crate::fs::write_output_atomically(&directory.join(name), "review artifact", |writer| {
        write(&mut BoundedWriter {
            inner: writer,
            remaining,
            hash: &mut hash,
        })
        .map_err(|error| format!("cannot write review artifact {name}: {error}"))
    })?;
    Ok(Artifact {
        name: name.into(),
        bytes: before - *remaining,
        sha256: hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    })
}

pub(super) fn write(
    directory: &Path,
    report: &impl Serialize,
    comparison: &DocumentViewComparison,
    complete: bool,
    old: Input<'_>,
    new: Input<'_>,
) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory).map_err(|error| {
        format!(
            "cannot create new review directory {}: {error}",
            directory.display()
        )
    })?;
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
            html::write(out, comparison, complete, &old, &new)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_budget_rejects_excess_without_partial_chunk_or_charge() {
        let mut output = Vec::new();
        let mut remaining = 3;
        let mut hash = Sha256::new();
        let mut writer = BoundedWriter {
            inner: &mut output,
            remaining: &mut remaining,
            hash: &mut hash,
        };
        writer.write_all(b"ab").expect("bounded prefix");
        assert!(writer.write_all(b"cd").is_err());
        assert_eq!(remaining, 1);
        assert_eq!(output, b"ab");
        assert_eq!(hash.finalize(), Sha256::digest(b"ab"));
    }
}
