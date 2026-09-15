#![warn(missing_docs)]

//! Shared normalization and binary container support for Syng English search.

use fst::Map;
use std::collections::BTreeSet;
use std::fmt;
use std::ops::Range;
use unicode_normalization::UnicodeNormalization;

/// Eight-byte identifier at the start of every uncompressed search container.
pub const MAGIC: &[u8; 8] = b"SYNGENG\0";
/// Binary search container version.
pub const SEARCH_FORMAT_VERSION: u32 = 1;
/// Index and query normalization version.
pub const NORMALIZATION_VERSION: u32 = 1;
/// Optional-grammar transformation version.
pub const GRAMMAR_VERSION: u32 = 1;
/// English inflection model version.
pub const MORPHOLOGY_VERSION: u32 = 1;
/// Fixed header byte length.
pub const HEADER_LEN: usize = 48;
/// Fixed section-directory record byte length.
pub const DIRECTORY_ENTRY_LEN: usize = 32;

macro_rules! checked_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u32);

        impl $name {
            /// Constructs an identifier from its stored integer.
            pub const fn new(value: u32) -> Self {
                Self(value)
            }
            /// Returns the stored integer.
            pub const fn get(self) -> u32 {
                self.0
            }
            /// Converts an array index, rejecting values above `u32::MAX`.
            pub fn from_index(value: usize) -> Result<Self, FormatError> {
                Ok(Self(
                    u32::try_from(value).map_err(|_| FormatError::IntegerOverflow)?,
                ))
            }
        }
    };
}

checked_id!(TokenId, "Bundle-local normalized-token identifier.");
checked_id!(TextKey, "Bundle-local normalized search-text identifier.");
checked_id!(
    MorphFamilyId,
    "Bundle-local English lemma and part-of-speech family identifier."
);
checked_id!(RuntimeKey, "Bundle-local lexical-unit identifier.");

/// Derivation flag indicating a top-level semicolon alternative.
pub const DERIVATION_SEMICOLON: u16 = 1 << 0;
/// Derivation flag indicating omitted parenthetical or bracketed text.
pub const DERIVATION_PARENTHETICAL_OMISSION: u16 = 1 << 1;
/// Derivation flag indicating joined hyphen spelling.
pub const DERIVATION_HYPHENS_JOINED: u16 = 1 << 2;
/// Derivation flag indicating separated hyphen spelling.
pub const DERIVATION_HYPHENS_SEPARATED: u16 = 1 << 3;
/// Derivation flag indicating removed apostrophes.
pub const DERIVATION_APOSTROPHE_REMOVED: u16 = 1 << 4;
/// Derivation flag indicating optional grammar reduction.
pub const DERIVATION_GRAMMAR_REDUCED: u16 = 1 << 5;
/// Mask containing every known version-1 derivation flag.
pub const ALL_DERIVATION_FLAGS: u16 = (1 << 6) - 1;

/// Stable section identifiers in the binary directory.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SectionKind {
    /// Token vocabulary finite-state map.
    TokenFst = 1,
    /// Reverse token strings.
    TokenStrings = 2,
    /// Token summary records.
    TokenMeta = 3,
    /// Complete normalized phrase finite-state map.
    PhraseFst = 4,
    /// Search-text summary records.
    TextMeta = 5,
    /// Concatenated search-text token identifiers.
    TextTokens = 6,
    /// Search-text to lexical-unit bindings.
    TextBindings = 7,
    /// Positioned token occurrences.
    TokenOccurrences = 8,
    /// Ranked direct token hits.
    TokenHits = 9,
    /// Morphology surface finite-state map.
    MorphologyFst = 10,
    /// Surface-to-family mappings.
    MorphologyAnalyses = 11,
    /// Family-to-corpus-token mappings.
    MorphologyTokens = 12,
    /// Versioned logical metadata.
    Metadata = 13,
}

