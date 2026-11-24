//! Generated fixtures whose only difference is token order.
//!
//! Every case below renders the same words twice in a different reading order,
//! so the token multiset of both PDFs must be identical and any reported change
//! comes from order alone. The cases separate legitimate relocations (a whole
//! paragraph moved across a page) from edits that change what the document says
//! (a table row swap, a list item swap, a negated word order). A diff engine may
//! describe a genuine edit as moves, but it must not reduce a meaning-changing
//! reorder to "no content change".
//!
//! The fixtures use the same [`RendererKind`] renderers and [`RenderPlan`]
//! construction as the built-in acceptance matrix, so renderer behaviour is
//! shared rather than reimplemented.
//!
//! [`RendererKind`]: crate::renderers::RendererKind

use crate::{
    BenchError, Result,
    cases::built_in_cases,
    mutation::{RenderLine, RenderPlan},
};

/// What the fixture says a correct engine must conclude about the pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReorderExpectation {
    /// Token order carries meaning, so the pair must not be "no content change".
    MeaningChanged,
    /// A complete paragraph moved and the content is unchanged; a move change
    /// is the expected description.
    LegitimateMove,
    /// Only layout or block order changed; the pair must be "no content change".
    NoContentChange,
}

impl ReorderExpectation {
    /// Stable manifest and report label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::MeaningChanged => "meaning-changed",
            Self::LegitimateMove => "legitimate-move",
            Self::NoContentChange => "no-content-change",
        }
    }
}

/// One generated pair of render plans with the intent of the reorder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReorderCase {
    id: String,
    description: &'static str,
    expectation: ReorderExpectation,
    old: RenderPlan,
    new: RenderPlan,
}

impl ReorderCase {
    /// Case slug, unique within [`reorder_cases`].
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// One sentence explaining which document property the case isolates.
    #[must_use]
    pub const fn description(&self) -> &'static str {
        self.description
    }

    /// Expected conclusion for a correct engine.
    #[must_use]
    pub const fn expectation(&self) -> ReorderExpectation {
        self.expectation
    }

    /// Unmodified revision.
    #[must_use]
    pub const fn old(&self) -> &RenderPlan {
        &self.old
    }

    /// Reordered revision.
    #[must_use]
    pub const fn new_plan(&self) -> &RenderPlan {
        &self.new
    }
}

const LINE_GAP: u16 = 24;

/// Builds one render line positioned at a column origin relative to the margin.
fn cell(text: &str, row: usize, x: u16) -> Result<RenderLine> {
    RenderLine::new(text, row, x)
}

fn one_page(lines: Vec<RenderLine>) -> Result<RenderPlan> {
    RenderPlan::positioned(vec![lines], LINE_GAP)
}

fn two_pages(first: Vec<RenderLine>, second: Vec<RenderLine>) -> Result<RenderPlan> {
    RenderPlan::positioned(vec![first, second], LINE_GAP)
}

fn built_in_pair(name: &str) -> Result<(RenderPlan, RenderPlan)> {
    let case = built_in_cases()?
        .into_iter()
        .find(|case| case.name() == name)
        .ok_or_else(|| BenchError::InvalidInput(format!("built-in case {name:?} is not listed")))?;
    Ok((case.plan().old().clone(), case.plan().new_plan().clone()))
}

/// Builds every reorder fixture in deterministic order.
///
/// # Errors
///
/// Returns [`BenchError::InvalidInput`] when a line, page, or built-in control
/// cannot be constructed.
pub fn reorder_cases() -> Result<Vec<ReorderCase>> {
    Ok(vec![
        table_cell_swap()?,
        table_row_swap()?,
        key_value_swap()?,
        word_order_swap()?,
        list_item_swap()?,
        paragraph_move_across_page()?,
        sentence_move_within_paragraph()?,
        control_column_change()?,
        control_two_column_reorder()?,
        control_line_wrap()?,
        control_page_break()?,
    ])
}

fn table_cell_swap() -> Result<ReorderCase> {
    let header = vec![
        cell("Item", 0, 0)?,
        cell("Q1", 0, 140)?,
        cell("Q2", 0, 220)?,
    ];
    let widget = vec![
        cell("Widget", 1, 0)?,
        cell("120", 1, 140)?,
        cell("450", 1, 220)?,
    ];
    let swapped_widget = vec![
        cell("Widget", 1, 0)?,
        cell("450", 1, 140)?,
        cell("120", 1, 220)?,
    ];
    let gadget = vec![
        cell("Gadget", 2, 0)?,
        cell("80", 2, 140)?,
        cell("300", 2, 220)?,
    ];
    let total = vec![
        cell("Total", 3, 0)?,
        cell("200", 3, 140)?,
        cell("750", 3, 220)?,
    ];
    Ok(ReorderCase {
        id: "table-cell-swap-in-row".to_owned(),
        description: "two numeric cells are exchanged inside one table row",
        expectation: ReorderExpectation::MeaningChanged,
        old: one_page([header.clone(), widget, gadget.clone(), total.clone()].concat())?,
        new: one_page([header, swapped_widget, gadget, total].concat())?,
    })
}

