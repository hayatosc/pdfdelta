//! Fuzzing-only entry point for the canonical YAML parser.

use crate::canonical::{CanonicalRenderDocument, MAX_CANONICAL_YAML_BYTES};

const MAX_INPUT_BYTES: usize = 64 * 1024;

/// Exercises the canonical YAML parser with tight resource budgets.
///
/// Inputs larger than 64 KiB are ignored. All parser errors are accepted
/// outcomes; the oracle asserts only sound success invariants that have been
/// confirmed against existing unit tests.
///
/// # Panics
///
/// Panics if a successfully parsed document violates its configured
/// section, paragraph, or render-line budgets, or if any text or identifier
/// violates the validated bounds.
#[doc(hidden)]
pub fn fuzz_canonical_yaml(input: &[u8]) {
    if input.len() > MAX_INPUT_BYTES {
        return;
    }
    let Ok(yaml) = std::str::from_utf8(input) else {
        return;
    };
    if yaml.len() > MAX_CANONICAL_YAML_BYTES {
        // The parser would reject this as InvalidInput; treat as acceptable.
        return;
    }
    let Ok(document) = CanonicalRenderDocument::from_yaml(yaml) else {
        return;
    };
    // Success invariants: these must hold for any Ok result and are
    // verified against the existing validation logic.
    assert!(!document.title().is_empty());
    assert!(document.sections().len() <= 8);
    let paragraph_count: usize = document
        .sections()
        .iter()
        .map(|section| section.paragraphs().len())
        .sum();
    assert!(paragraph_count <= 32);
    let render_lines = document.render_lines();
    assert!(render_lines.len() <= 32);
    assert_eq!(
        render_lines.len(),
        1 + document.sections().len() + paragraph_count
    );
    for line in render_lines {
        assert!(!line.is_empty());
        assert!(line.len() <= 512);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_YAML: &str = r"
document:
  title: Quarterly Service Report
  sections:
    - id: availability
      heading: Service availability
      paragraphs:
        - id: availability-p1
          text: Release 10 remains available during the transition.
";

    #[test]
    fn valid_canonical_yaml_reaches_success_path() {
        fuzz_canonical_yaml(VALID_YAML.as_bytes());
        let doc = CanonicalRenderDocument::from_yaml(VALID_YAML).expect("valid YAML should parse");
        assert_eq!(doc.title(), "Quarterly Service Report");
    }

    #[test]
    fn oversized_input_is_ignored() {
        fuzz_canonical_yaml(&vec![b'x'; MAX_INPUT_BYTES + 1]);
    }

    #[test]
    fn invalid_utf8_is_ignored() {
        fuzz_canonical_yaml(&[0xff, 0xfe, 0xfd]);
    }

    #[test]
    fn alias_and_tag_inputs_are_rejected_without_panic() {
        for yaml in [
            "document:\n  title: &a Title\n  sections:\n    - id: s\n      heading: Heading\n      paragraphs:\n        - id: p1\n          text: Text\n",
            "document:\n  title: !mytag Title\n  sections:\n    - id: s\n      heading: Heading\n      paragraphs:\n        - id: p1\n          text: Text\n",
            "document:\n  title: Title\n  sections:\n    - id: s\n      heading: Heading\n      paragraphs:\n        - id: p1\n          text: Text\n---\n",
        ] {
            fuzz_canonical_yaml(yaml.as_bytes());
            assert!(CanonicalRenderDocument::from_yaml(yaml).is_err());
        }
    }
}