impl TryFrom<u32> for SectionKind {
    type Error = FormatError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            1 => Self::TokenFst,
            2 => Self::TokenStrings,
            3 => Self::TokenMeta,
            4 => Self::PhraseFst,
            5 => Self::TextMeta,
            6 => Self::TextTokens,
            7 => Self::TextBindings,
            8 => Self::TokenOccurrences,
            9 => Self::TokenHits,
            10 => Self::MorphologyFst,
            11 => Self::MorphologyAnalyses,
            12 => Self::MorphologyTokens,
            13 => Self::Metadata,
            _ => return Err(FormatError::UnknownSection(value)),
        })
    }
}

/// Section byte encoding.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectionCodec {
    /// Section-specific version-1 raw encoding.
    RawV1 = 1,
}

/// One owned section supplied to the container writer.
#[derive(Clone, Debug)]
pub struct Section {
    /// Stable section kind.
    pub kind: SectionKind,
    /// Section encoding.
    pub codec: SectionCodec,
    /// Logical number of records represented.
    pub item_count: u32,
    /// Encoded bytes.
    pub bytes: Vec<u8>,
}

/// Parsed section-directory entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SectionEntry {
    /// Stable section kind.
    pub kind: SectionKind,
    /// Section encoding.
    pub codec: SectionCodec,
    /// Absolute byte offset in the uncompressed container.
    pub offset: u64,
    /// Byte length.
    pub length: u64,
    /// Logical number of records represented.
    pub item_count: u32,
}

/// Borrowing, structurally validated view of a search container.
#[derive(Debug)]
pub struct EnglishSearchIndex<'a> {
    bytes: &'a [u8],
    lexical_unit_count: u32,
    sections: Vec<SectionEntry>,
}

impl<'a> EnglishSearchIndex<'a> {
    /// Parses and structurally validates a complete uncompressed container.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FormatError> {
        if bytes.len() < HEADER_LEN {
            return Err(FormatError::Truncated);
        }
        if &bytes[..8] != MAGIC {
            return Err(FormatError::BadMagic);
        }
        for (at, supported, name) in [
            (8, SEARCH_FORMAT_VERSION, "search"),
            (12, NORMALIZATION_VERSION, "normalization"),
            (16, GRAMMAR_VERSION, "grammar"),
            (20, MORPHOLOGY_VERSION, "morphology"),
        ] {
            let found = read_u32(bytes, at)?;
            if found != supported {
                return Err(FormatError::UnsupportedVersion(name, found));
            }
        }
        let lexical_unit_count = read_u32(bytes, 24)?;
        let directory_count = read_u32(bytes, 28)? as usize;
        let directory_offset =
            usize::try_from(read_u64(bytes, 32)?).map_err(|_| FormatError::IntegerOverflow)?;
        if read_u64(bytes, 40)? != 0 {
            return Err(FormatError::ReservedNonzero);
        }
        let directory_len = directory_count
            .checked_mul(DIRECTORY_ENTRY_LEN)
            .ok_or(FormatError::IntegerOverflow)?;
        let directory_end = directory_offset
            .checked_add(directory_len)
            .ok_or(FormatError::IntegerOverflow)?;
        if directory_offset < HEADER_LEN || directory_end > bytes.len() {
            return Err(FormatError::InvalidOffset);
        }
        let mut sections = Vec::with_capacity(directory_count);
        let mut previous_kind = 0;
        let mut ranges = Vec::new();
        for index in 0..directory_count {
            let at = directory_offset + index * DIRECTORY_ENTRY_LEN;
            let raw_kind = read_u32(bytes, at)?;
            if raw_kind <= previous_kind {
                return Err(FormatError::UnsortedOrDuplicateSections);
            }
            previous_kind = raw_kind;
            let kind = SectionKind::try_from(raw_kind)?;
            let codec = match read_u32(bytes, at + 4)? {
                1 => SectionCodec::RawV1,
                value => return Err(FormatError::UnknownCodec(value)),
            };
            let offset = read_u64(bytes, at + 8)?;
            let length = read_u64(bytes, at + 16)?;
            let item_count = read_u32(bytes, at + 24)?;
            if read_u32(bytes, at + 28)? != 0 {
                return Err(FormatError::ReservedNonzero);
            }
            let start = usize::try_from(offset).map_err(|_| FormatError::IntegerOverflow)?;
            let len = usize::try_from(length).map_err(|_| FormatError::IntegerOverflow)?;
            let end = start.checked_add(len).ok_or(FormatError::IntegerOverflow)?;
            if start % 8 != 0 || start < directory_end || end > bytes.len() {
                return Err(FormatError::InvalidOffset);
            }
            ranges.push((start, end));
            sections.push(SectionEntry {
                kind,
                codec,
                offset,
                length,
                item_count,
            });
        }
        ranges.sort_unstable();
        if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err(FormatError::OverlappingSections);
        }
        let required = [
            SectionKind::TokenFst,
            SectionKind::TokenStrings,
            SectionKind::TokenMeta,
            SectionKind::PhraseFst,
            SectionKind::TextMeta,
            SectionKind::TextTokens,
            SectionKind::TextBindings,
            SectionKind::TokenOccurrences,
            SectionKind::TokenHits,
            SectionKind::MorphologyFst,
            SectionKind::MorphologyAnalyses,
            SectionKind::MorphologyTokens,
            SectionKind::Metadata,
        ];
        if required
            .iter()
            .any(|kind| !sections.iter().any(|entry| entry.kind == *kind))
        {
            return Err(FormatError::MissingSection);
        }
        let parsed = Self {
            bytes,
            lexical_unit_count,
            sections,
        };
        Map::new(parsed.section(SectionKind::TokenFst).unwrap())
            .map_err(|_| FormatError::MalformedFst)?;
        Map::new(parsed.section(SectionKind::PhraseFst).unwrap())
            .map_err(|_| FormatError::MalformedFst)?;
        Map::new(parsed.section(SectionKind::MorphologyFst).unwrap())
            .map_err(|_| FormatError::MalformedFst)?;
        Ok(parsed)
    }

    /// Number of lexical units the index was built against.
    pub const fn lexical_unit_count(&self) -> u32 {
        self.lexical_unit_count
    }
    /// Sorted section directory.
    pub fn sections(&self) -> &[SectionEntry] {
        &self.sections
    }
    /// Returns one section's bytes.
    pub fn section(&self, kind: SectionKind) -> Option<&'a [u8]> {
        self.sections
            .iter()
            .find(|entry| entry.kind == kind)
            .map(|entry| {
                let start = entry.offset as usize;
                &self.bytes[start..start + entry.length as usize]
            })
    }
}