fn table_row_swap() -> Result<ReorderCase> {
    let row = |row: usize, values: [&str; 6]| -> Result<Vec<RenderLine>> {
        let origins = [0, 100, 190, 280, 370, 460];
        values
            .iter()
            .zip(origins)
            .map(|(text, x)| cell(text, row, x))
            .collect()
    };
    let header = row(
        0,
        ["Region", "Units", "Revenue", "Currency", "Owner", "Share"],
    )?;
    let north = row(1, ["North", "120", "4500", "EUR", "Alice", "12"])?;
    let south = row(2, ["South", "90", "3100", "USD", "Bob", "8"])?;
    let east = row(3, ["East", "60", "2200", "GBP", "Carol", "5"])?;
    let swapped_north = row(1, ["South", "90", "3100", "USD", "Bob", "8"])?;
    let swapped_south = row(2, ["North", "120", "4500", "EUR", "Alice", "12"])?;
    Ok(ReorderCase {
        id: "table-row-swap".to_owned(),
        description: "two complete table rows are exchanged; every value now sits beside a different label",
        expectation: ReorderExpectation::MeaningChanged,
        old: one_page([header.clone(), north, south, east.clone()].concat())?,
        new: one_page([header, swapped_north, swapped_south, east].concat())?,
    })
}

fn key_value_swap() -> Result<ReorderCase> {
    let owner = vec![cell("Owner:", 0, 0)?, cell("Alice M Smith", 0, 150)?];
    let reviewer = vec![cell("Reviewer:", 1, 0)?, cell("Bob K Jones", 1, 150)?];
    let approver = vec![cell("Approver:", 2, 0)?, cell("Carol R Lee", 2, 150)?];
    let swapped_owner = vec![cell("Owner:", 0, 0)?, cell("Bob K Jones", 0, 150)?];
    let swapped_reviewer = vec![cell("Reviewer:", 1, 0)?, cell("Alice M Smith", 1, 150)?];
    Ok(ReorderCase {
        id: "key-value-column-swap".to_owned(),
        description: "two values are exchanged between the rows of a two-column key/value list",
        expectation: ReorderExpectation::MeaningChanged,
        old: one_page([owner, reviewer, approver.clone()].concat())?,
        new: one_page([swapped_owner, swapped_reviewer, approver].concat())?,
    })
}

fn word_order_swap() -> Result<ReorderCase> {
    let opening = cell("Release notes describe how the default policy", 0, 0)?;
    let old_negation = cell("may not always apply to archived records.", 1, 0)?;
    let new_negation = cell("may always not apply to archived records.", 1, 0)?;
    let closing = cell("Operators should confirm the effective rule.", 2, 0)?;
    Ok(ReorderCase {
        id: "word-order-swap".to_owned(),
        description: "the negation words are exchanged inside one sentence",
        expectation: ReorderExpectation::MeaningChanged,
        old: one_page(vec![opening.clone(), old_negation, closing.clone()])?,
        new: one_page(vec![opening, new_negation, closing])?,
    })
}

fn list_item_swap() -> Result<ReorderCase> {
    let heading = cell("Steps for the migration", 0, 0)?;
    let first = cell(
        "1. Configure the primary repository before enabling indexing",
        1,
        0,
    )?;
    let second = cell(
        "2. Enable indexing only after the initial synchronization completes",
        2,
        0,
    )?;
    let third = cell(
        "3. Verify the dashboard reports the expected document counts",
        3,
        0,
    )?;
    let swapped_first = cell(
        "1. Enable indexing only after the initial synchronization completes",
        1,
        0,
    )?;
    let swapped_second = cell(
        "2. Configure the primary repository before enabling indexing",
        2,
        0,
    )?;
    Ok(ReorderCase {
        id: "list-item-swap".to_owned(),
        description: "two numbered list items exchange their bodies; the numbering is kept",
        expectation: ReorderExpectation::MeaningChanged,
        old: one_page(vec![heading.clone(), first, second, third.clone()])?,
        new: one_page(vec![heading, swapped_first, swapped_second, third])?,
    })
}

