#[cfg(test)]
mod h28_projection_probe {
    use super::*;
    use crate::alignment::BlockSeparator;
    use crate::diff::TextSpan;
    use crate::diff::assessment::{SourceInterval, project};
    use crate::pdf::{LopdfParser, ParseLimits, PdfParser};
    use crate::source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor};
    use std::collections::HashMap;

    fn extract(path: &str) -> (Box<dyn crate::pdf::ParsedPdf>, ExtractionOutcome) {
        let bytes = std::fs::read(path).expect("h28 fixture");
        let pdf = LopdfParser
            .parse(bytes.into(), ParseLimits::default())
            .expect("h28 fixture");
        let outcome = ContentStreamGlyphExtractor
            .extract_outcome(&*pdf, ExtractionLimits::default())
            .expect("h28 fixture");
        (pdf, outcome)
    }

    fn prepare_side(path: &str, side: DocumentSide) -> PreparedDocument {
        let (pdf, outcome) = extract(path);
        let mut budget = PipelineOptions::default().diff.max_assessment_work;
        crate::document::with_native_order_proof(
            Some(pdf),
            outcome.document().clone(),
            &mut budget,
            |document, proof, _budget| {
                prepare(
                    document,
                    PipelineOptions::default(),
                    side,
                    &mut PipelineDiagnostics::new(),
                    proof,
                )
                .expect("prepare")
            },
        )
    }

    fn side_of(prepared: &PreparedDocument) -> crate::diff::Side<'_> {
        let canonical = prepared
            .blocks
            .iter()
            .map(|block| block.canonical.comparable_tokens().expect("tokens"))
            .collect::<Vec<_>>();
        let index = prepared
            .blocks
            .iter()
            .enumerate()
            .map(|(position, block)| (block.block, position))
            .collect::<HashMap<_, _>>();
        let total_tokens = canonical.iter().map(Vec::len).sum();
        crate::diff::Side {
            blocks: &prepared.blocks,
            index,
            canonical,
            total_tokens,
        }
    }

    fn span_of(relation: &serde_json::Value, side: &str) -> TextSpan {
        let blocks = relation[format!("{side}b")]
            .as_array()
            .expect("blocks")
            .iter()
            .map(|value| crate::layout::BlockId(value.as_u64().expect("block")))
            .collect::<Vec<_>>();
        let (s, e, cs, ce) = match side {
            "o" => (
                relation["os"].as_u64().expect("os") as usize,
                relation["oe"].as_u64().expect("oe") as usize,
                relation["oc"].as_u64().expect("oc") as usize,
                relation["oc_end"].as_u64().expect("oc_end") as usize,
            ),
            _ => (
                relation["ns"].as_u64().expect("ns") as usize,
                relation["ne"].as_u64().expect("ne") as usize,
                relation["nc"].as_u64().expect("nc") as usize,
                relation["nc_end"].as_u64().expect("nc_end") as usize,
            ),
        };
        TextSpan {
            blocks,
            separator: match relation["sep"].as_str() {
                Some("space") => Some(BlockSeparator::Space),
                _ => None,
            },
            canonical_range: crate::normalize::ScalarRange { start: cs, end: ce },
            comparable_range: crate::diff::TokenRange { start: s, end: e },
        }
    }

    fn projected(
        side: &crate::diff::Side<'_>,
        relation: &serde_json::Value,
        which: &str,
    ) -> Vec<(usize, usize, usize)> {
        project(side, &span_of(relation, which))
            .expect("projection")
            .into_iter()
            .map(|interval: SourceInterval| (interval.block_index, interval.start, interval.end))
            .collect()
    }

    #[test]
    fn w2_projection_probe() {
        let Ok(census_path) = std::env::var("H28_CENSUS") else {
            return;
        };
        let Ok(relations_path) = std::env::var("H28_RELATIONS") else {
            return;
        };
        let census: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(census_path).expect("census"))
                .expect("census json");
        let relations: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(relations_path).expect("relations"))
                .expect("relations json");
        let relations = relations["relations"]
            .as_array()
            .expect("relations")
            .clone();
        let old_prepared = prepare_side(
            &std::env::var("H28_W2_OLD").expect("old path"),
            DocumentSide::Old,
        );
        let new_prepared = prepare_side(
            &std::env::var("H28_W2_NEW").expect("new path"),
            DocumentSide::New,
        );
        let old_side = side_of(&old_prepared);
        let new_side = side_of(&new_prepared);

        // Representative assertion: [blocks 0,1] group 11..391 projects to
        // block1 local 0..380 on the old side.
        let known = relations
            .iter()
            .find(|relation| {
                relation["ob"] == serde_json::json!([0, 1])
                    && relation["os"] == serde_json::json!(11)
                    && relation["oe"] == serde_json::json!(391)
            })
            .expect("known relation");
        let known_projection = projected(&old_side, known, "o");
        println!("H28 known projection {known_projection:?}");
        assert!(
            known_projection.contains(&(1, 0, 380)),
            "known relation must project to block1 local 0..380"
        );

        let mut proven = 0usize;
        let mut conflict = 0usize;
        let mut missing = 0usize;
        let mut unowned = 0usize;
        for entry in census["census"].as_array().expect("entries") {
            let side_name = entry["side"].as_str().expect("side");
            let block = entry["block"].as_u64().expect("block");
            let start = entry["start"].as_u64().expect("start") as usize;
            let end = entry["end"].as_u64().expect("end") as usize;
            let (own_side, other_side) = if side_name == "old" {
                (&old_side, &new_side)
            } else {
                (&new_side, &old_side)
            };
            let which = if side_name == "old" { "o" } else { "n" };
            let block_id = crate::layout::BlockId(block);
            let mut left = Vec::new();
            let mut right = Vec::new();
            let mut covering = 0usize;
            for relation in &relations {
                if relation["outcome"] != serde_json::json!("established")
                    || !relation["reasons"].as_array().is_some_and(Vec::is_empty)
                {
                    continue;
                }
                let projection = projected(own_side, relation, which);
                for (block_index, s, e) in &projection {
                    if *block_index != own_side.index[&block_id] {
                        continue;
                    }
                    if *e == start {
                        left.push((relation.clone(), projection.clone()));
                    }
                    if *s == end {
                        right.push((relation.clone(), projection.clone()));
                    }
                    if *s <= start && end <= *e {
                        covering += 1;
                    }
                }
            }
            if covering > 0 {
                unowned += 1;
                continue;
            }
            if left.len() != 1 || right.len() != 1 {
                conflict += 1;
                continue;
            }
            let (left_rel, left_projection) = &left[0];
            let (right_rel, right_projection) = &right[0];
            let left_other = projected(
                other_side,
                left_rel,
                if side_name == "old" { "n" } else { "o" },
            );
            let right_other = projected(
                other_side,
                right_rel,
                if side_name == "old" { "n" } else { "o" },
            );
            let left_ok = left_other.iter().any(|(_, s, e)| *e == start);
            let right_ok = right_other.iter().any(|(_, s, _)| *s == end);
            let pair_ok =
                left_projection.len() == 1 && right_projection.len() == 1 && left_ok && right_ok;
            if pair_ok {
                proven += 1;
                println!(
                    "H28 proven {side_name} block={block} gap={start}..{end} left={:?} right={:?}",
                    left_projection, right_projection
                );
            } else {
                missing += 1;
            }
        }
        println!(
            "H28 result proven={proven} conflict={conflict} missing_counterpart={missing} covered={unowned}"
        );
    }
}
