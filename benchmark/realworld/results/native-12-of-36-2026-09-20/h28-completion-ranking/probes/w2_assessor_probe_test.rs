#[cfg(test)]
mod h28_assessor_probe {
    use super::*;
    use crate::pdf::{LopdfParser, ParseLimits, PdfParser};
    use crate::source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor};

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

    #[test]
    fn w2_assessor_probe() {
        let Ok(paths_file) = std::env::var("H28_W2_PDFS") else {
            return;
        };
        let paths: Vec<String> = std::fs::read_to_string(paths_file)
            .expect("h28 fixture")
            .lines()
            .map(String::from)
            .collect();
        let (old_pdf, old_outcome) = extract(&paths[0]);
        let (new_pdf, new_outcome) = extract(&paths[1]);
        let (comparison, work) = compare_extraction_outcomes_with_native_order_proofs(
            Some(old_pdf),
            old_outcome,
            Some(new_pdf),
            new_outcome,
            PipelineOptions::default(),
            &mut PipelineDiagnostics::new(),
        )
        .expect("comparison");
        println!(
            "H28 comparison unresolved={} changes={} work={work:?}",
            comparison.comparison.unresolved_regions.len(),
            comparison.comparison.changes.len()
        );
    }
}
