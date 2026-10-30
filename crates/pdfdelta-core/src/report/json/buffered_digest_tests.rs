use super::*;

#[derive(Default)]
struct CountingWriter {
    bytes: Vec<u8>,
    calls: usize,
    max_write: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        let count = bytes.len().min(self.max_write);
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn buffered_source_array_preserves_partial_writes_and_tail() -> Result<()> {
    for length in [0, 1, 2, 256, 257, 4096] {
        let sources = vec![SpanSourceEvidence::BlockSeparatorSpace; length];
        let expected = serde_json::to_vec(&JsonSourceArray(&sources))
            .expect("fixture sources serialize into a byte vector");
        for max_write in [1, 17, 8192, usize::MAX] {
            let mut writer = CountingWriter {
                max_write,
                ..CountingWriter::default()
            };
            buffered_source_array(&mut writer, &sources)?;
            assert_eq!(writer.bytes, expected);
        }
    }
    Ok(())
}

#[test]
fn buffered_source_array_coalesces_small_writes() -> Result<()> {
    let sources = vec![SpanSourceEvidence::BlockSeparatorSpace; 4096];
    let mut direct = CountingWriter {
        max_write: usize::MAX,
        ..CountingWriter::default()
    };
    serde_json::to_writer(&mut direct, &JsonSourceArray(&sources))
        .expect("fixture sources serialize into the counting writer");
    let mut buffered = CountingWriter {
        max_write: usize::MAX,
        ..CountingWriter::default()
    };
    buffered_source_array(&mut buffered, &sources)?;
    assert_eq!(buffered.bytes, direct.bytes);
    assert!(buffered.calls * 100 < direct.calls);
    Ok(())
}

struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("fixture sink refused"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn buffered_source_array_reports_final_flush_failure() {
    let error = buffered_source_array(&mut FailingWriter, &[])
        .expect_err("final flush propagates the sink failure");
    assert!(error.to_string().contains("fixture sink refused"));
}

#[test]
fn buffered_source_array_reports_zero_write_failure() {
    let mut writer = CountingWriter::default();
    assert!(buffered_source_array(&mut writer, &[]).is_err());
}

#[test]
fn buffered_source_digest_reports_byte_overflow() {
    let mut digest = SourceDigest {
        hash: Sha256::new(),
        bytes: usize::MAX - 1,
    };
    let error = buffered_source_array(&mut digest, &[])
        .expect_err("the final bracket overflows the seeded byte counter");
    assert!(error.to_string().contains("byte count overflow"));
}

#[test]
fn buffered_digest_matches_oneshot_across_buffer_and_sha_boundaries() {
    for length in [0, 1, 63, 64, 65, 8191, 8192, 8193, 16383, 16384, 16385] {
        let bytes: Vec<u8> = (0..length).map(|index| (index * 37 + 11) as u8).collect();
        for fragment in [1, 7, 63, 64, 65, 8191, 8192, 8193] {
            let mut digest = SourceDigest {
                hash: Sha256::new(),
                bytes: 0,
            };
            {
                let mut writer = std::io::BufWriter::with_capacity(8192, &mut digest);
                for piece in bytes.chunks(fragment) {
                    writer
                        .write_all(piece)
                        .expect("bounded fixture digest write");
                }
                writer.flush().expect("bounded fixture digest flush");
            }
            assert_eq!(digest.bytes, bytes.len());
            assert_eq!(digest.hash.finalize(), Sha256::digest(&bytes));
        }
    }
}

/// Checks digests and all summary fields against an independent Python oracle.
#[test]
fn source_summary_matches_independent_hash_and_field_oracle() -> Result<()> {
    use crate::{
        model::{GlyphId, PageId, Rect, Vec2},
        pdf::ObjectRef,
    };
    let oracle: serde_json::Value = serde_json::from_str(include_str!("source_digest_oracle.json"))
        .expect("fixed source checksum oracle");
    let rows = oracle["cases"].as_array().expect("oracle case array");
    assert_eq!(rows.len(), 18);
    for row in rows {
        let length = row["source_items"].as_u64().expect("fixture length") as usize;
        let sources: Vec<_> = (0..length)
            .map(|index| {
                let id = u64::MAX - index as u64;
                match index % 4 {
                    0 => SpanSourceEvidence::Glyph {
                        glyph_id: GlyphId(id),
                        page: PageId(u32::MAX - index as u32),
                        bbox: Rect {
                            min: Vec2 {
                                x: [-0.0, f64::MIN_POSITIVE, -f64::MAX][index % 3],
                                y: -0.125,
                            },
                            max: Vec2 {
                                x: f64::MAX,
                                y: [1e20, 1e-20, f64::EPSILON][index % 3],
                            },
                        },
                        content_stream: ObjectRef {
                            object_number: u32::MAX,
                            generation: u16::MAX,
                        },
                        operator_index: index as u32,
                    },
                    1 => SpanSourceEvidence::SyntheticSpace {
                        preceding_glyph_id: GlyphId(id),
                        following_glyph_id: GlyphId(id - 1),
                    },
                    2 => SpanSourceEvidence::LineBreak {
                        preceding_glyph_id: GlyphId(id),
                        following_glyph_id: GlyphId(id - 1),
                    },
                    _ => SpanSourceEvidence::BlockSeparatorSpace,
                }
            })
            .collect();
        let actual = serde_json::to_value(JsonSourceSummary::new(&sources)?)
            .expect("source summary serialization");
        assert_eq!(actual, row["expected"], "source count {length}");
    }
    Ok(())
}
