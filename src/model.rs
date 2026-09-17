//! Canonical serialized model and stable lexical identity primitives.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::{fmt, str::FromStr};
use unicode_normalization::UnicodeNormalization;

/// Incompatible schema version wrapped around every binary artifact.
pub const SCHEMA_VERSION: u32 = 4;
/// Version prefix used by persistent lexical identifiers.
pub const IDENTITY_VERSION: u8 = 1;

/// Persistent identity derived from normalized headwords and canonical Pinyin.
#[derive(
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct LexicalId([u8; 32]);

impl LexicalId {
    /// Computes an identity from simplified, traditional, and numbered-Pinyin fields.
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

    /// Hashes an already canonical identity preimage and adds the version prefix.
    pub fn from_preimage(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// Validates and wraps a serialized lexical identifier.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ModelError> {
        let value = value.as_ref();
        let digest = value
            .strip_prefix("1:")
            .ok_or(ModelError::InvalidLexicalId)?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ModelError::InvalidLexicalId);
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in digest.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (hex_nibble(pair[0]).ok_or(ModelError::InvalidLexicalId)? << 4)
                | hex_nibble(pair[1]).ok_or(ModelError::InvalidLexicalId)?;
        }
        Ok(Self(bytes))
    }

    /// Returns the identity format version.
    pub const fn version(&self) -> u8 {
        IDENTITY_VERSION
    }

    /// Returns the fixed SHA-256 digest used for constant-time archive lookup.
    pub const fn digest(&self) -> &[u8; 32] {
        &self.0
    }

    /// Produces the exact NUL-separated bytes used as the SHA-256 preimage.
    pub fn prehash_bytes(
        simplified: &str,
        traditional: &str,
        numbered_pinyin: &str,
    ) -> Result<Vec<u8>, ModelError> {
        let simplified = normalize_headword(simplified)?;
        let traditional = normalize_headword(traditional)?;
        let numbered_pinyin = crate::pinyin::canonicalize_numbered(numbered_pinyin)
            .map_err(|_| ModelError::InvalidPinyin)?;
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
        formatter.write_str("1:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for LexicalId {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for LexicalId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for LexicalId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Normalizes a headword with NFC and outer trimming and rejects unsafe values.
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

/// Applies NFC normalization and outer trimming to publishable prose.
pub fn normalize_text(value: &str) -> String {
    value.nfc().collect::<String>().trim().to_owned()
}

/// Failure modes for lexical identity and text normalization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelError {
    /// A headword contains no characters after outer trimming.
    EmptyHeadword,
    /// A headword contains a control character.
    InvalidHeadword,
    /// A serialized lexical identifier has the wrong version or digest form.
    InvalidLexicalId,
    /// Numbered Pinyin cannot be canonicalized for identity construction.
    InvalidPinyin,
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for ModelError {}

/// Canonical Pinyin representations stored together for display and lookup.
#[derive(
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct Pinyin {
    /// Tone-marked, space-separated display form.
    pub marks: String,
    /// Concatenated syllable-plus-tone identity and lookup form.
    pub numbers: String,
    /// Tone number for each syllable, in order.
    pub tones: Vec<u8>,
}

/// Upstream dictionary that supports a published value.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// CC-CEDICT.
    CcCedict,
    /// Chinese Notes.
    ChineseNotes,
    /// English Wiktionary.
    Wiktionary,
}

/// A value paired with one or more ordered source attributions.
#[derive(
    Clone,
    Debug,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct Sourced<T> {
    /// Published value.
    pub value: T,
    /// Ordered, deduplicated sources supporting the value.
    pub sources: Vec<Source>,
}

impl<T> Sourced<T> {
    /// Creates a value attributed to one source.
    pub fn one(value: T, source: Source) -> Self {
        Self {
            value,
            sources: vec![source],
        }
    }
}

/// A pronunciation variant that does not exist as its own lexical entity.
#[derive(
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct AlternativePronunciation {
    /// Canonical alternate Pinyin.
    pub pronunciation: Pinyin,
    /// Source-provided scope label such as `Taiwan pr.` or `also pr.`.
    pub label: String,
}

/// Structured bilingual usage example.
#[derive(
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct Example {
    /// Simplified Chinese example text, source-attested or generated as a display fallback.
    pub simplified: Option<String>,
    /// Traditional Chinese example text, source-attested or generated as a display fallback.
    pub traditional: Option<String>,
    /// English translation when the source provides one.
    pub english: Option<String>,
}

/// Reviewed semantic category for a structured qualifier.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum QualifierCategory {
    /// Subject area or professional domain.
    Domain,
    /// Formality or social register.
    Register,
    /// Geographic variety.
    Region,
    /// Usage condition.
    Usage,
    /// Grammatical or lexical restriction.
    Restriction,
    /// Informational note.
    Information,
    /// Explanatory annotation.
    Explanation,
}

/// Reviewed structured qualifier attached to a definition.
#[derive(
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct Qualifier {
    /// Semantic category of the qualifier.
    pub category: QualifierCategory,
    /// Normalized source value.
    pub value: String,
}

