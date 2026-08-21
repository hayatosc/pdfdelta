use std::{cell::Cell, rc::Rc};

use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Operand {
    Number(f64),
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<Self>),
    Dictionary(Vec<(Vec<u8>, Self)>),
    Boolean(bool),
    Null,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Operation {
    pub(crate) operands: Vec<Operand>,
    pub(crate) operator: Vec<u8>,
    pub(crate) index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContentLimits {
    pub(crate) max_operators: usize,
    pub(crate) max_operand_stack: usize,
    pub(crate) max_operand_nodes: usize,
    pub(crate) max_nesting_depth: usize,
    pub(crate) max_string_bytes: usize,
}

impl Default for ContentLimits {
    fn default() -> Self {
        Self {
            max_operators: 1_000_000,
            max_operand_stack: 1_024,
            max_operand_nodes: 1_000_000,
            max_nesting_depth: 64,
            max_string_bytes: 16 * 1024 * 1024,
        }
    }
}

#[cfg(test)]
fn parse_operations(input: &[u8], limits: ContentLimits) -> Result<Vec<Operation>> {
    let mut parser = ContentParser::new(limits);
    let operations = parser.parse_fragment(input)?;
    parser.finish()?;
    Ok(operations)
}

pub(crate) struct ContentParser {
    limits: ContentLimits,
    pending_operands: Vec<Operand>,
    pending_fragment: Vec<u8>,
    operator_budget: OperatorBudget,
    operand_budget: OperandBudget,
}

struct ParsedFragment {
    operations: Vec<Operation>,
    incomplete_operand_start: Option<usize>,
}

#[derive(Clone)]
pub(crate) struct OperatorBudget {
    limit: usize,
    remaining: Rc<Cell<usize>>,
}

#[derive(Clone)]
pub(crate) struct OperandBudget {
    limit: usize,
    remaining: Rc<Cell<usize>>,
}

impl ContentParser {
    #[cfg(test)]
    pub(crate) fn new(limits: ContentLimits) -> Self {
        let operator_budget = OperatorBudget::new(limits.max_operators);
        let operand_budget = OperandBudget::new(limits.max_operand_nodes);
        Self::with_budgets(limits, operator_budget, operand_budget)
    }

    pub(crate) fn with_budgets(
        limits: ContentLimits,
        operator_budget: OperatorBudget,
        operand_budget: OperandBudget,
    ) -> Self {
        Self {
            limits,
            pending_operands: Vec::new(),
            pending_fragment: Vec::new(),
            operator_budget,
            operand_budget,
        }
    }

    pub(crate) fn parse_fragment(&mut self, input: &[u8]) -> Result<Vec<Operation>> {
        let retrying_incomplete_operand = !self.pending_fragment.is_empty();
        let buffered = if retrying_incomplete_operand {
            let mut buffered = std::mem::take(&mut self.pending_fragment);
            // Page content streams are separate lexical segments. LF both separates
            // adjacent tokens and terminates a comment at the prior stream boundary.
            buffered.push(b'\n');
            buffered.extend_from_slice(input);
            Some(buffered)
        } else {
            None
        };
        let input = buffered.as_deref().unwrap_or(input);
        let parsed = Parser::new(
            input,
            self.limits,
            self.operator_budget.clone(),
            self.operand_budget.clone(),
        )
        .parse_fragment(&mut self.pending_operands)?;
        if let Some(start) = parsed.incomplete_operand_start {
            if retrying_incomplete_operand {
                return Err(Error::Unresolved(
                    "dictionary value remains incomplete in the next content stream".to_owned(),
                ));
            }
            self.pending_fragment.extend_from_slice(&input[start..]);
        }
        Ok(parsed.operations)
    }

    pub(crate) fn finish(self) -> Result<()> {
        if !self.pending_fragment.is_empty() {
            Err(Error::Unresolved(
                "content stream sequence ends with an incomplete dictionary value".to_owned(),
            ))
        } else if self.pending_operands.is_empty() {
            Ok(())
        } else {
            Err(Error::Unresolved(
                "content stream sequence ends with operands but no operator".to_owned(),
            ))
        }
    }
}

impl OperatorBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            remaining: Rc::new(Cell::new(limit)),
        }
    }

    fn reserve(&self) -> Result<()> {
        let remaining = self.remaining.get();
        if remaining == 0 {
            return Err(Error::LimitExceeded {
                resource: "content operators",
                limit: self.limit,
            });
        }
        self.remaining.set(remaining - 1);
        Ok(())
    }
}

