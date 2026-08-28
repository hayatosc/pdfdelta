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
            "column-change-only",
            document(&[
                ("left-first", "Left first paragraph remains stable"),
                ("left-second", "Left second paragraph remains stable"),
                ("right-first", "Right first paragraph remains stable"),
                ("right-second", "Right second paragraph remains stable"),
            ])?,
            Mutation::ColumnChange,
            30,
        )?,
        BenchmarkCase::new(
            "line-height-change-only",
            document(&[
                ("opening", "Opening paragraph establishes context"),
                ("target", "A simple release note remains stable"),
                ("closing", "Closing paragraph confirms context"),
            ])?,
            Mutation::LineHeightChange { new_line_gap: 48 },
            30,
        )?,
        BenchmarkCase::new(
            "margin-change-only",
            document(&[
                ("opening", "Opening paragraph establishes context"),
                ("target", "A simple release note remains stable"),
                ("closing", "Closing paragraph confirms context"),
            ])?,
            Mutation::MarginChange { new_margin: 96 },
            30,
        )?,
        BenchmarkCase::new(
            "font-size-change-only",
            document(&[
                ("opening", "Opening paragraph establishes context"),
                ("target", "A simple release note remains stable"),
                ("closing", "Closing paragraph confirms context"),
            ])?,
            Mutation::FontSizeChange { new_font_size: 14 },
            30,
        )?,
        BenchmarkCase::new(
            "page-size-change-only",
            document(&[
                ("opening", "Opening paragraph establishes context"),
                ("target", "A simple release note remains stable"),
                ("closing", "Closing paragraph confirms context"),
            ])?,
            Mutation::PageSizeChange {
                new_page_width: 595,
                new_page_height: 842,
            },
            30,
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
        BenchmarkCase::new(
            "repeated-obligations-number-replacement",
            document(&[
                (
                    "service",
                    "The supplier shall retain service records for 5 years",
                ),
                (
                    "security",
                    "The supplier shall retain security records for 7 years",
                ),
                (
                    "billing",
                    "The supplier shall retain billing records for 5 years",
                ),
                (
                    "audit",
                    "The supplier shall retain audit records for 5 years",
                ),
            ])?,
            Mutation::NumberReplace {
                paragraph_id: "security".to_owned(),
                new_number: "10".to_owned(),
            },
            30,
        )?,
        BenchmarkCase::new(
            "repeated-terms-text-replacement",
            document(&[
                (
                    "north",
                    "Regional support remains available during local business hours",
                ),
                (
                    "south",
                    "Regional support remains available during local office hours",
                ),
                (
                    "east",
                    "Regional support remains available during local business hours",
                ),
                (
                    "west",
                    "Regional support remains available during local business hours",
                ),
            ])?,
            Mutation::TextReplace {
                paragraph_id: "south".to_owned(),
                new_text: "Regional support remains available during extended office hours"
                    .to_owned(),
            },
            30,
        )?,
        BenchmarkCase::new(
            "long-prose-double-reflow",
            document(&[
                (
                    "summary",
                    "Executive summary and review scope remain unchanged",
                ),
                (
                    "analysis",
                    "The review team examined each control and recorded the evidence before issuing its final assessment",
                ),
                (
                    "finding",
                    "The resulting finding and recommendation remain unchanged",
                ),
            ])?,
            Mutation::LineWrapTwice {
                paragraph_id: "analysis".to_owned(),
                after_words: [6, 12],
            },
            30,
        )?,
        BenchmarkCase::new(
            "numbered-requirement-text-insertion",
            document(&[
                (
                    "requirement-1",
                    "1. The operator shall record each access request",
                ),
                (
                    "requirement-2",
                    "2. The operator shall review each access request",
                ),
                (
                    "requirement-3",
                    "3. The operator shall archive each access request",
                ),
            ])?,
            Mutation::TextInsert {
                paragraph_id: "requirement-2".to_owned(),
                at: "2. The operator shall ".chars().count(),
                text: "independently ".to_owned(),
            },
            30,
        )?,
        BenchmarkCase::new(
            "pagination-churn-mid-document",
            document(&[
                ("overview", "Overview of the annual compliance review"),
                ("scope", "Scope and responsible teams remain unchanged"),
                ("method", "Review methods and evidence remain unchanged"),
                ("control-1", "Control one remains effective and unchanged"),
                ("control-2", "Control two remains effective and unchanged"),
                ("control-3", "Control three remains effective and unchanged"),
                ("finding-1", "Finding one remains resolved and unchanged"),
                ("finding-2", "Finding two remains resolved and unchanged"),
                ("approval", "Final approval and signoff remain unchanged"),
            ])?,
            Mutation::PageBreak {
                before_paragraph: 4,
            },
            24,
        )?,
        BenchmarkCase::new(
            "footnote-like-paragraph-insertion",
            document(&[
                ("policy", "The policy applies to all production systems"),
                ("exception", "Approved exceptions require annual review"),
                (
                    "footnote-1",
                    "Footnote 1. Production excludes training systems",
                ),
            ])?,
            Mutation::ParagraphInsert {
                index: 3,
                paragraph: Paragraph::new(
                    "footnote-2",
                    "Footnote 2. Annual review occurs each January",
                )?,
            },
            30,
        )?,
        BenchmarkCase::new(
            "footnote-like-paragraph-deletion",
            document(&[
                (
                    "report",
                    "The report covers the consolidated operating results",
                ),
                (
                    "footnote-1",
                    "Footnote 1. Amounts are rounded to whole dollars",
                ),
                (
                    "footnote-2",
                    "Footnote 2. Prior periods use constant currency",
                ),
                (
                    "footnote-3",
                    "Footnote 3. Totals may differ due to rounding",
                ),
            ])?,
            Mutation::ParagraphDelete {
                paragraph_id: "footnote-2".to_owned(),
            },
            30,
        )?,
        BenchmarkCase::new(
            "section-labeled-paragraph-movement",
            document(&[
                ("introduction", "Section 1 Introduction and purpose"),
                ("definitions", "Section 2 Definitions and interpretation"),
                ("operations", "Section 3 Operating requirements"),
                ("audit", "Section 4 Audit rights and records"),
                ("termination", "Section 5 Termination and transition"),
            ])?,
            Mutation::ParagraphMove {
                paragraph_id: "audit".to_owned(),
                to_index: 1,
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