/// Writes one deterministic uncompressed container from pre-encoded sections.
pub fn write_container(
    lexical_unit_count: u32,
    mut sections: Vec<Section>,
) -> Result<Vec<u8>, FormatError> {
    sections.sort_by_key(|section| section.kind);
    if sections.windows(2).any(|pair| pair[0].kind == pair[1].kind) {
        return Err(FormatError::UnsortedOrDuplicateSections);
    }
    let count = u32::try_from(sections.len()).map_err(|_| FormatError::IntegerOverflow)?;
    let directory_end = HEADER_LEN
        .checked_add(
            sections
                .len()
                .checked_mul(DIRECTORY_ENTRY_LEN)
                .ok_or(FormatError::IntegerOverflow)?,
        )
        .ok_or(FormatError::IntegerOverflow)?;
    let first_section = align8(directory_end);
    let mut offsets = Vec::with_capacity(sections.len());
    let mut cursor = first_section;
    for section in &sections {
        cursor = align8(cursor);
        offsets.push(cursor);
        cursor = cursor
            .checked_add(section.bytes.len())
            .ok_or(FormatError::IntegerOverflow)?;
    }
    let mut output = vec![0; cursor];
    output[..8].copy_from_slice(MAGIC);
    put_u32(&mut output, 8, SEARCH_FORMAT_VERSION);
    put_u32(&mut output, 12, NORMALIZATION_VERSION);
    put_u32(&mut output, 16, GRAMMAR_VERSION);
    put_u32(&mut output, 20, MORPHOLOGY_VERSION);
    put_u32(&mut output, 24, lexical_unit_count);
    put_u32(&mut output, 28, count);
    put_u64(&mut output, 32, HEADER_LEN as u64);
    for (index, (section, offset)) in sections.iter().zip(offsets).enumerate() {
        let at = HEADER_LEN + index * DIRECTORY_ENTRY_LEN;
        put_u32(&mut output, at, section.kind as u32);
        put_u32(&mut output, at + 4, section.codec as u32);
        put_u64(&mut output, at + 8, offset as u64);
        put_u64(&mut output, at + 16, section.bytes.len() as u64);
        put_u32(&mut output, at + 24, section.item_count);
        output[offset..offset + section.bytes.len()].copy_from_slice(&section.bytes);
    }
    EnglishSearchIndex::parse(&output)?;
    Ok(output)
}