impl OperandBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            remaining: Rc::new(Cell::new(limit)),
        }
    }

    fn reserve(&self) -> Result<()> {
        let remaining = self.remaining.get();
        if remaining == 0 {
            return Err(Error::LimitExceeded {
                resource: "content operand nodes",
                limit: self.limit,
            });
        }
        self.remaining.set(remaining - 1);
        Ok(())
    }

    fn restore(&self, remaining: usize) {
        self.remaining.set(remaining);
    }
}

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
    limits: ContentLimits,
    operator_count: usize,
    operator_budget: OperatorBudget,
    operand_budget: OperandBudget,
    incomplete_dictionary_value: bool,
}

impl<'a> Parser<'a> {
    fn new(
        input: &'a [u8],
        limits: ContentLimits,
        operator_budget: OperatorBudget,
        operand_budget: OperandBudget,
    ) -> Self {
        Self {
            input,
            position: 0,
            limits,
            operator_count: 0,
            operator_budget,
            operand_budget,
            incomplete_dictionary_value: false,
        }
    }

    fn parse_fragment(mut self, operands: &mut Vec<Operand>) -> Result<ParsedFragment> {
        let mut operations = Vec::new();

        loop {
            self.skip_space_and_comments();
            let Some(byte) = self.peek() else {
                break;
            };

            if self.starts_operand(byte) {
                let checkpoint = (
                    self.position,
                    self.operand_budget.remaining.get(),
                    operands.len(),
                );
                let operand = match self.parse_operand(0) {
                    Ok(operand) => operand,
                    Err(Error::Unresolved(_)) if self.incomplete_dictionary_value => {
                        let (start, remaining, operand_count) = checkpoint;
                        self.operand_budget.restore(remaining);
                        operands.truncate(operand_count);
                        return Ok(ParsedFragment {
                            operations,
                            incomplete_operand_start: Some(start),
                        });
                    }
                    Err(error) => return Err(error),
                };
                self.push_operand(operands, operand)?;
                continue;
            }

            if matches!(byte, b']' | b')' | b'>') {
                return self.unresolved("unexpected closing delimiter");
            }

            let token = self.read_regular_token()?;
            if let Some(operand) = self.keyword_operand(token) {
                self.reserve_operand_node()?;
                self.push_operand(operands, operand)?;
            } else if looks_numeric(token) {
                return self.unresolved("invalid numeric operand");
            } else {
                let index = self.next_operator_index()?;
                if token == b"BI" {
                    operands.clear();
                    self.skip_inline_image()?;
                    operations.push(Operation {
                        operands: Vec::new(),
                        operator: token.to_vec(),
                        index,
                    });
                    continue;
                }
                operations.push(Operation {
                    operands: std::mem::take(operands),
                    operator: token.to_vec(),
                    index,
                });
            }
        }

        Ok(ParsedFragment {
            operations,
            incomplete_operand_start: None,
        })
    }