/// Reviewed kinds of lexical entity that are not ordinary parts of speech.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum LexicalKind {
    /// Source boilerplate entry.
    Boilerplate,
    /// Form that cannot occur independently.
    BoundForm,
    /// Classifier or measure-word entry.
    Classifier,
    /// Contracted form.
    Contraction,
    /// Conventional expression.
    Expression,
    /// Foreign or borrowed form.
    Foreign,
    /// Infix.
    Infix,
    /// Idiomatic expression.
    Idiom,
    /// Productive lexical pattern.
    Pattern,
    /// Multiword phrase.
    Phrase,
    /// Phonetic component or use.
    Phonetic,
    /// Prefix.
    Prefix,
    /// Proverb.
    Proverb,
    /// Character radical.
    Radical,
    /// Fixed or set phrase.
    SetPhrase,
    /// Suffix.
    Suffix,
}

/// Reviewed grammatical parts of speech.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum PartOfSpeech {
    /// Adjective.
    Adjective,
    /// Adverb.
    Adverb,
    /// Auxiliary verb.
    AuxiliaryVerb,
    /// Conjunction.
    Conjunction,
    /// Determiner.
    Determiner,
    /// Interjection.
    Interjection,
    /// Interrogative pronoun.
    InterrogativePronoun,
    /// Measure word.
    MeasureWord,
    /// Noun.
    Noun,
    /// Number.
    Number,
    /// Onomatopoeia.
    Onomatopoeia,
    /// Ordinal.
    Ordinal,
    /// Particle.
    Particle,
    /// Postposition.
    Postposition,
    /// Preposition.
    Preposition,
    /// Pronoun.
    Pronoun,
    /// Proper noun.
    ProperNoun,
    /// Quantity expression.
    Quantity,
    /// Verb.
    Verb,
}

/// One ordered English definition and its independently attributed metadata.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct Definition {
    /// Leaf English gloss.
    pub gloss: Sourced<String>,
    /// Ordered parent glosses that scope the leaf gloss.
    pub context: Vec<Sourced<String>>,
    /// Structured usage examples.
    pub examples: Vec<Sourced<Example>>,
    /// Substantive explanatory prose.
    pub commentary: Vec<Sourced<String>>,
    /// Reviewed structured qualifiers.
    pub qualifiers: Vec<Sourced<Qualifier>>,
    /// Reviewed lexical kinds.
    pub lexical_kinds: Vec<Sourced<LexicalKind>>,
    /// Reviewed parts of speech.
    pub parts_of_speech: Vec<Sourced<PartOfSpeech>>,
    /// Pronunciations scoped to this definition.
    pub alternative_pronunciations: Vec<Sourced<AlternativePronunciation>>,
    /// Classifier identities scoped to this definition.
    pub measure_words: Vec<Sourced<LexicalId>>,
}

impl Definition {
    /// Creates a definition with an attributed gloss and empty metadata vectors.
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

/// Closed proficiency level shared by the supported HSK systems.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub enum HskLevel {
    /// Level one.
    One,
    /// Level two.
    Two,
    /// Level three.
    Three,
    /// Level four.
    Four,
    /// Level five.
    Five,
    /// Level six.
    Six,
    /// Combined levels seven through nine.
    SevenToNine,
}

/// HSK memberships across historical and current standards.
#[derive(
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct HskLevels {
    /// Memberships in the 2015 HSK vocabulary.
    pub hsk_2015: Vec<HskLevel>,
    /// Memberships in the 2021 Chinese proficiency standard.
    pub proficiency_standard_2021: Vec<HskLevel>,
    /// Memberships in the 2025 HSK exam syllabus.
    pub hsk_exam_syllabus_2025: Vec<HskLevel>,
}

/// Canonical published dictionary entity for one exact identity tuple.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct LexicalUnit {
    /// Persistent content-derived identity.
    pub id: LexicalId,
    /// NFC-normalized simplified headword.
    pub simplified: String,
    /// NFC-normalized traditional headword.
    pub traditional: String,
    /// Primary Mandarin pronunciation.
    pub pinyin: Pinyin,
    /// Document-normalized commonness score; zero means unseen in the frequency corpora.
    pub commonness: f32,
    /// Entity-scoped pronunciation variants without their own lexical entity.
    pub alternative_pronunciations: Vec<Sourced<AlternativePronunciation>>,
    /// Entity-scoped classifier identities.
    pub measure_words: Vec<Sourced<LexicalId>>,
    /// HSK proficiency memberships.
    pub hsk: HskLevels,
    /// Ordered English definitions.
    pub english: Vec<Definition>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_preimage_uses_nul_separators_and_nfc_headwords() {
        let bytes = LexicalId::prehash_bytes(" 烟火 ", "煙火", "yan1huo3").unwrap();
        assert_eq!(bytes, "烟火\0煙火\0yan1huo3".as_bytes());
        assert_eq!(
            LexicalId::new("烟火", "煙火", "yan1huo3")
                .unwrap()
                .to_string(),
            "1:1f3478580959306ec1a7c9339a95346106204a327f7d0d7ea4764cdf49cdc2d9"
        );
        assert_eq!(
            LexicalId::new("烟火", "煙火", "yan1-huo3").unwrap(),
            LexicalId::new("烟火", "煙火", "yan1huo3").unwrap()
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
