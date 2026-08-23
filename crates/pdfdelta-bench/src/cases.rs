use crate::{
    BenchError, Result,
    canonical::{CanonicalDocument, Paragraph},
    mutation::{Mutation, MutationPlan},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchmarkCase {
    name: String,
    plan: MutationPlan,
}

impl BenchmarkCase {
    pub fn new(
        name: impl Into<String>,
        document: CanonicalDocument,
        mutation: Mutation,
        line_gap: u16,
    ) -> Result<Self> {
        let name = name.into();
        validate_case_name(&name)?;
        Ok(Self {
            name,
            plan: mutation.apply(&document, line_gap)?,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn plan(&self) -> &MutationPlan {
        &self.plan
    }
}

pub fn built_in_cases() -> Result<Vec<BenchmarkCase>> {
    Ok(vec![
        BenchmarkCase::new(
            "line-wrap-only",
            document(&[
                ("opening", "Opening context remains stable"),
                ("target", "A simple release note remains stable"),
                ("closing", "Closing context remains stable"),
            ])?,
            Mutation::LineWrap {
                paragraph_id: "target".to_owned(),
                after_word: 4,
            },
            30,
        )?,
        BenchmarkCase::new(
            "double-line-wrap-only",
            document(&[
                ("opening", "Opening context remains stable"),
                (
                    "target",
                    "A detailed release note remains completely stable across layouts",
                ),
                ("closing", "Closing context remains stable"),
            ])?,
            Mutation::LineWrapTwice {
                paragraph_id: "target".to_owned(),
                after_words: [3, 6],
            },
            30,
        )?,
        BenchmarkCase::new(
            "page-break-only",
            document(&[
                ("first", "First line keeps a steady cadence"),
                ("second", "Second line keeps a steady cadence"),
                ("third", "Third line keeps a steady cadence"),
                ("fourth", "Fourth line keeps a steady cadence"),
            ])?,
            Mutation::PageBreak {
                before_paragraph: 2,
            },
            12,
        )?,
        BenchmarkCase::new(
            "text-replacement",
            document(&[
                ("opening", "Opening paragraph establishes context"),
                ("release", "Release 10 remains available"),
                ("closing", "Closing paragraph confirms context"),
            ])?,
            Mutation::TextReplace {
                paragraph_id: "release".to_owned(),
                new_text: "Release 20 remains available".to_owned(),
            },
            30,
        )?,
        BenchmarkCase::new(
            "text-insertion",
            document(&[
                ("opening", "Opening paragraph establishes context"),
                ("target", "A simple release note remains stable"),
                ("closing", "Closing paragraph confirms context"),
            ])?,
            // deliberate: the inserted run uses scalars that appear nowhere
            // else in the document, so character-level Myers cannot find an
            // equal-cost script that fragments the insertion into hunks.
            Mutation::TextInsert {
                paragraph_id: "target".to_owned(),
                at: "A simple release note ".chars().count(),
                text: "2026 ".to_owned(),
            },
            30,
        )?,
        BenchmarkCase::new(
            "text-deletion",
            document(&[
                ("opening", "Opening paragraph establishes context"),
                ("target", "A very simple release note remains stable"),
                ("closing", "Closing paragraph confirms context"),
            ])?,
            Mutation::TextDelete {
                paragraph_id: "target".to_owned(),
                start: "A ".chars().count(),
                end: "A very ".chars().count(),
            },
            30,
        )?,
        BenchmarkCase::new(
            "number-replacement",
            document(&[
                ("opening", "Opening paragraph establishes context"),
                ("version", "Version 12 ships during quarter 4"),
                ("closing", "Closing paragraph confirms context"),
            ])?,
            Mutation::NumberReplace {
                paragraph_id: "version".to_owned(),
                new_number: "13".to_owned(),
            },
            30,
        )?,
        BenchmarkCase::new(
            "paragraph-insertion",
            document(&[
                ("opening", "Opening paragraph remains stable"),
                ("closing", "Closing paragraph remains stable"),
            ])?,
            Mutation::ParagraphInsert {
                index: 1,
                paragraph: Paragraph::new("inserted", "Inserted paragraph contains generic text")?,
            },
            30,
        )?,
        BenchmarkCase::new(
            "paragraph-deletion",
            document(&[
                ("opening", "Opening paragraph remains stable"),
                ("removed", "Removed paragraph contains generic text"),
                ("closing", "Closing paragraph remains stable"),
            ])?,
            Mutation::ParagraphDelete {
                paragraph_id: "removed".to_owned(),
            },
            30,
        )?,
        BenchmarkCase::new(
            "paragraph-move",
            document(&[
                (
                    "opening",
                    "Opening context paragraph remains stable and uniquely identifies the beginning",
                ),
                (
                    "middle",
                    "Middle context paragraph remains stable and uniquely identifies the body",
                ),
                (
                    "moved",
                    "Moved closing paragraph remains stable and uniquely identifies the ending",
                ),
            ])?,
            Mutation::ParagraphMove {
                paragraph_id: "moved".to_owned(),
                to_index: 0,
            },
            30,
        )?,
    ])
}

fn document(paragraphs: &[(&str, &str)]) -> Result<CanonicalDocument> {
    let paragraphs = paragraphs
        .iter()
        .map(|(id, text)| Paragraph::new(*id, *text))
        .collect::<Result<Vec<_>>>()?;
    CanonicalDocument::new(paragraphs)
}

fn validate_case_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(BenchError::InvalidInput(
            "benchmark case names must be nonblank lowercase ASCII slugs".to_owned(),
        ));
    }
    Ok(())
}