    fn parse_operand(&mut self, depth: usize) -> Result<Operand> {
        self.reserve_operand_node()?;
        match self.peek() {
            Some(b'/') => self.parse_name().map(Operand::Name),
            Some(b'(') => {
                self.check_depth(depth + 1)?;
                self.parse_literal_string(depth + 1).map(Operand::String)
            }
            Some(b'<') => {
                if self.input[self.position..].starts_with(b"<<") {
                    self.parse_dictionary(depth)
                } else {
                    self.check_depth(depth + 1)?;
                    self.parse_hex_string().map(Operand::String)
                }
            }
            Some(b'[') => self.parse_array(depth),
            Some(_) => {
                let token = self.read_regular_token()?;
                if let Some(operand) = self.keyword_operand(token) {
                    Ok(operand)
                } else {
                    self.unresolved("operator is not allowed inside an array")
                }
            }
            None => self.unresolved("expected an operand"),
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<Operand> {
        self.check_depth(depth + 1)?;
        self.position += 1;
        let mut values = Vec::new();
        loop {
            self.skip_space_and_comments();
            match self.peek() {
                Some(b']') => {
                    self.position += 1;
                    return Ok(Operand::Array(values));
                }
                Some(_) => {
                    let value = self.parse_operand(depth + 1)?;
                    self.push_operand(&mut values, value)?;
                }
                None => return self.unresolved("unterminated array"),
            }
        }
    }

    fn parse_dictionary(&mut self, depth: usize) -> Result<Operand> {
        self.check_depth(depth + 1)?;
        self.position += 2;
        let mut entries = Vec::new();
        loop {
            self.skip_space_and_comments();
            if self.input[self.position..].starts_with(b">>") {
                self.position += 2;
                return Ok(Operand::Dictionary(entries));
            }
            if self.peek().is_none() {
                return self.unresolved("unterminated dictionary");
            }
            if self.peek() != Some(b'/') {
                return self.unresolved("dictionary key must be a name");
            }
            let key = self.parse_name()?;
            self.skip_space_and_comments();
            if self.input[self.position..].starts_with(b">>") {
                return self.unresolved("dictionary key is missing a value");
            }
            if self.peek().is_none() {
                self.incomplete_dictionary_value = true;
            }
            let value = self.parse_operand(depth + 1)?;
            if entries.len() >= self.limits.max_operand_stack {
                return Err(Error::LimitExceeded {
                    resource: "content operand stack",
                    limit: self.limits.max_operand_stack,
                });
            }
            entries.push((key, value));
        }
    }

    fn parse_name(&mut self) -> Result<Vec<u8>> {
        self.position += 1;
        let mut name = Vec::new();
        while let Some(byte) = self.peek() {
            if is_whitespace(byte) || is_delimiter(byte) {
                break;
            }
            self.position += 1;
            if byte == b'#' {
                let high = self.take_hex_digit("invalid name escape")?;
                let low = self.take_hex_digit("invalid name escape")?;
                name.push((high << 4) | low);
            } else {
                name.push(byte);
            }
        }
        Ok(name)
    }

    fn parse_literal_string(&mut self, base_depth: usize) -> Result<Vec<u8>> {
        self.position += 1;
        let mut value = Vec::new();
        let mut parentheses = 1usize;
        while let Some(byte) = self.take() {
            match byte {
                b'(' => {
                    parentheses = parentheses.checked_add(1).ok_or(Error::LimitExceeded {
                        resource: "content nesting depth",
                        limit: self.limits.max_nesting_depth,
                    })?;
                    self.check_depth(base_depth - 1 + parentheses)?;
                    self.push_string_byte(&mut value, byte)?;
                }
                b')' => {
                    parentheses -= 1;
                    if parentheses == 0 {
                        return Ok(value);
                    }
                    self.push_string_byte(&mut value, byte)?;
                }
                b'\\' => self.parse_string_escape(&mut value)?,
                _ => self.push_string_byte(&mut value, byte)?,
            }
        }
        self.unresolved("unterminated literal string")
    }

    fn parse_string_escape(&mut self, value: &mut Vec<u8>) -> Result<()> {
        let Some(escaped) = self.take() else {
            return self.unresolved("unterminated literal string escape");
        };
        match escaped {
            b'n' => self.push_string_byte(value, b'\n'),
            b'r' => self.push_string_byte(value, b'\r'),
            b't' => self.push_string_byte(value, b'\t'),
            b'b' => self.push_string_byte(value, 0x08),
            b'f' => self.push_string_byte(value, 0x0c),
            b'\n' => Ok(()),
            b'\r' => {
                if self.peek() == Some(b'\n') {
                    self.position += 1;
                }
                Ok(())
            }
            b'0'..=b'7' => {
                let mut octal = u16::from(escaped - b'0');
                for _ in 0..2 {
                    match self.peek() {
                        Some(next @ b'0'..=b'7') => {
                            self.position += 1;
                            octal = octal * 8 + u16::from(next - b'0');
                        }
                        _ => break,
                    }
                }
                self.push_string_byte(value, octal as u8)
            }
            _ => self.push_string_byte(value, escaped),
        }
    }

    fn parse_hex_string(&mut self) -> Result<Vec<u8>> {
        self.position += 1;
        let mut value = Vec::new();
        let mut high_nibble = None;
        loop {
            let Some(byte) = self.take() else {
                return self.unresolved("unterminated hexadecimal string");
            };
            if byte == b'>' {
                if let Some(high) = high_nibble {
                    self.push_string_byte(&mut value, high << 4)?;
                }
                return Ok(value);
            }
            if is_whitespace(byte) {
                continue;
            }
            let digit = hex_value(byte)
                .ok_or(Error::Unresolved("invalid hexadecimal string".to_owned()))?;
            if let Some(high) = high_nibble.take() {
                self.push_string_byte(&mut value, (high << 4) | digit)?;
            } else {
                high_nibble = Some(digit);
            }
        }
    }

    fn keyword_operand(&self, token: &[u8]) -> Option<Operand> {
        match token {
            b"true" => Some(Operand::Boolean(true)),
            b"false" => Some(Operand::Boolean(false)),
            b"null" => Some(Operand::Null),
            _ if is_number(token) => std::str::from_utf8(token)
                .ok()?
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .map(Operand::Number),
            _ => None,
        }
    }

    fn skip_inline_image(&mut self) -> Result<()> {
        loop {
            self.skip_space_and_comments();
            let Some(byte) = self.peek() else {
                return self.unresolved("inline image is missing ID");
            };
            match byte {
                b'/' => {
                    self.parse_name()?;
                }
                _ if self.starts_operand(byte) => {
                    self.parse_operand(0)?;
                }
                b']' | b')' | b'>' => return self.unresolved("invalid inline image dictionary"),
                _ => {
                    let token = self.read_regular_token()?;
                    if token == b"ID" {
                        break;
                    }
                }
            }
        }

        match self.take() {
            Some(b'\r') => {
                if self.peek() == Some(b'\n') {
                    self.position += 1;
                }
            }
            Some(byte) if is_whitespace(byte) => {}
            _ => return self.unresolved("inline image ID is not followed by whitespace"),
        }

        while self.position + 1 < self.input.len() {
            if self.input[self.position] == b'E'
                && self.input[self.position + 1] == b'I'
                && self.position > 0
                && is_whitespace(self.input[self.position - 1])
                && self
                    .input
                    .get(self.position + 2)
                    .is_none_or(|&byte| is_whitespace(byte) || is_delimiter(byte))
            {
                self.position += 2;
                return Ok(());
            }
            self.position += 1;
        }
        self.unresolved("unterminated inline image data")
    }

    fn starts_operand(&self, byte: u8) -> bool {
        matches!(byte, b'/' | b'(' | b'<' | b'[')
            || byte.is_ascii_digit()
            || matches!(byte, b'+' | b'-' | b'.')
    }

    fn read_regular_token(&mut self) -> Result<&'a [u8]> {
        let start = self.position;
        while let Some(byte) = self.peek() {
            if is_whitespace(byte) || is_delimiter(byte) {
                break;
            }
            self.position += 1;
        }
        if start == self.position {
            self.unresolved("expected a regular token")
        } else {
            Ok(&self.input[start..self.position])
        }
    }

    fn next_operator_index(&mut self) -> Result<u32> {
        self.operator_budget.reserve()?;
        let index = u32::try_from(self.operator_count).map_err(|_| Error::LimitExceeded {
            resource: "content operator index",
            limit: u32::MAX as usize,
        })?;
        self.operator_count += 1;
        Ok(index)
    }

    fn push_operand(&self, operands: &mut Vec<Operand>, operand: Operand) -> Result<()> {
        if operands.len() >= self.limits.max_operand_stack {
            return Err(Error::LimitExceeded {
                resource: "content operand stack",
                limit: self.limits.max_operand_stack,
            });
        }
        operands.push(operand);
        Ok(())
    }

    fn reserve_operand_node(&self) -> Result<()> {
        self.operand_budget.reserve()
    }

    fn push_string_byte(&self, value: &mut Vec<u8>, byte: u8) -> Result<()> {
        if value.len() >= self.limits.max_string_bytes {
            return Err(Error::LimitExceeded {
                resource: "content string bytes",
                limit: self.limits.max_string_bytes,
            });
        }
        value.push(byte);
        Ok(())
    }

    fn check_depth(&self, depth: usize) -> Result<()> {
        if depth > self.limits.max_nesting_depth {
            Err(Error::LimitExceeded {
                resource: "content nesting depth",
                limit: self.limits.max_nesting_depth,
            })
        } else {
            Ok(())
        }
    }

    fn take_hex_digit(&mut self, message: &'static str) -> Result<u8> {
        self.take()
            .and_then(hex_value)
            .ok_or_else(|| Error::Unresolved(message.to_owned()))
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            while self.peek().is_some_and(is_whitespace) {
                self.position += 1;
            }
            if self.peek() != Some(b'%') {
                return;
            }
            while let Some(byte) = self.take() {
                if matches!(byte, b'\r' | b'\n') {
                    break;
                }
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn unresolved<T>(&self, message: &str) -> Result<T> {
        Err(Error::Unresolved(format!(
            "{message} at content byte {}",
            self.position
        )))
    }
}

fn is_number(token: &[u8]) -> bool {
    let token = token
        .strip_prefix(b"+")
        .or_else(|| token.strip_prefix(b"-"))
        .unwrap_or(token);
    let mut dot_seen = false;
    let mut digit_seen = false;
    for &byte in token {
        match byte {
            b'0'..=b'9' => digit_seen = true,
            b'.' if !dot_seen => dot_seen = true,
            _ => return false,
        }
    }
    digit_seen
}

fn looks_numeric(token: &[u8]) -> bool {
    token
        .first()
        .is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.'))
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, 0x00 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ContentLimits {
        ContentLimits {
            max_operators: 20,
            max_operand_stack: 10,
            max_operand_nodes: 100,
            max_nesting_depth: 5,
            max_string_bytes: 32,
        }
    }

    #[test]
    fn parses_operands_comments_and_escapes() -> Result<()> {
        let input =
            b"% heading\n12 -.5 /A#20Name (a\\(b\\)\\n\\101) <48656c6c6f2> [true null /N] Tj";
        let operations = parse_operations(input, limits())?;

        assert_eq!(
            operations,
            vec![Operation {
                operands: vec![
                    Operand::Number(12.0),
                    Operand::Number(-0.5),
                    Operand::Name(b"A Name".to_vec()),
                    Operand::String(b"a(b)\nA".to_vec()),
                    Operand::String(vec![b'H', b'e', b'l', b'l', b'o', 0x20]),
                    Operand::Array(vec![
                        Operand::Boolean(true),
                        Operand::Null,
                        Operand::Name(b"N".to_vec()),
                    ]),
                ],
                operator: b"Tj".to_vec(),
                index: 0,
            }]
        );
        Ok(())
    }

    #[test]
    fn balances_literal_parentheses_and_continued_lines() -> Result<()> {
        let operations = parse_operations(b"(outer(inner)\\\r\nline) Tj", limits())?;
        assert_eq!(
            operations[0].operands,
            vec![Operand::String(b"outer(inner)line".to_vec())]
        );
        Ok(())
    }

    #[test]
    fn truncates_large_octal_escapes_to_one_byte() -> Result<()> {
        let operations = parse_operations(b"(\\777) Tj", limits())?;
        assert_eq!(operations[0].operands, vec![Operand::String(vec![0xff])]);
        Ok(())
    }

    #[test]
    fn skips_inline_images_using_token_boundaries() -> Result<()> {
        let input = b"q BI /W 1 /H 1 ID abcEIxyz \x00 EI Q 7 Tc";
        let operations = parse_operations(input, limits())?;
        assert_eq!(
            operations
                .iter()
                .map(|operation| (operation.operator.as_slice(), operation.index))
                .collect::<Vec<_>>(),
            vec![
                (b"q".as_slice(), 0),
                (b"BI".as_slice(), 1),
                (b"Q".as_slice(), 2),
                (b"Tc".as_slice(), 3)
            ]
        );
        assert_eq!(operations[3].operands, vec![Operand::Number(7.0)]);
        Ok(())
    }

    #[test]
    fn carries_completed_operands_between_fragments() -> Result<()> {
        let mut parser = ContentParser::new(limits());

        assert!(
            parser
                .parse_fragment(b"12 /Move")
                .is_ok_and(|ops| ops.is_empty())
        );
        let second = parser.parse_fragment(b"3 m q")?;
        let third = parser.parse_fragment(b"Q")?;
        parser.finish()?;

        assert_eq!(
            second,
            vec![
                Operation {
                    operands: vec![
                        Operand::Number(12.0),
                        Operand::Name(b"Move".to_vec()),
                        Operand::Number(3.0),
                    ],
                    operator: b"m".to_vec(),
                    index: 0,
                },
                Operation {
                    operands: Vec::new(),
                    operator: b"q".to_vec(),
                    index: 1,
                },
            ]
        );
        assert_eq!(third[0].index, 0);
        assert_eq!(third[0].operator, b"Q");
        Ok(())
    }

    #[test]
    fn carries_a_dictionary_value_across_fragments() -> Result<()> {
        let mut parser = ContentParser::new(limits());

        let first = parser.parse_fragment(b"q /Span << /ActualText ")?;
        let second = parser.parse_fragment(b"<FEFF0031>>> BDC Q")?;
        parser.finish()?;

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].operator, b"q");
        assert_eq!(
            second[0].operands,
            vec![
                Operand::Name(b"Span".to_vec()),
                Operand::Dictionary(vec![(
                    b"ActualText".to_vec(),
                    Operand::String(vec![0xfe, 0xff, 0x00, 0x31]),
                )]),
            ]
        );
        assert_eq!(second[0].operator, b"BDC");
        assert_eq!(second[1].operator, b"Q");
        Ok(())
    }

    #[test]
    fn carries_prior_operands_before_a_cross_fragment_dictionary() -> Result<()> {
        let mut constrained = limits();
        constrained.max_operand_nodes = 3;
        let mut parser = ContentParser::new(constrained);

        assert!(parser.parse_fragment(b"/Span")?.is_empty());
        assert!(parser.parse_fragment(b"<< /ActualText ")?.is_empty());
        let operations = parser.parse_fragment(b"<0031>>> BDC")?;
        parser.finish()?;

        assert_eq!(
            operations[0].operands,
            vec![
                Operand::Name(b"Span".to_vec()),
                Operand::Dictionary(vec![(
                    b"ActualText".to_vec(),
                    Operand::String(vec![0x00, 0x31]),
                )]),
            ]
        );
        assert_eq!(operations[0].operator, b"BDC");
        Ok(())
    }

    #[test]
    fn separates_a_dictionary_name_key_from_its_numeric_value() -> Result<()> {
        let mut parser = ContentParser::new(limits());

        assert!(parser.parse_fragment(b"/Span << /MCID")?.is_empty());
        let operations = parser.parse_fragment(b"0 >> BDC")?;
        parser.finish()?;

        assert_eq!(
            operations[0].operands,
            vec![
                Operand::Name(b"Span".to_vec()),
                Operand::Dictionary(vec![(b"MCID".to_vec(), Operand::Number(0.0))]),
            ]
        );
        assert_eq!(operations[0].operator, b"BDC");
        Ok(())
    }

    #[test]
    fn terminates_a_comment_at_the_content_stream_boundary() -> Result<()> {
        let mut parser = ContentParser::new(limits());

        assert!(
            parser
                .parse_fragment(b"/Span << /MCID % trailing comment")?
                .is_empty()
        );
        let operations = parser.parse_fragment(b"0 >> BDC")?;
        parser.finish()?;

        assert_eq!(
            operations[0].operands,
            vec![
                Operand::Name(b"Span".to_vec()),
                Operand::Dictionary(vec![(b"MCID".to_vec(), Operand::Number(0.0))]),
            ]
        );
        assert_eq!(operations[0].operator, b"BDC");
        Ok(())
    }

    #[test]
    fn counts_a_cross_fragment_dictionary_operand_once() -> Result<()> {
        let mut constrained = limits();
        constrained.max_operand_nodes = 3;
        let mut parser = ContentParser::new(constrained);

        assert!(parser.parse_fragment(b"/Span << /ActualText ")?.is_empty());
        assert_eq!(parser.parse_fragment(b"<0031>>> BDC")?.len(), 1);
        parser.finish()?;
        Ok(())
    }

    #[test]
    fn does_not_hide_a_low_operand_limit_at_a_fragment_boundary() {
        let mut constrained = limits();
        constrained.max_operand_nodes = 2;
        let mut parser = ContentParser::new(constrained);

        assert!(matches!(
            parser.parse_fragment(b"/Span << /ActualText "),
            Err(Error::LimitExceeded {
                resource: "content operand nodes",
                limit: 2,
            })
        ));
    }

    #[test]
    fn rejects_a_dictionary_value_still_missing_after_the_boundary() -> Result<()> {
        let mut parser = ContentParser::new(limits());

        assert!(
            parser
                .parse_fragment(b"/Span << /ActualText ")
                .is_ok_and(|operations| operations.is_empty())
        );
        assert!(matches!(
            parser.parse_fragment(b">> BDC"),
            Err(Error::Unresolved(message))
                if message.contains("dictionary key is missing a value")
        ));
        Ok(())
    }

    #[test]
    fn rejects_a_dictionary_value_incomplete_in_two_fragments() -> Result<()> {
        let mut parser = ContentParser::new(limits());
        assert!(parser.parse_fragment(b"/Span << /ActualText ")?.is_empty());

        assert!(matches!(
            parser.parse_fragment(b" "),
            Err(Error::Unresolved(message))
                if message.contains("remains incomplete in the next content stream")
        ));
        Ok(())
    }

    #[test]
    fn rejects_the_first_of_many_empty_continuation_fragments() -> Result<()> {
        let mut parser = ContentParser::new(limits());
        assert!(parser.parse_fragment(b"/Span << /ActualText ")?.is_empty());

        let attempted = std::iter::repeat_n(b"".as_slice(), 1_000)
            .position(|fragment| parser.parse_fragment(fragment).is_err());
        assert_eq!(attempted, Some(0));
        Ok(())
    }

    #[test]
    fn enforces_pending_operand_limit_between_fragments() -> Result<()> {
        let mut constrained = limits();
        constrained.max_operand_stack = 1;
        let mut parser = ContentParser::new(constrained);

        assert!(parser.parse_fragment(b"1")?.is_empty());
        assert!(matches!(
            parser.parse_fragment(b"2 m"),
            Err(Error::LimitExceeded { .. })
        ));
        Ok(())
    }

    #[test]
    fn enforces_aggregate_operator_limit_across_fragments() -> Result<()> {
        let mut constrained = limits();
        constrained.max_operators = 2;
        let mut parser = ContentParser::new(constrained);

        assert_eq!(parser.parse_fragment(b"q")?[0].index, 0);
        assert_eq!(parser.parse_fragment(b"BI /W 1 ID data EI")?[0].index, 0);
        assert!(matches!(
            parser.parse_fragment(b"Q"),
            Err(Error::LimitExceeded { .. })
        ));
        Ok(())
    }

    #[test]
    fn enforces_aggregate_operand_node_limit_across_nested_arrays_and_fragments() -> Result<()> {
        let mut constrained = limits();
        constrained.max_operand_nodes = 5;
        assert!(matches!(
            parse_operations(b"[[1 2] [3 4]] Tj", constrained),
            Err(Error::LimitExceeded {
                resource: "content operand nodes",
                limit: 5,
            })
        ));

        constrained.max_operand_nodes = 2;
        let mut parser = ContentParser::new(constrained);
        assert_eq!(parser.parse_fragment(b"1 m")?.len(), 1);
        assert_eq!(parser.parse_fragment(b"2 m")?.len(), 1);
        assert!(matches!(
            parser.parse_fragment(b"3 m"),
            Err(Error::LimitExceeded {
                resource: "content operand nodes",
                limit: 2,
            })
        ));
        Ok(())
    }

    #[test]
    fn shares_the_operand_node_budget_across_parsers() -> Result<()> {
        let mut constrained = limits();
        constrained.max_operand_nodes = 2;
        let operator_budget = OperatorBudget::new(constrained.max_operators);
        let budget = OperandBudget::new(constrained.max_operand_nodes);
        let mut first =
            ContentParser::with_budgets(constrained, operator_budget.clone(), budget.clone());
        let mut second = ContentParser::with_budgets(constrained, operator_budget, budget);

        assert_eq!(first.parse_fragment(b"1 m")?.len(), 1);
        assert_eq!(second.parse_fragment(b"2 m")?.len(), 1);
        assert!(matches!(
            first.parse_fragment(b"3 m"),
            Err(Error::LimitExceeded {
                resource: "content operand nodes",
                limit: 2,
            })
        ));
        Ok(())
    }

    #[test]
    fn shares_the_operator_budget_across_parsers() -> Result<()> {
        let mut constrained = limits();
        constrained.max_operators = 2;
        let operator_budget = OperatorBudget::new(constrained.max_operators);
        let operand_budget = OperandBudget::new(constrained.max_operand_nodes);
        let mut first = ContentParser::with_budgets(
            constrained,
            operator_budget.clone(),
            operand_budget.clone(),
        );
        let mut second = ContentParser::with_budgets(constrained, operator_budget, operand_budget);

        assert_eq!(first.parse_fragment(b"q")?.len(), 1);
        assert_eq!(second.parse_fragment(b"Q")?.len(), 1);
        assert!(matches!(
            first.parse_fragment(b"BT"),
            Err(Error::LimitExceeded {
                resource: "content operators",
                limit: 2,
            })
        ));
        Ok(())
    }

    #[test]
    fn does_not_carry_incomplete_composite_tokens() {
        let mut parser = ContentParser::new(limits());
        assert!(matches!(
            parser.parse_fragment(b"(unfinished"),
            Err(Error::Unresolved(_))
        ));
    }

    #[test]
    fn rejects_an_unfinished_cross_fragment_dictionary_at_sequence_end() -> Result<()> {
        let mut parser = ContentParser::new(limits());
        assert!(parser.parse_fragment(b"/Span << /ActualText ")?.is_empty());

        assert!(matches!(
            parser.finish(),
            Err(Error::Unresolved(message))
                if message.contains("incomplete dictionary value")
        ));
        Ok(())
    }

    #[test]
    fn parses_tagged_pdf_property_dictionary() -> Result<()> {
        let operations = parse_operations(b"/P << /MCID 0 >> BDC", limits())?;

        assert_eq!(
            operations[0].operands,
            vec![
                Operand::Name(b"P".to_vec()),
                Operand::Dictionary(vec![(b"MCID".to_vec(), Operand::Number(0.0))]),
            ]
        );
        assert_eq!(operations[0].operator, b"BDC");
        Ok(())
    }

    #[test]
    fn rejects_malformed_or_over_limit_dictionaries() {
        assert!(matches!(
            parse_operations(b"<< 1 2 >> BDC", limits()),
            Err(Error::Unresolved(_))
        ));
        assert!(matches!(
            parse_operations(b"<< /MCID >> BDC", limits()),
            Err(Error::Unresolved(_))
        ));

        let mut constrained = limits();
        constrained.max_operand_stack = 1;
        assert!(matches!(
            parse_operations(b"<< /A 1 /B 2 >> BDC", constrained),
            Err(Error::LimitExceeded { .. })
        ));

        constrained = limits();
        constrained.max_nesting_depth = 1;
        assert!(matches!(
            parse_operations(b"<< /A << /B 1 >> >> BDC", constrained),
            Err(Error::LimitExceeded { .. })
        ));
    }

    #[test]
    fn rejects_malformed_syntax() {
        for input in [b"(open Tj".as_slice(), b"<0g> Tj", b"[1 2 Tj", b"/Bad#x Tj"] {
            assert!(matches!(
                parse_operations(input, limits()),
                Err(Error::Unresolved(_))
            ));
        }
    }

    #[test]
    fn enforces_each_configured_limit() {
        let mut constrained = limits();
        constrained.max_operators = 1;
        assert!(matches!(
            parse_operations(b"q Q", constrained),
            Err(Error::LimitExceeded { .. })
        ));

        constrained = limits();
        constrained.max_operand_stack = 1;
        assert!(matches!(
            parse_operations(b"1 2 m", constrained),
            Err(Error::LimitExceeded { .. })
        ));

        constrained = limits();
        constrained.max_nesting_depth = 1;
        assert!(matches!(
            parse_operations(b"[[1]] Tj", constrained),
            Err(Error::LimitExceeded { .. })
        ));

        constrained = limits();
        constrained.max_string_bytes = 2;
        assert!(matches!(
            parse_operations(b"(abc) Tj", constrained),
            Err(Error::LimitExceeded { .. })
        ));
    }
}