fn paragraph_move_across_page() -> Result<ReorderCase> {
    let heading = cell("Section 2 Operational controls", 0, 0)?;
    let opening = cell(
        "Opening context remains stable for every review cycle.",
        1,
        0,
    )?;
    let moved_first = cell(
        "The review starts with a scope statement that names every system.",
        2,
        0,
    )?;
    let moved_second = cell(
        "Each control owner then supplies evidence for the current period.",
        3,
        0,
    )?;
    let closing = cell(
        "Closing context remains stable for every review cycle.",
        4,
        0,
    )?;
    let appendix_heading = cell("Appendix A Reference material", 0, 0)?;
    let appendix_body = cell("Reference tables remain unchanged in this revision.", 1, 0)?;
    Ok(ReorderCase {
        id: "paragraph-move-across-page".to_owned(),
        description: "a complete paragraph moves from page 1 to page 2 with unchanged text",
        expectation: ReorderExpectation::LegitimateMove,
        old: two_pages(
            vec![
                heading.clone(),
                opening.clone(),
                moved_first.clone(),
                moved_second.clone(),
                closing.clone(),
            ],
            vec![appendix_heading.clone(), appendix_body.clone()],
        )?,
        new: two_pages(
            vec![heading, opening, closing],
            vec![appendix_heading, appendix_body, moved_first, moved_second],
        )?,
    })
}

fn sentence_move_within_paragraph() -> Result<ReorderCase> {
    let first = cell(
        "The reviewer records every finding in the register and then",
        0,
        0,
    )?;
    let first_wrap = cell("forwards it to the approving officer.", 1, 0)?;
    let moved = cell("Evidence must be collected before the review begins.", 2, 0)?;
    let closing = cell(
        "Approval follows only after the register is complete.",
        3,
        0,
    )?;
    let moved_first = cell("Evidence must be collected before the review begins.", 0, 0)?;
    let first_second = cell(
        "The reviewer records every finding in the register and then",
        1,
        0,
    )?;
    let first_wrap_third = cell("forwards it to the approving officer.", 2, 0)?;
    Ok(ReorderCase {
        id: "sentence-move-within-paragraph".to_owned(),
        description: "one sentence moves to the front of its paragraph on the same page",
        expectation: ReorderExpectation::MeaningChanged,
        old: one_page(vec![first, first_wrap, moved, closing.clone()])?,
        new: one_page(vec![moved_first, first_second, first_wrap_third, closing])?,
    })
}

fn control_column_change() -> Result<ReorderCase> {
    let (old, new) = built_in_pair("column-change-only")?;
    Ok(ReorderCase {
        id: "control-column-change".to_owned(),
        description: "the same paragraphs are reflowed from one column into two",
        expectation: ReorderExpectation::NoContentChange,
        old,
        new,
    })
}

fn control_two_column_reorder() -> Result<ReorderCase> {
    let first = cell("Alpha section explains the first control.", 0, 0)?;
    let second = cell("Beta section explains the second control.", 1, 0)?;
    let third = cell("Gamma section explains the third control.", 2, 0)?;
    let fourth = cell("Delta section explains the fourth control.", 3, 0)?;
    let third_left = cell("Gamma section explains the third control.", 0, 0)?;
    let fourth_left = cell("Delta section explains the fourth control.", 1, 0)?;
    let first_right = cell("Alpha section explains the first control.", 0, 280)?;
    let second_right = cell("Beta section explains the second control.", 1, 280)?;
    Ok(ReorderCase {
        id: "control-two-column-reorder".to_owned(),
        description: "the same four blocks move into two columns in a different block order",
        expectation: ReorderExpectation::NoContentChange,
        old: one_page(vec![first, second, third, fourth])?,
        new: one_page(vec![third_left, fourth_left, first_right, second_right])?,
    })
}

fn control_line_wrap() -> Result<ReorderCase> {
    let (old, new) = built_in_pair("line-wrap-only")?;
    Ok(ReorderCase {
        id: "control-line-wrap".to_owned(),
        description: "one paragraph is re-wrapped without changing its words",
        expectation: ReorderExpectation::NoContentChange,
        old,
        new,
    })
}

fn control_page_break() -> Result<ReorderCase> {
    let (old, new) = built_in_pair("page-break-only")?;
    Ok(ReorderCase {
        id: "control-page-break".to_owned(),
        description: "one paragraph list is split across a page break",
        expectation: ReorderExpectation::NoContentChange,
        old,
        new,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn cases_have_unique_ids_and_distinct_revisions() {
        let cases = reorder_cases().expect("reorder fixtures are constructible");
        let ids = cases
            .iter()
            .map(|case| case.id().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), cases.len(), "case ids must be unique");
        for case in &cases {
            assert_ne!(
                case.old(),
                case.new_plan(),
                "{} must differ between revisions",
                case.id()
            );
        }
    }
}
