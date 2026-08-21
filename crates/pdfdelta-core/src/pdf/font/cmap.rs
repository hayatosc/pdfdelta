use std::collections::BTreeMap;

use crate::{Error, Result};

const MAX_PDF_CODE_BYTES: usize = 4;

#[derive(Clone, Copy, Debug)]
pub(crate) struct CMapLimits {
    pub(crate) max_entries: usize,
    pub(crate) max_code_bytes: usize,
    pub(crate) max_output_scalars: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UnicodeMapping {
    Mapped(String),
    Unmapped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DecodedCode {
    pub(crate) source: Vec<u8>,
    pub(crate) mapping: UnicodeMapping,
}

#[derive(Clone, Debug)]
pub(crate) struct ToUnicodeCMap {
    codespaces: [Vec<CodeSpace>; MAX_PDF_CODE_BYTES],
    mappings: BTreeMap<Vec<u8>, String>,
    total_entries: usize,
}

#[derive(Clone, Debug)]
struct CodeSpace {
    low: Vec<u8>,
    high: Vec<u8>,
}

#[cfg(test)]
pub(crate) fn parse_to_unicode(input: &[u8], limits: CMapLimits) -> Result<ToUnicodeCMap> {
    parse_to_unicode_with_width(input, limits, None)
}

pub(crate) fn parse_to_unicode_for_width(
    input: &[u8],
    limits: CMapLimits,
    source_width: usize,
) -> Result<ToUnicodeCMap> {
    if source_width == 0 || source_width > MAX_PDF_CODE_BYTES {
        return unresolved("expected CMap source width must be one to four bytes");
    }
    if source_width > limits.max_code_bytes {
        return Err(Error::LimitExceeded {
            resource: "CMap source code bytes",
            limit: limits.max_code_bytes,
        });
    }
    parse_to_unicode_with_width(input, limits, Some(source_width))
}

fn parse_to_unicode_with_width(
    input: &[u8],
    limits: CMapLimits,
    source_width: Option<usize>,
) -> Result<ToUnicodeCMap> {
    let parser = Parser {
        lexer: Lexer::new(input),
        limits,
        source_width,
        cmap: ToUnicodeCMap {
            codespaces: std::array::from_fn(|_| Vec::new()),
            mappings: BTreeMap::new(),
            total_entries: 0,
        },
        entries: 0,
        output_scalars: 0,
    };
    parser.parse()
}

impl ToUnicodeCMap {
    pub(crate) fn entry_count(&self) -> usize {
        self.total_entries
    }

    pub(crate) fn decode(
        &self,
        input: &[u8],
        max_mapped_text_bytes: usize,
    ) -> Result<Vec<DecodedCode>> {
        let mut decoded = Vec::new();
        let mut cursor = 0;
        let mut mapped_text_bytes = 0usize;

        while cursor < input.len() {
            let remaining = &input[cursor..];
            let width = (1..=remaining.len().min(MAX_PDF_CODE_BYTES))
                .rev()
                .find(|&width| self.contains_code(&remaining[..width]));

            let Some(width) = width else {
                if self.is_truncated_prefix(remaining) {
                    return unresolved("truncated source code");
                }
                return unresolved("source code is outside the CMap codespace");
            };

            let source = remaining[..width].to_vec();
            let mapping = match self.mappings.get(&source) {
                Some(text) => {
                    mapped_text_bytes =
                        mapped_text_bytes
                            .checked_add(text.len())
                            .ok_or(Error::LimitExceeded {
                                resource: "decoded Unicode text bytes",
                                limit: max_mapped_text_bytes,
                            })?;
                    if mapped_text_bytes > max_mapped_text_bytes {
                        return Err(Error::LimitExceeded {
                            resource: "decoded Unicode text bytes",
                            limit: max_mapped_text_bytes,
                        });
                    }
                    UnicodeMapping::Mapped(text.clone())
                }
                None => UnicodeMapping::Unmapped,
            };
            decoded.push(DecodedCode { source, mapping });
            cursor += width;
        }

        Ok(decoded)
    }

    fn contains_code(&self, code: &[u8]) -> bool {
        code.len()
            .checked_sub(1)
            .and_then(|index| self.codespaces.get(index))
            .is_some_and(|spaces| interval_contains(spaces, code))
    }

    fn is_truncated_prefix(&self, code: &[u8]) -> bool {
        self.codespaces
            .iter()
            .skip(code.len())
            .any(|spaces| interval_contains_prefix(spaces, code))
    }
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    limits: CMapLimits,
    source_width: Option<usize>,
    cmap: ToUnicodeCMap,
    entries: usize,
    output_scalars: usize,
}

#[derive(Clone, Copy)]
enum WrapperState {
    Bare,
    Abbreviated,
    Prolog {
        outer_depth: usize,
    },
    CMap {
        outer_depth: usize,
        inner_depth: usize,
    },
    Epilog {
        outer_depth: usize,
    },
}

impl Parser<'_> {
    fn parse(mut self) -> Result<ToUnicodeCMap> {
        let mut pending_count = None;
        let mut wrapper = WrapperState::Bare;

        while let Some(token) = self.lexer.next_token()? {
            match token {
                Token::Word(b"begin") => {
                    wrapper = match wrapper {
                        WrapperState::Bare => WrapperState::Prolog { outer_depth: 1 },
                        WrapperState::Prolog { outer_depth } => WrapperState::Prolog {
                            outer_depth: increment_depth(outer_depth)?,
                        },
                        WrapperState::CMap {
                            outer_depth,
                            inner_depth,
                        } => WrapperState::CMap {
                            outer_depth,
                            inner_depth: increment_depth(inner_depth)?,
                        },
                        WrapperState::Abbreviated | WrapperState::Epilog { .. } => {
                            return unresolved("CMap wrapper has an invalid begin transition");
                        }
                    };
                    pending_count = None;
                }
                Token::Word(b"begincmap") => {
                    wrapper = match wrapper {
                        WrapperState::Bare => WrapperState::CMap {
                            outer_depth: 0,
                            inner_depth: 0,
                        },
                        WrapperState::Prolog { outer_depth } => WrapperState::CMap {
                            outer_depth,
                            inner_depth: 0,
                        },
                        _ => return unresolved("CMap wrapper has an invalid begincmap transition"),
                    };
                    pending_count = None;
                }
                Token::Word(b"endcmap") => {
                    let WrapperState::CMap {
                        outer_depth,
                        inner_depth: 0,
                    } = wrapper
                    else {
                        return unresolved("unexpected CMap block terminator");
                    };
                    wrapper = WrapperState::Epilog { outer_depth };
                    pending_count = None;
                }
                Token::Word(b"end") => {
                    wrapper = match wrapper {
                        WrapperState::CMap {
                            outer_depth,
                            inner_depth,
                        } if inner_depth > 0 => WrapperState::CMap {
                            outer_depth,
                            inner_depth: inner_depth - 1,
                        },
                        WrapperState::Epilog { outer_depth } if outer_depth > 0 => {
                            WrapperState::Epilog {
                                outer_depth: outer_depth - 1,
                            }
                        }
                        _ => return unresolved("unexpected CMap block terminator"),
                    };
                    pending_count = None;
                }
                Token::Word(b"begincodespacerange") => {
                    wrapper = mapping_wrapper_state(wrapper)?;
                    let count = required_count(pending_count, "begincodespacerange")?;
                    self.parse_codespaces(count)?;
                    pending_count = None;
                }
                Token::Word(b"beginbfchar") => {
                    wrapper = mapping_wrapper_state(wrapper)?;
                    let count = required_count(pending_count, "beginbfchar")?;
                    self.parse_bfchars(count)?;
                    pending_count = None;
                }
                Token::Word(b"beginbfrange") => {
                    wrapper = mapping_wrapper_state(wrapper)?;
                    let count = required_count(pending_count, "beginbfrange")?;
                    self.parse_bfranges(count)?;
                    pending_count = None;
                }
                Token::Word(b"usecmap" | b"begincidchar" | b"begincidrange") => {
                    return Err(Error::Unsupported(
                        "CMap inheritance and CID mappings are not supported".into(),
                    ));
                }
                Token::Word(word) if word.starts_with(b"end") => {
                    return unresolved("unexpected CMap block terminator");
                }
                Token::Word(word) => pending_count = parse_decimal(word),
                _ => pending_count = None,
            }
        }

        if !matches!(
            wrapper,
            WrapperState::Abbreviated | WrapperState::Epilog { outer_depth: 0 }
        ) {
            return unresolved("CMap wrapper has an unclosed scope");
        }

        if self.cmap.codespaces.iter().all(Vec::is_empty) {
            return unresolved("ToUnicode CMap has no codespace ranges");
        }
        validate_codespaces(&mut self.cmap.codespaces)?;
        if self
            .cmap
            .mappings
            .keys()
            .any(|source| !self.cmap.contains_code(source))
        {
            return unresolved("mapping source is outside the CMap codespace");
        }

        self.cmap.total_entries = self.entries;
        Ok(self.cmap)
    }

    fn parse_codespaces(&mut self, count: usize) -> Result<()> {
        self.reserve_entries(count)?;
        for _ in 0..count {
            let low = self.source_hex()?;
            let high = self.source_hex()?;
            let (low, high) = match self.source_width {
                Some(width) => reconcile_codespace(low, high, width)?,
                None if low.len() == high.len() && low <= high => (low, high),
                None => return unresolved("invalid codespace range"),
            };
            let candidate = CodeSpace { low, high };
            self.cmap.codespaces[candidate.low.len() - 1].push(candidate);
        }
        self.expect_word(b"endcodespacerange")
    }

    fn parse_bfchars(&mut self, count: usize) -> Result<()> {
        self.reserve_entries(count)?;
        for _ in 0..count {
            let source = self.source_hex()?;
            let source = self.reconcile_source(source)?;
            let destination = self.destination()?;
            self.insert_mapping(source, destination)?;
        }
        self.expect_word(b"endbfchar")
    }

    fn parse_bfranges(&mut self, declarations: usize) -> Result<()> {
        for _ in 0..declarations {
            let low = self.source_hex()?;
            let high = self.source_hex()?;
            let (low, high) = match self.source_width {
                Some(width) => (
                    reconcile_source(low, width)?,
                    reconcile_source(high, width)?,
                ),
                None if low.len() == high.len() => (low, high),
                None => return unresolved("invalid bf range"),
            };
            if low > high {
                return unresolved("invalid bf range");
            }
            let count = range_len(&low, &high)?;
            self.reserve_entries(count)?;

            match self.lexer.next_token()? {
                Some(Token::Hex(raw)) => self.parse_sequential_range(low, count, raw)?,
                Some(Token::ArrayStart) => self.parse_array_range(low, count)?,
                _ => return unresolved("bf range has an invalid destination"),
            }
        }
        self.expect_word(b"endbfrange")
    }

    fn parse_sequential_range(
        &mut self,
        mut source: Vec<u8>,
        count: usize,
        raw: &[u8],
    ) -> Result<()> {
        let mut destination_bytes = self.destination_bytes(raw)?;
        for index in 0..count {
            let destination = self.decode_destination(&destination_bytes)?;
            self.insert_mapping(source.clone(), destination)?;
            if index + 1 < count
                && (!increment_be(&mut source) || !increment_be(&mut destination_bytes))
            {
                return unresolved("bf range increment overflow");
            }
        }
        Ok(())
    }

    fn parse_array_range(&mut self, mut source: Vec<u8>, count: usize) -> Result<()> {
        for index in 0..count {
            let destination = self.destination()?;
            self.insert_mapping(source.clone(), destination)?;
            if index + 1 < count && !increment_be(&mut source) {
                return unresolved("bf range source overflow");
            }
        }
        match self.lexer.next_token()? {
            Some(Token::ArrayEnd) => Ok(()),
            _ => unresolved("bf range destination array has the wrong length"),
        }
    }

    fn source_hex(&mut self) -> Result<Vec<u8>> {
        let Some(Token::Hex(raw)) = self.lexer.next_token()? else {
            return unresolved("expected hexadecimal source code");
        };
        let byte_len = hex_byte_len(raw)?;
        if byte_len == 0 || byte_len > MAX_PDF_CODE_BYTES {
            return unresolved("source codes must contain one to four bytes");
        }
        if byte_len > self.limits.max_code_bytes {
            return Err(Error::LimitExceeded {
                resource: "CMap source code bytes",
                limit: self.limits.max_code_bytes,
            });
        }
        decode_hex(raw, byte_len)
    }

    fn reconcile_source(&self, source: Vec<u8>) -> Result<Vec<u8>> {
        match self.source_width {
            Some(width) => reconcile_source(source, width),
            None => Ok(source),
        }
    }

    fn destination(&mut self) -> Result<String> {
        let Some(Token::Hex(raw)) = self.lexer.next_token()? else {
            return unresolved("expected hexadecimal UTF-16BE destination");
        };
        let bytes = self.destination_bytes(raw)?;
        self.decode_destination(&bytes)
    }

    fn destination_bytes(&self, raw: &[u8]) -> Result<Vec<u8>> {
        let byte_len = hex_byte_len(raw)?;
        let max_bytes = self.limits.max_output_scalars.saturating_mul(4);
        if byte_len > max_bytes {
            return Err(output_limit_error(self.limits.max_output_scalars));
        }
        decode_hex(raw, byte_len)
    }

    fn decode_destination(&self, bytes: &[u8]) -> Result<String> {
        decode_utf16(
            bytes,
            self.limits
                .max_output_scalars
                .saturating_sub(self.output_scalars),
            self.limits.max_output_scalars,
        )
    }

    fn insert_mapping(&mut self, source: Vec<u8>, destination: String) -> Result<()> {
        let scalars = destination.chars().count();
        let next = self
            .output_scalars
            .checked_add(scalars)
            .ok_or(Error::LimitExceeded {
                resource: "CMap output Unicode scalars",
                limit: self.limits.max_output_scalars,
            })?;
        if next > self.limits.max_output_scalars {
            return Err(Error::LimitExceeded {
                resource: "CMap output Unicode scalars",
                limit: self.limits.max_output_scalars,
            });
        }
        if self.cmap.mappings.contains_key(&source) {
            return unresolved("duplicate CMap source mapping");
        }
        self.output_scalars = next;
        self.cmap.mappings.insert(source, destination);
        Ok(())
    }

    fn reserve_entries(&mut self, additional: usize) -> Result<()> {
        let next = self
            .entries
            .checked_add(additional)
            .ok_or(Error::LimitExceeded {
                resource: "CMap entries",
                limit: self.limits.max_entries,
            })?;
        if next > self.limits.max_entries {
            return Err(Error::LimitExceeded {
                resource: "CMap entries",
                limit: self.limits.max_entries,
            });
        }
        self.entries = next;
        Ok(())
    }

    fn expect_word(&mut self, expected: &[u8]) -> Result<()> {
        match self.lexer.next_token()? {
            Some(Token::Word(actual)) if actual == expected => Ok(()),
            _ => unresolved("CMap block has a missing or misplaced terminator"),
        }
    }
}

fn increment_depth(depth: usize) -> Result<usize> {
    depth
        .checked_add(1)
        .ok_or_else(|| Error::Unresolved("CMap wrapper nesting is too deep".into()))
}

fn mapping_wrapper_state(wrapper: WrapperState) -> Result<WrapperState> {
    match wrapper {
        WrapperState::Bare | WrapperState::Abbreviated => Ok(WrapperState::Abbreviated),
        WrapperState::CMap { .. } => Ok(wrapper),
        WrapperState::Prolog { .. } | WrapperState::Epilog { .. } => {
            unresolved("CMap mapping block appears outside begincmap")
        }
    }
}

fn required_count(count: Option<usize>, operator: &str) -> Result<usize> {
    count.ok_or_else(|| Error::Unresolved(format!("{operator} has no valid entry count")))
}

fn parse_decimal(word: &[u8]) -> Option<usize> {
    if word.is_empty() || !word.iter().all(u8::is_ascii_digit) {
        return None;
    }
    word.iter().try_fold(0usize, |value, digit| {
        value
            .checked_mul(10)?
            .checked_add(usize::from(digit - b'0'))
    })
}

fn validate_codespaces(codespaces: &mut [Vec<CodeSpace>; MAX_PDF_CODE_BYTES]) -> Result<()> {
    for spaces in codespaces.iter_mut() {
        spaces.sort_by(|left, right| left.low.cmp(&right.low));
        if spaces.windows(2).any(|pair| pair[1].low <= pair[0].high) {
            return unresolved("overlapping or ambiguous CMap codespaces");
        }
    }

    for short_width in 1..MAX_PDF_CODE_BYTES {
        for long_width in short_width + 1..=MAX_PDF_CODE_BYTES {
            if prefix_ranges_overlap(
                &codespaces[short_width - 1],
                &codespaces[long_width - 1],
                short_width,
            ) {
                return unresolved("overlapping or ambiguous CMap codespaces");
            }
        }
    }
    Ok(())
}

fn prefix_ranges_overlap(short: &[CodeSpace], long: &[CodeSpace], prefix_len: usize) -> bool {
    let (mut short_index, mut long_index) = (0, 0);
    while let (Some(short), Some(long)) = (short.get(short_index), long.get(long_index)) {
        let long_low = &long.low[..prefix_len];
        let long_high = &long.high[..prefix_len];
        if short.high.as_slice() < long_low {
            short_index += 1;
        } else if long_high < short.low.as_slice() {
            long_index += 1;
        } else {
            return true;
        }
    }
    false
}

fn interval_contains(spaces: &[CodeSpace], code: &[u8]) -> bool {
    spaces
        .binary_search_by(|space| {
            if space.high.as_slice() < code {
                std::cmp::Ordering::Less
            } else if space.low.as_slice() > code {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn interval_contains_prefix(spaces: &[CodeSpace], prefix: &[u8]) -> bool {
    spaces
        .binary_search_by(|space| {
            if &space.high[..prefix.len()] < prefix {
                std::cmp::Ordering::Less
            } else if &space.low[..prefix.len()] > prefix {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn reconcile_source(mut source: Vec<u8>, width: usize) -> Result<Vec<u8>> {
    match source.len().cmp(&width) {
        std::cmp::Ordering::Less => {
            let mut reconciled = vec![0; width - source.len()];
            reconciled.append(&mut source);
            Ok(reconciled)
        }
        std::cmp::Ordering::Equal => Ok(source),
        std::cmp::Ordering::Greater => {
            let prefix_len = source.len() - width;
            if source[..prefix_len].iter().any(|byte| *byte != 0) {
                return unresolved("CMap source width requires non-zero truncation");
            }
            Ok(source.split_off(prefix_len))
        }
    }
}

fn reconcile_codespace(low: Vec<u8>, high: Vec<u8>, width: usize) -> Result<(Vec<u8>, Vec<u8>)> {
    let low = code_value(&low);
    let high = code_value(&high);
    if low > high {
        return unresolved("invalid codespace range");
    }

    let max = (1u64 << (width * 8)) - 1;
    if low > max {
        return unresolved("CMap codespace does not contain the decoder source width");
    }
    Ok((
        fixed_width_code(low, width),
        fixed_width_code(high.min(max), width),
    ))
}

fn fixed_width_code(value: u64, width: usize) -> Vec<u8> {
    value.to_be_bytes()[size_of::<u64>() - width..].to_vec()
}

fn range_len(low: &[u8], high: &[u8]) -> Result<usize> {
    let low = code_value(low);
    let high = code_value(high);
    let length = high
        .checked_sub(low)
        .and_then(|delta| delta.checked_add(1))
        .ok_or_else(|| Error::Unresolved("invalid or overflowing bf range".into()))?;
    usize::try_from(length).map_err(|_| Error::Unresolved("bf range is too large".into()))
}

fn code_value(code: &[u8]) -> u64 {
    code.iter()
        .fold(0, |value, byte| (value << 8) | u64::from(*byte))
}

fn hex_byte_len(raw: &[u8]) -> Result<usize> {
    let mut digits = 0usize;
    for byte in raw.iter().filter(|byte| !is_pdf_whitespace(**byte)) {
        hex_digit(*byte)?;
        digits = digits
            .checked_add(1)
            .ok_or_else(|| Error::Unresolved("hexadecimal string is too large".into()))?;
    }
    if !digits.is_multiple_of(2) {
        return unresolved("hexadecimal string has an odd number of digits");
    }
    Ok(digits / 2)
}

fn decode_hex(raw: &[u8], byte_len: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(byte_len);
    let mut digits = raw.iter().copied().filter(|byte| !is_pdf_whitespace(*byte));
    while let Some(high) = digits.next() {
        let low = digits.next().ok_or_else(|| {
            Error::Unresolved("hexadecimal string has an odd number of digits".into())
        })?;
        bytes.push((hex_digit(high)? << 4) | hex_digit(low)?);
    }
    Ok(bytes)
}

fn hex_digit(digit: u8) -> Result<u8> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        b'A'..=b'F' => Ok(digit - b'A' + 10),
        _ => unresolved("hexadecimal string contains a non-hex digit"),
    }
}

fn decode_utf16(bytes: &[u8], scalar_limit: usize, configured_limit: usize) -> Result<String> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(2) {
        return unresolved("UTF-16BE destination must contain complete, non-empty code units");
    }
    let units = || {
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
    };
    let mut scalar_count = 0usize;
    for scalar in char::decode_utf16(units()) {
        scalar.map_err(|_| Error::Unresolved("invalid UTF-16BE destination".into()))?;
        scalar_count = scalar_count
            .checked_add(1)
            .ok_or_else(|| output_limit_error(configured_limit))?;
        if scalar_count > scalar_limit {
            return Err(output_limit_error(configured_limit));
        }
    }
    let mut text = String::new();
    for scalar in char::decode_utf16(units()) {
        text.push(scalar.map_err(|_| Error::Unresolved("invalid UTF-16BE destination".into()))?);
    }
    Ok(text)
}

fn output_limit_error(limit: usize) -> Error {
    Error::LimitExceeded {
        resource: "CMap output Unicode scalars",
        limit,
    }
}

fn increment_be(bytes: &mut [u8]) -> bool {
    for byte in bytes.iter_mut().rev() {
        let (next, overflow) = byte.overflowing_add(1);
        *byte = next;
        if !overflow {
            return true;
        }
    }
    false
}

fn unresolved<T>(message: &str) -> Result<T> {
    Err(Error::Unresolved(message.into()))
}

#[derive(Clone, Copy, Debug)]
enum Token<'a> {
    Word(&'a [u8]),
    Hex(&'a [u8]),
    ArrayStart,
    ArrayEnd,
    Other,
}

struct Lexer<'a> {
    input: &'a [u8],
    cursor: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, cursor: 0 }
    }

    fn next_token(&mut self) -> Result<Option<Token<'a>>> {
        self.skip_ignored();
        let Some(&first) = self.input.get(self.cursor) else {
            return Ok(None);
        };
        match first {
            b'[' => {
                self.cursor += 1;
                Ok(Some(Token::ArrayStart))
            }
            b']' => {
                self.cursor += 1;
                Ok(Some(Token::ArrayEnd))
            }
            b'<' if self.input.get(self.cursor + 1) != Some(&b'<') => {
                let start = self.cursor + 1;
                let Some(end_offset) = self.input[start..].iter().position(|byte| *byte == b'>')
                else {
                    return unresolved("unterminated hexadecimal string");
                };
                let end = start + end_offset;
                self.cursor = end + 1;
                Ok(Some(Token::Hex(&self.input[start..end])))
            }
            b'<' | b'>' => {
                self.cursor += usize::from(self.input.get(self.cursor + 1) == Some(&first)) + 1;
                Ok(Some(Token::Other))
            }
            _ => {
                let start = self.cursor;
                while self
                    .input
                    .get(self.cursor)
                    .is_some_and(|byte| !is_delimiter(*byte))
                {
                    self.cursor += 1;
                }
                Ok(Some(Token::Word(&self.input[start..self.cursor])))
            }
        }
    }

    fn skip_ignored(&mut self) {
        loop {
            while self
                .input
                .get(self.cursor)
                .is_some_and(|byte| is_pdf_whitespace(*byte))
            {
                self.cursor += 1;
            }
            if self.input.get(self.cursor) != Some(&b'%') {
                break;
            }
            while self
                .input
                .get(self.cursor)
                .is_some_and(|byte| !matches!(byte, b'\r' | b'\n'))
            {
                self.cursor += 1;
            }
        }
    }
}

fn is_delimiter(byte: u8) -> bool {
    is_pdf_whitespace(byte) || matches!(byte, b'[' | b']' | b'<' | b'>' | b'%')
}

fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0x00 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: CMapLimits = CMapLimits {
        max_entries: 32,
        max_code_bytes: 4,
        max_output_scalars: 64,
    };

    #[test]
    fn parses_char_and_both_range_forms() -> Result<()> {
        let cmap = parse_to_unicode(
            br#"
                2 begincodespacerange
                <00> <7F>
                <8100> <81FF>
                endcodespacerange
                1 beginbfchar
                <41> <0041>
                endbfchar
                1 beginbfrange
                <42> <44> <0042>
                endbfrange
                1 beginbfrange
                <8100> <8101> [<0066 0069> <0066 006C>]
                endbfrange
            "#,
            LIMITS,
        )?;

        assert_eq!(cmap.entry_count(), 8);
        assert_eq!(
            cmap.decode(&[0x41, 0x42, 0x44, 0x81, 0x00, 0x45], usize::MAX)?,
            vec![
                mapped(&[0x41], "A"),
                mapped(&[0x42], "B"),
                mapped(&[0x44], "D"),
                mapped(&[0x81, 0x00], "fi"),
                DecodedCode {
                    source: vec![0x45],
                    mapping: UnicodeMapping::Unmapped,
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn parses_standard_postscript_wrapper() -> Result<()> {
        let cmap = parse_to_unicode(
            br#"
                /CIDInit /ProcSet findresource begin
                12 dict begin
                begincmap
                1 begincodespacerange <00> <FF> endcodespacerange
                1 beginbfchar <41> <0041> endbfchar
                endcmap
                CMapName currentdict /CMap defineresource pop
                end
                end
            "#,
            LIMITS,
        )?;

        assert_eq!(cmap.decode(b"A", usize::MAX)?, vec![mapped(b"A", "A")]);
        Ok(())
    }

    #[test]
    fn parses_cid_system_info_dictionary_inside_cmap() -> Result<()> {
        let cmap = parse_to_unicode(
            br#"
                /CIDInit /ProcSet findresource begin
                12 dict begin
                begincmap
                /CIDSystemInfo 3 dict dup begin
                /Registry (Adobe) def
                /Ordering (UCS) def
                /Supplement 0 def
                end def
                1 begincodespacerange <00> <FF> endcodespacerange
                1 beginbfchar <41> <0041> endbfchar
                endcmap
                CMapName currentdict /CMap defineresource pop
                end
                end
            "#,
            LIMITS,
        )?;

        assert_eq!(cmap.decode(b"A", usize::MAX)?, vec![mapped(b"A", "A")]);
        Ok(())
    }

    #[test]
    fn rejects_stray_or_unbalanced_wrapper_terminators() {
        let valid_mapping =
            "1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <0041> endbfchar";
        for suffix in [
            "endcmap",
            "end",
            "endcodespacerange",
            "endbfchar",
            "endbfrange",
        ] {
            let input = format!("{valid_mapping} {suffix}");
            assert!(matches!(
                parse_to_unicode(input.as_bytes(), LIMITS),
                Err(Error::Unresolved(_))
            ));
        }

        for suffix in ["/Parent usecmap", "0 begincidchar", "0 begincidrange"] {
            let input = format!("{valid_mapping} {suffix}");
            assert!(matches!(
                parse_to_unicode(input.as_bytes(), LIMITS),
                Err(Error::Unsupported(_))
            ));
        }

        let unclosed_wrapper = format!("begin begincmap {valid_mapping} endcmap");
        assert!(matches!(
            parse_to_unicode(unclosed_wrapper.as_bytes(), LIMITS),
            Err(Error::Unresolved(_))
        ));

        let unclosed_inner_dictionary = format!("begincmap 3 dict begin {valid_mapping} endcmap");
        assert!(matches!(
            parse_to_unicode(unclosed_inner_dictionary.as_bytes(), LIMITS),
            Err(Error::Unresolved(_))
        ));

        let mapping_in_prolog = format!("begin {valid_mapping} begincmap endcmap end");
        assert!(matches!(
            parse_to_unicode(mapping_in_prolog.as_bytes(), LIMITS),
            Err(Error::Unresolved(_))
        ));

        let mapping_before_wrapper = format!("{valid_mapping} begincmap endcmap");
        assert!(matches!(
            parse_to_unicode(mapping_before_wrapper.as_bytes(), LIMITS),
            Err(Error::Unresolved(_))
        ));

        assert!(parse_to_unicode(valid_mapping.as_bytes(), LIMITS).is_ok());

        let mapping_after_endcmap =
            format!("begincmap {valid_mapping} endcmap 0 beginbfchar endbfchar");
        assert!(matches!(
            parse_to_unicode(mapping_after_endcmap.as_bytes(), LIMITS),
            Err(Error::Unresolved(_))
        ));
    }

    #[test]
    fn decodes_japanese_and_surrogate_pair_destinations() -> Result<()> {
        let cmap = parse_to_unicode(
            b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
              2 beginbfchar <1234> <65E5> <1235> <D83DDE00> endbfchar",
            LIMITS,
        )?;

        assert_eq!(
            cmap.decode(&[0x12, 0x34, 0x12, 0x35], usize::MAX)?,
            vec![mapped(&[0x12, 0x34], "日"), mapped(&[0x12, 0x35], "😀")]
        );
        Ok(())
    }

    #[test]
    fn reconciles_zero_extended_sources_to_decoder_width() -> Result<()> {
        let simple = parse_to_unicode_for_width(
            b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
              1 beginbfchar <20> <0020> endbfchar",
            LIMITS,
            1,
        )?;
        assert_eq!(simple.decode(b" ", usize::MAX)?, vec![mapped(b" ", " ")]);

        let range = parse_to_unicode_for_width(
            b"1 begincodespacerange <00> <FF> endcodespacerange \
              1 beginbfrange <ae> <00ff> <00AE> endbfrange",
            CMapLimits {
                max_entries: 128,
                max_output_scalars: 128,
                ..LIMITS
            },
            1,
        )?;
        assert_eq!(
            range.decode(&[0xae, 0xff], usize::MAX)?,
            vec![mapped(&[0xae], "®"), mapped(&[0xff], "ÿ"),]
        );
        Ok(())
    }

    #[test]
    fn rejects_unsafe_or_ambiguous_width_reconciliation() {
        let non_zero_truncation = parse_to_unicode_for_width(
            b"1 begincodespacerange <00> <FF> endcodespacerange \
              1 beginbfchar <0120> <0020> endbfchar",
            LIMITS,
            1,
        );
        assert!(matches!(non_zero_truncation, Err(Error::Unresolved(_))));

        let duplicate = parse_to_unicode_for_width(
            b"1 begincodespacerange <0000> <FFFF> endcodespacerange \
              2 beginbfchar <20> <0020> <0020> <0041> endbfchar",
            LIMITS,
            1,
        );
        assert!(matches!(duplicate, Err(Error::Unresolved(_))));

        let outside_decoder_domain = parse_to_unicode_for_width(
            b"1 begincodespacerange <0100> <FFFF> endcodespacerange",
            LIMITS,
            1,
        );
        assert!(matches!(outside_decoder_domain, Err(Error::Unresolved(_))));

        let outside_codespace = parse_to_unicode_for_width(
            b"1 begincodespacerange <0020> <0030> endcodespacerange \
              1 beginbfchar <31> <0031> endbfchar",
            LIMITS,
            1,
        );
        assert!(matches!(outside_codespace, Err(Error::Unresolved(_))));
    }

    #[test]
    fn rejects_ambiguous_codespaces_and_truncated_codes() -> Result<()> {
        let ambiguous = parse_to_unicode(
            b"2 begincodespacerange <00> <7F> <0000> <7FFF> endcodespacerange",
            LIMITS,
        );
        assert!(matches!(ambiguous, Err(Error::Unresolved(_))));

        let cmap = parse_to_unicode(
            b"1 begincodespacerange <8100> <81FF> endcodespacerange",
            LIMITS,
        )?;
        assert!(
            matches!(cmap.decode(&[0x81], usize::MAX), Err(Error::Unresolved(message)) if message.contains("truncated"))
        );
        Ok(())
    }

    #[test]
    fn accepts_adjacent_codespaces_and_rejects_overlap() -> Result<()> {
        let adjacent = parse_to_unicode(
            b"2 begincodespacerange <00> <7F> <80> <FF> endcodespacerange",
            LIMITS,
        )?;
        assert_eq!(adjacent.entry_count(), 2);

        let overlap = parse_to_unicode(
            b"2 begincodespacerange <00> <80> <80> <FF> endcodespacerange",
            LIMITS,
        );
        assert!(matches!(overlap, Err(Error::Unresolved(_))));
        Ok(())
    }

    #[test]
    fn indexes_many_codespaces_without_pairwise_scanning() -> Result<()> {
        const ENTRY_COUNT: usize = 16_384;
        let mut input = format!("{ENTRY_COUNT} begincodespacerange ");
        for code in 0..ENTRY_COUNT {
            input.push_str(&format!("<{code:04X}> <{code:04X}> "));
        }
        input.push_str("endcodespacerange");

        let cmap = parse_to_unicode(
            input.as_bytes(),
            CMapLimits {
                max_entries: ENTRY_COUNT,
                ..LIMITS
            },
        )?;
        assert_eq!(cmap.entry_count(), ENTRY_COUNT);
        assert_eq!(
            cmap.decode(&[0x12, 0x34], usize::MAX)?,
            vec![DecodedCode {
                source: vec![0x12, 0x34],
                mapping: UnicodeMapping::Unmapped,
            }]
        );
        Ok(())
    }

    #[test]
    fn rejects_malformed_utf16_and_enforces_limits() {
        let invalid_utf16 = parse_to_unicode(
            b"1 begincodespacerange <00> <FF> endcodespacerange \
              1 beginbfchar <41> <D800> endbfchar",
            LIMITS,
        );
        assert!(matches!(invalid_utf16, Err(Error::Unresolved(_))));

        let entry_limit = parse_to_unicode(
            b"1 begincodespacerange <00> <FF> endcodespacerange \
              1 beginbfrange <41> <43> <0041> endbfrange",
            CMapLimits {
                max_entries: 3,
                ..LIMITS
            },
        );
        assert!(matches!(
            entry_limit,
            Err(Error::LimitExceeded {
                resource: "CMap entries",
                limit: 3
            })
        ));

        let output_limit = parse_to_unicode(
            b"1 begincodespacerange <00> <FF> endcodespacerange \
              1 beginbfchar <41> <00660069> endbfchar",
            CMapLimits {
                max_output_scalars: 1,
                ..LIMITS
            },
        );
        assert!(matches!(
            output_limit,
            Err(Error::LimitExceeded {
                resource: "CMap output Unicode scalars",
                limit: 1
            })
        ));

        let repeated_mapping = parse_to_unicode(
            b"1 begincodespacerange <00> <FF> endcodespacerange \
              1 beginbfchar <41> <00610062006300640065006600670068> endbfchar",
            LIMITS,
        )
        .expect("repeated mapping fixture should parse");
        assert!(matches!(
            repeated_mapping.decode(b"AAAA", 31),
            Err(Error::LimitExceeded {
                resource: "decoded Unicode text bytes",
                limit: 31
            })
        ));
    }

    fn mapped(source: &[u8], text: &str) -> DecodedCode {
        DecodedCode {
            source: source.to_vec(),
            mapping: UnicodeMapping::Mapped(text.into()),
        }
    }
}
