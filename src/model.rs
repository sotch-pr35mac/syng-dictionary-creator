use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use unicode_normalization::UnicodeNormalization;

pub const SCHEMA_VERSION: u32 = 4;
pub const IDENTITY_VERSION: u8 = 1;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LexicalId(String);

impl LexicalId {
    pub fn new(
        simplified: &str,
        traditional: &str,
        numbered_pinyin: &str,
    ) -> Result<Self, ModelError> {
        Ok(Self::from_preimage(&Self::prehash_bytes(
            simplified,
            traditional,
            numbered_pinyin,
        )?))
    }

    pub fn from_preimage(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self(format!("{IDENTITY_VERSION}:{digest:x}"))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        let expected_prefix = format!("{IDENTITY_VERSION}:");
        let digest = value
            .strip_prefix(&expected_prefix)
            .ok_or(ModelError::InvalidLexicalId)?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ModelError::InvalidLexicalId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn prehash_bytes(
        simplified: &str,
        traditional: &str,
        numbered_pinyin: &str,
    ) -> Result<Vec<u8>, ModelError> {
        let simplified = normalize_headword(simplified)?;
        let traditional = normalize_headword(traditional)?;
        let mut bytes =
            Vec::with_capacity(simplified.len() + traditional.len() + numbered_pinyin.len() + 2);
        bytes.extend_from_slice(simplified.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(traditional.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(numbered_pinyin.as_bytes());
        Ok(bytes)
    }
}

impl fmt::Display for LexicalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub fn normalize_headword(value: &str) -> Result<String, ModelError> {
    let normalized = value.nfc().collect::<String>();
    let normalized = normalized.trim().to_owned();
    if normalized.is_empty() {
        return Err(ModelError::EmptyHeadword);
    }
    if normalized.chars().any(char::is_control) {
        return Err(ModelError::InvalidHeadword);
    }
    Ok(normalized)
}

pub fn normalize_text(value: &str) -> String {
    value.nfc().collect::<String>().trim().to_owned()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelError {
    EmptyHeadword,
    InvalidHeadword,
    InvalidLexicalId,
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for ModelError {}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Pinyin {
    pub marks: String,
    pub numbers: String,
    pub tones: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    CcCedict,
    ChineseNotes,
    Wiktionary,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Sourced<T> {
    pub value: T,
    pub sources: Vec<Source>,
}

impl<T> Sourced<T> {
    pub fn one(value: T, source: Source) -> Self {
        Self {
            value,
            sources: vec![source],
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct AlternativePronunciation {
    pub pronunciation: Pinyin,
    pub label: String,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Example {
    pub chinese: String,
    pub english: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QualifierCategory {
    Domain,
    Register,
    Region,
    Usage,
    Restriction,
    Information,
    Explanation,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Qualifier {
    pub category: QualifierCategory,
    pub value: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LexicalKind {
    Boilerplate,
    BoundForm,
    Classifier,
    Contraction,
    Expression,
    Foreign,
    Infix,
    Pattern,
    Phrase,
    Phonetic,
    Prefix,
    Proverb,
    Radical,
    SetPhrase,
    Suffix,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PartOfSpeech {
    Adjective,
    Adverb,
    AuxiliaryVerb,
    Conjunction,
    Determiner,
    Interjection,
    InterrogativePronoun,
    MeasureWord,
    Noun,
    Number,
    Onomatopoeia,
    Ordinal,
    Particle,
    Postposition,
    Preposition,
    Pronoun,
    ProperNoun,
    Quantity,
    Verb,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Definition {
    pub gloss: Sourced<String>,
    pub context: Vec<Sourced<String>>,
    pub examples: Vec<Sourced<Example>>,
    pub commentary: Vec<Sourced<String>>,
    pub qualifiers: Vec<Sourced<Qualifier>>,
    pub lexical_kinds: Vec<Sourced<LexicalKind>>,
    pub parts_of_speech: Vec<Sourced<PartOfSpeech>>,
    pub alternative_pronunciations: Vec<Sourced<AlternativePronunciation>>,
    pub measure_words: Vec<Sourced<LexicalId>>,
}

impl Definition {
    pub fn new(gloss: String, source: Source) -> Self {
        Self {
            gloss: Sourced::one(gloss, source),
            context: Vec::new(),
            examples: Vec::new(),
            commentary: Vec::new(),
            qualifiers: Vec::new(),
            lexical_kinds: Vec::new(),
            parts_of_speech: Vec::new(),
            alternative_pronunciations: Vec::new(),
            measure_words: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HskLevel {
    One,
    Two,
    Three,
    Four,
    Five,
    Six,
    SevenToNine,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct HskLevels {
    pub hsk_2015: Vec<HskLevel>,
    pub proficiency_standard_2021: Vec<HskLevel>,
    pub hsk_exam_syllabus_2025: Vec<HskLevel>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LexicalUnit {
    pub id: LexicalId,
    pub simplified: String,
    pub traditional: String,
    pub pinyin: Pinyin,
    pub alternative_pronunciations: Vec<Sourced<AlternativePronunciation>>,
    pub measure_words: Vec<Sourced<LexicalId>>,
    pub hsk: HskLevels,
    pub english: Vec<Definition>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BinaryEnvelope<T> {
    pub schema_version: u32,
    pub payload: T,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_preimage_uses_nul_separators_and_nfc_headwords() {
        let bytes = LexicalId::prehash_bytes(" 烟火 ", "煙火", "yan1huo3").unwrap();
        assert_eq!(bytes, "烟火\0煙火\0yan1huo3".as_bytes());
        assert_eq!(
            LexicalId::new("烟火", "煙火", "yan1huo3").unwrap().as_str(),
            "1:1f3478580959306ec1a7c9339a95346106204a327f7d0d7ea4764cdf49cdc2d9"
        );
    }

    #[test]
    fn identity_preserves_case_and_tones() {
        assert_ne!(
            LexicalId::new("复明", "復明", "fu4Ming2").unwrap(),
            LexicalId::new("复明", "復明", "fu4ming2").unwrap()
        );
        assert_ne!(
            LexicalId::new("烟火", "煙火", "yan1huo3").unwrap(),
            LexicalId::new("烟火", "煙火", "yan1huo5").unwrap()
        );
    }

    #[test]
    fn headwords_reject_empty_values_and_controls() {
        assert_eq!(normalize_headword(" \n "), Err(ModelError::EmptyHeadword));
        assert_eq!(
            normalize_headword("烟\0火"),
            Err(ModelError::InvalidHeadword)
        );
    }
}