/// One normalized query token with its original UTF-8 byte coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryToken {
    /// Normalized spelling.
    pub text: String,
    /// Byte range in the original query.
    pub raw_range: Range<usize>,
    /// Raw-token group; aliases from one hyphenated token share this value.
    pub raw_group: u32,
}

/// A phrase segment that cannot match across hard punctuation boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuerySegment {
    /// Literal normalized tokens.
    pub tokens: Vec<QueryToken>,
}

/// One uniformly transformed view of all query phrase segments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryView {
    /// Hard-boundary-delimited query segments.
    pub segments: Vec<QuerySegment>,
    /// Spelling derivations applied to this view.
    pub derivation_flags: u16,
}

/// Produces literal and uniform spelling-alias query views with raw coverage intact.
pub fn query_views(raw: &str) -> Vec<QueryView> {
    let literal = tokenize_query(raw);
    let has_apostrophe = literal
        .iter()
        .flat_map(|segment| &segment.tokens)
        .any(|token| token.text.contains('\''));
    let has_hyphen = literal
        .iter()
        .flat_map(|segment| &segment.tokens)
        .any(|token| token.text.contains('-'));
    let mut modes = BTreeSet::from([(false, 0_u8)]);
    if has_apostrophe {
        modes.insert((true, 0));
    }
    if has_hyphen {
        modes.insert((false, 1));
        modes.insert((false, 2));
    }
    if has_apostrophe && has_hyphen {
        modes.insert((true, 1));
        modes.insert((true, 2));
    }
    modes
        .into_iter()
        .map(|(remove_apostrophe, hyphen_mode)| {
            let mut flags = if remove_apostrophe {
                DERIVATION_APOSTROPHE_REMOVED
            } else {
                0
            };
            flags |= match hyphen_mode {
                1 => DERIVATION_HYPHENS_JOINED,
                2 => DERIVATION_HYPHENS_SEPARATED,
                _ => 0,
            };
            let segments = literal
                .iter()
                .map(|segment| {
                    let tokens = segment
                        .tokens
                        .iter()
                        .flat_map(|token| {
                            let text = if remove_apostrophe {
                                token.text.replace('\'', "")
                            } else {
                                token.text.clone()
                            };
                            let forms = match hyphen_mode {
                                1 => vec![text.replace('-', "")],
                                2 => text
                                    .split('-')
                                    .filter(|part| !part.is_empty())
                                    .map(str::to_owned)
                                    .collect(),
                                _ => vec![text],
                            };
                            forms
                                .into_iter()
                                .map(|text| QueryToken {
                                    text,
                                    raw_range: token.raw_range.clone(),
                                    raw_group: token.raw_group,
                                })
                                .collect::<Vec<_>>()
                        })
                        .collect();
                    QuerySegment { tokens }
                })
                .collect();
            QueryView {
                segments,
                derivation_flags: flags,
            }
        })
        .collect()
}

