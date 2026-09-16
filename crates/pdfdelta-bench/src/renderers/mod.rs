mod classic_xref;
mod lopdf_tj;

use crate::{
    BenchError, Result,
    mutation::{PAGE_BOTTOM, PAGE_TOP, RenderPlan},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderLimits {
    pub max_pages: usize,
    pub max_lines_per_page: usize,
    pub max_line_bytes: usize,
    pub max_total_text_bytes: usize,
    pub max_pdf_bytes: usize,
}

impl Default for RenderLimits {
    fn default() -> Self {
        Self {
            max_pages: 4,
            max_lines_per_page: 32,
            max_line_bytes: 512,
            max_total_text_bytes: 16 * 1024,
            max_pdf_bytes: 256 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RendererKind {
    LopdfTj,
    ClassicXrefTj,
}

impl RendererKind {
    #[must_use]
    pub const fn all() -> [Self; 2] {
        [Self::LopdfTj, Self::ClassicXrefTj]
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::LopdfTj => "lopdf-tj",
            Self::ClassicXrefTj => "classic-xref-tj",
        }
    }

    pub fn render(self, plan: &RenderPlan, limits: RenderLimits) -> Result<Vec<u8>> {
        let stats = validate_plan(plan, limits)?;
        match self {
            Self::LopdfTj => lopdf_tj::render(plan, limits),
            Self::ClassicXrefTj => classic_xref::render(plan, limits, stats),
        }
    }
}

#[derive(Clone, Copy)]
struct PlanStats {
    total_text_bytes: usize,
}

fn validate_plan(plan: &RenderPlan, limits: RenderLimits) -> Result<PlanStats> {
    for (name, value) in [
        ("max_pages", limits.max_pages),
        ("max_lines_per_page", limits.max_lines_per_page),
        ("max_line_bytes", limits.max_line_bytes),
        ("max_total_text_bytes", limits.max_total_text_bytes),
        ("max_pdf_bytes", limits.max_pdf_bytes),
    ] {
        if value == 0 {
            return Err(BenchError::InvalidInput(format!(
                "render limit {name} must be greater than zero"
            )));
        }
    }
    if plan.pages().len() > limits.max_pages {
        return Err(BenchError::InvalidInput(format!(
            "render plan exceeds the {}-page limit",
            limits.max_pages
        )));
    }

    let mut total_text_bytes = 0_usize;
    for (page_index, (lines, positions)) in plan.pages().iter().zip(plan.positions()).enumerate() {
        if lines.len() > limits.max_lines_per_page {
            return Err(BenchError::InvalidInput(format!(
                "render plan page {page_index} exceeds the {}-line limit",
                limits.max_lines_per_page
            )));
        }
        let vertical_span = positions
            .iter()
            .map(|position| position.row())
            .max()
            .unwrap_or(0)
            .checked_mul(usize::from(plan.line_gap()))
            .ok_or_else(|| BenchError::InvalidInput("render line span overflowed".to_owned()))?;
        if vertical_span > usize::try_from(PAGE_TOP - PAGE_BOTTOM).unwrap_or(usize::MAX) {
            return Err(BenchError::InvalidInput(format!(
                "render plan page {page_index} does not fit the vertical page area"
            )));
        }
        for (line, position) in lines.iter().zip(positions) {
            if line.len() > limits.max_line_bytes {
                return Err(BenchError::InvalidInput(format!(
                    "render plan line exceeds the {}-byte limit",
                    limits.max_line_bytes
                )));
            }
            let x = plan.margin().checked_add(position.x()).ok_or_else(|| {
                BenchError::InvalidInput("render line horizontal position overflowed".to_owned())
            })?;
            if x >= plan.page_width() {
                return Err(BenchError::InvalidInput(format!(
                    "render plan page {page_index} line origin {x} lies outside page width {}",
                    plan.page_width()
                )));
            }
            total_text_bytes = total_text_bytes.checked_add(line.len()).ok_or_else(|| {
                BenchError::InvalidInput("render text byte count overflowed".to_owned())
            })?;
            if total_text_bytes > limits.max_total_text_bytes {
                return Err(BenchError::InvalidInput(format!(
                    "render plan exceeds the {}-byte text limit",
                    limits.max_total_text_bytes
                )));
            }
        }
    }
    Ok(PlanStats { total_text_bytes })
}

fn escape_pdf_literal(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '(' | ')' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn line_y(row: usize, line_gap: u16) -> Result<i64> {
    let row = i64::try_from(row)
        .map_err(|_| BenchError::InvalidInput("render line index exceeds i64".to_owned()))?;
    Ok(PAGE_TOP - row * i64::from(line_gap))
}

fn render_error(renderer: &'static str, message: impl Into<String>) -> BenchError {
    BenchError::Render {
        renderer,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_serialization_matches_name() {
        for renderer in RendererKind::all() {
            assert_eq!(
                serde_json::to_string(&renderer).expect("renderer serializes"),
                format!("\"{}\"", renderer.name())
            );
        }
    }
}