/// Tokenizes a query while retaining hard boundaries and raw UTF-8 ranges.
pub fn tokenize_query(raw: &str) -> Vec<QuerySegment> {
    let mut segments = Vec::new();
    let mut tokens = Vec::new();
    let mut token_start = None;
    let mut group = 0_u32;
    let chars: Vec<(usize, char)> = raw.char_indices().collect();
    let finish =
        |end: usize, start: &mut Option<usize>, tokens: &mut Vec<QueryToken>, group: &mut u32| {
            if let Some(begin) = start.take() {
                let normalized = normalize_text(&raw[begin..end]);
                for text in normalized.split(' ') {
                    if !text.is_empty() {
                        tokens.push(QueryToken {
                            text: text.to_owned(),
                            raw_range: begin..end,
                            raw_group: *group,
                        });
                    }
                }
                *group = group.saturating_add(1);
            }
        };
    for (index, &(at, ch)) in chars.iter().enumerate() {
        let end = chars.get(index + 1).map_or(raw.len(), |value| value.0);
        if is_hard_boundary(ch) {
            finish(at, &mut token_start, &mut tokens, &mut group);
            if !tokens.is_empty() {
                segments.push(QuerySegment {
                    tokens: std::mem::take(&mut tokens),
                });
            }
        } else if is_base_character(ch)
            || (is_combining_mark(ch) && token_start.is_some())
            || ((ch == '\'' || ch == '’' || ch == '-')
                && token_start.is_some()
                && chars
                    .get(index + 1)
                    .is_some_and(|(_, next)| is_base_character(*next)))
        {
            token_start.get_or_insert(at);
        } else {
            finish(at, &mut token_start, &mut tokens, &mut group);
        }
        if index + 1 == chars.len() {
            finish(end, &mut token_start, &mut tokens, &mut group);
        }
    }
    if !tokens.is_empty() {
        segments.push(QuerySegment { tokens });
    }
    segments
}

/// Normalizes prose into ASCII-space-separated lexical tokens.
pub fn normalize_text(value: &str) -> String {
    let folded = value
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>();
    let chars: Vec<char> = folded.nfc().collect();
    let mut result = String::new();
    let mut previous_space = true;
    for (index, &ch) in chars.iter().enumerate() {
        let attached = is_base_character(ch) || (is_combining_mark(ch) && !previous_space);
        let internal = (ch == '\'' || ch == '’' || ch == '-')
            && index > 0
            && index + 1 < chars.len()
            && (is_base_character(chars[index - 1]) || is_combining_mark(chars[index - 1]))
            && is_base_character(chars[index + 1]);
        if attached || internal {
            if ch == '’' {
                result.push('\'');
            } else {
                result.push(ch);
            }
            previous_space = false;
        } else if !previous_space && !result.is_empty() {
            result.push(' ');
            previous_space = true;
        }
    }
    while result.ends_with(' ') {
        result.pop();
    }
    result
}

/// Returns the literal spelling plus deterministic apostrophe and hyphen aliases.
pub fn spelling_views(normalized: &str) -> Vec<(String, u16)> {
    let mut views = BTreeSet::from([(normalized.to_owned(), 0_u16)]);
    if normalized.contains('\'') {
        views.insert((normalized.replace('\'', ""), DERIVATION_APOSTROPHE_REMOVED));
    }
    if normalized.contains('-') {
        views.insert((normalized.replace('-', ""), DERIVATION_HYPHENS_JOINED));
        views.insert((
            normalized
                .replace('-', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            DERIVATION_HYPHENS_SEPARATED,
        ));
    }
    views.into_iter().collect()
}

/// Applies the single optional-grammar reduction view to normalized tokens.
pub fn reduce_optional_grammar(tokens: &[&str]) -> Option<Vec<String>> {
    if tokens.len() < 2 {
        return None;
    }
    let mut reduced = tokens
        .iter()
        .copied()
        .filter(|token| !matches!(*token, "a" | "an" | "the"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if reduced.len() > 1 && reduced.first().is_some_and(|token| token == "to") {
        reduced.remove(0);
    }
    if reduced.len() > 1
        && reduced.first().is_some_and(|token| {
            matches!(
                token.as_str(),
                "be" | "am" | "is" | "are" | "was" | "were" | "been" | "being"
            )
        })
    {
        reduced.remove(0);
    }
    if reduced.is_empty()
        || reduced
            .iter()
            .map(String::as_str)
            .eq(tokens.iter().copied())
    {
        None
    } else {
        Some(reduced)
    }
}

/// Encodes an unsigned integer using canonical LEB128.
pub fn write_uleb128(mut value: u64, output: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Decodes one canonical unsigned LEB128 integer.
pub fn read_uleb128(bytes: &[u8], cursor: &mut usize) -> Result<u64, FormatError> {
    let start = *cursor;
    let mut result = 0_u64;
    for shift in (0..=63).step_by(7) {
        let byte = *bytes.get(*cursor).ok_or(FormatError::Truncated)?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err(FormatError::MalformedLeb128);
        }
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            let mut canonical = Vec::new();
            write_uleb128(result, &mut canonical);
            if canonical.len() != *cursor - start {
                return Err(FormatError::MalformedLeb128);
            }
            return Ok(result);
        }
    }
    Err(FormatError::MalformedLeb128)
}

fn is_base_character(ch: char) -> bool {
    ch.is_alphanumeric()
}
fn is_combining_mark(ch: char) -> bool {
    unicode_normalization::char::is_combining_mark(ch)
}
fn is_hard_boundary(ch: char) -> bool {
    matches!(
        ch,
        ',' | ';'
            | ':'
            | '.'
            | '!'
            | '?'
            | '\n'
            | '\r'
            | '，'
            | '；'
            | '：'
            | '。'
            | '！'
            | '？'
            | '、'
    )
}
fn align8(value: usize) -> usize {
    (value + 7) & !7
}
fn read_u32(bytes: &[u8], at: usize) -> Result<u32, FormatError> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(FormatError::Truncated)?
            .try_into()
            .unwrap(),
    ))
}
fn read_u64(bytes: &[u8], at: usize) -> Result<u64, FormatError> {
    Ok(u64::from_le_bytes(
        bytes
            .get(at..at + 8)
            .ok_or(FormatError::Truncated)?
            .try_into()
            .unwrap(),
    ))
}
fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_u64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// Structural format failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormatError {
    /// Input ended inside a required field.
    Truncated,
    /// Magic bytes do not identify this format.
    BadMagic,
    /// A component version is unsupported.
    UnsupportedVersion(&'static str, u32),
    /// An integer could not be represented safely.
    IntegerOverflow,
    /// Reserved bytes are nonzero.
    ReservedNonzero,
    /// A section kind is unknown.
    UnknownSection(u32),
    /// A section codec is unknown.
    UnknownCodec(u32),
    /// Required sections are absent.
    MissingSection,
    /// Sections are duplicated or not sorted.
    UnsortedOrDuplicateSections,
    /// A section offset, length, or alignment is invalid.
    InvalidOffset,
    /// Section byte ranges overlap.
    OverlappingSections,
    /// A finite-state section is malformed.
    MalformedFst,
    /// A variable integer is malformed or noncanonical.
    MalformedLeb128,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for FormatError {}

#[cfg(test)]
mod tests {
    use super::*;
    use fst::Map;

    #[test]
    fn normalization_retains_digits_and_builds_spelling_aliases() {
        let normalized = normalize_text("3-D, don't");
        assert_eq!(normalized, "3-d don't");
        assert_eq!(normalize_text("Ｅ\u{301}COLE"), "école");
        assert_eq!(normalize_text("\u{301}accent"), "accent");
        assert!(spelling_views("3-d").contains(&("3d".to_owned(), DERIVATION_HYPHENS_JOINED)));
        assert!(spelling_views("3-d").contains(&("3 d".to_owned(), DERIVATION_HYPHENS_SEPARATED)));
        assert!(
            spelling_views("don't").contains(&("dont".to_owned(), DERIVATION_APOSTROPHE_REMOVED))
        );
    }

    #[test]
    fn grammar_is_narrow_and_never_discards_standalone_words() {
        assert_eq!(
            reduce_optional_grammar(&["to", "be", "a", "doctor"]),
            Some(vec!["doctor".to_owned()])
        );
        assert_eq!(
            reduce_optional_grammar(&["a", "picture", "of", "the", "store"]),
            Some(
                vec!["picture", "of", "store"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            )
        );
        for word in ["is", "too", "a", "the"] {
            assert_eq!(reduce_optional_grammar(&[word]), None);
        }
        assert_eq!(reduce_optional_grammar(&["give", "up"]), None);
        assert_eq!(reduce_optional_grammar(&["my", "name", "is"]), None);
        for phrase in [
            &["to", "happy"][..],
            &["be", "happy"],
            &["to", "be", "happy"],
            &["a", "happy"],
            &["the", "happy"],
        ] {
            assert_eq!(
                reduce_optional_grammar(phrase),
                Some(vec!["happy".to_owned()])
            );
        }
    }

    #[test]
    fn query_ranges_and_hard_boundaries_refer_to_raw_utf8() {
        let segments = tokenize_query("École—3-D；don't");
        assert_eq!(segments.len(), 2);
        assert_eq!(
            segments[0]
                .tokens
                .iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>(),
            ["école", "3-d"]
        );
        assert_eq!(
            &"École—3-D；don't"[segments[0].tokens[0].raw_range.clone()],
            "École"
        );
        assert_eq!(segments[1].tokens[0].text, "don't");
        let separated = query_views("3-D")
            .into_iter()
            .find(|view| view.derivation_flags == DERIVATION_HYPHENS_SEPARATED)
            .unwrap();
        assert_eq!(
            separated.segments[0]
                .tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<Vec<_>>(),
            ["3", "d"]
        );
        assert_eq!(
            separated.segments[0].tokens[0].raw_range,
            separated.segments[0].tokens[1].raw_range
        );
        assert_eq!(
            separated.segments[0].tokens[0].raw_group,
            separated.segments[0].tokens[1].raw_group
        );
    }

    #[test]
    fn container_rejects_damage() {
        let empty_map = Map::from_iter(std::iter::empty::<(&str, u64)>()).unwrap();
        let empty_fst = empty_map.as_fst().as_bytes().to_vec();
        let sections = (1..=13)
            .map(|raw| Section {
                kind: SectionKind::try_from(raw).unwrap(),
                codec: SectionCodec::RawV1,
                item_count: 0,
                bytes: if matches!(raw, 1 | 4 | 10) {
                    empty_fst.clone()
                } else {
                    Vec::new()
                },
            })
            .collect();
        let bytes = write_container(0, sections).unwrap();
        assert!(EnglishSearchIndex::parse(&bytes).is_ok());
        let mut bad = bytes.clone();
        bad[0] = 0;
        assert_eq!(
            EnglishSearchIndex::parse(&bad).unwrap_err(),
            FormatError::BadMagic
        );
        let mut bad = bytes;
        bad[8] = 2;
        assert_eq!(
            EnglishSearchIndex::parse(&bad).unwrap_err(),
            FormatError::UnsupportedVersion("search", 2)
        );

        let mut bad = write_test_container(&empty_fst);
        bad[28..32].copy_from_slice(&12_u32.to_le_bytes());
        assert_eq!(
            EnglishSearchIndex::parse(&bad).unwrap_err(),
            FormatError::MissingSection
        );

        let mut bad = write_test_container(&empty_fst);
        bad[HEADER_LEN + DIRECTORY_ENTRY_LEN..HEADER_LEN + DIRECTORY_ENTRY_LEN + 4]
            .copy_from_slice(&1_u32.to_le_bytes());
        assert_eq!(
            EnglishSearchIndex::parse(&bad).unwrap_err(),
            FormatError::UnsortedOrDuplicateSections
        );

        let mut bad = write_test_container(&empty_fst);
        let first_offset = read_u64(&bad, HEADER_LEN + 8).unwrap();
        put_u64(&mut bad, HEADER_LEN + 8, first_offset + 1);
        assert_eq!(
            EnglishSearchIndex::parse(&bad).unwrap_err(),
            FormatError::InvalidOffset
        );

        let mut bad = write_test_container(&empty_fst);
        let first_offset = read_u64(&bad, HEADER_LEN + 8).unwrap() as usize;
        bad[first_offset] ^= 0xff;
        assert_eq!(
            EnglishSearchIndex::parse(&bad).unwrap_err(),
            FormatError::MalformedFst
        );
    }

    fn write_test_container(empty_fst: &[u8]) -> Vec<u8> {
        write_container(
            0,
            (1..=13)
                .map(|raw| Section {
                    kind: SectionKind::try_from(raw).unwrap(),
                    codec: SectionCodec::RawV1,
                    item_count: 0,
                    bytes: if matches!(raw, 1 | 4 | 10) {
                        empty_fst.to_vec()
                    } else {
                        Vec::new()
                    },
                })
                .collect(),
        )
        .unwrap()
    }
}
