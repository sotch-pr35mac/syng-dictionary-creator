//! Conservative Hanyu Pinyin parsing and canonicalization.

use crate::model::Pinyin;
use prettify_pinyin::prettify;
use std::collections::HashSet;
use std::fmt;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

static VALID_SYLLABLES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    // Standard Hanyu Pinyin syllables. Case is checked only after lookup so the
    // spelling retained in identity remains exactly source-significant.
    "a ai an ang ao ba bai ban bang bao bei ben beng bi bian biao bie bin bing bo bu \
     ca cai can cang cao ce cen ceng cha chai chan chang chao che chen cheng chi chong \
     chou chu chua chuai chuan chuang chui chun chuo ci cong cou cu cuan cui cun cuo \
     da dai dan dang dao de dei den deng di dia dian diao die ding diu dong dou du duan \
     dui dun duo e ei en eng er fa fan fang fei fen feng fo fou fu ga gai gan gang gao \
     ge gei gen geng gong gou gu gua guai guan guang gui gun guo ha hai han hang hao he \
     hei hen heng hm hng hong hou hu hua huai huan huang hui hun huo ji jia jian jiang \
     jiao jie jin jing jiong jiu ju juan jue jun ka kai kan kang kao ke kei ken keng kong \
     kou ku kua kuai kuan kuang kui kun kuo la lai lan lang lao le lei leng li lia lian \
     liang liao lie lin ling liu lo long lou lu lü luan lüan lue lüe lun luo ma mai man \
     mang mao me mei men meng mi mian miao mie min ming miu mo mou mu n na nai nan nang \
     nao ne nei nen neng ng ni nian niang niao nie nin ning niu nong nou nu nü nuan nüan \
     nue nüe nun nuo o ou pa pai pan pang pao pei pen peng pi pian piao pie pin ping po pou \
     pu qi qia qian qiang qiao qie qin qing qiong qiu qu quan que qun r ran rang rao re \
     ren reng ri rong rou ru rua ruan rui run ruo sa sai san sang sao se sen seng sha shai \
     shan shang shao she shei shen sheng shi shou shu shua shuai shuan shuang shui shun \
     shuo si song sou su suan sui sun suo ta tai tan tang tao te teng ti tian tiao tie \
     ting tong tou tu tuan tui tun tuo wa wai wan wang wei wen weng wo wu xi xia xian \
     xiang xiao xie xin xing xiong xiu xu xuan xue xun ya yan yang yao ye yi yin ying \
     yong you yu yuan yue yun za zai zan zang zao ze zei zen zeng zha zhai zhan zhang \
     zhao zhe zhei zhen zheng zhi zhong zhou zhu zhua zhuai zhuan zhuang zhui zhun \
     zhuo zi zong zou zu zuan zui zun zuo"
        .split_whitespace()
        .collect()
});

/// Reasons a pronunciation cannot be converted into canonical Mandarin Pinyin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinyinError {
    /// Input contains no pronunciation.
    Empty,
    /// Input contains a character outside the accepted Pinyin syntax.
    InvalidCharacter,
    /// A numbered syllable does not end in a tone from one through five.
    MissingTone,
    /// A supplied tone is outside the range one through five.
    InvalidTone,
    /// A token is not in the reviewed Hanyu Pinyin syllable vocabulary.
    InvalidSyllable(String),
    /// The source explicitly marks the pronunciation as unavailable.
    NotApplicable,
    /// Marked input admits more than one valid syllable segmentation.
    AmbiguousSegmentation,
    /// No segmentation matches the expected Han-character count.
    SyllableCount,
}

impl fmt::Display for PinyinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for PinyinError {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Syllable {
    letters: String,
    tone: u8,
}

/// Parses numbered Pinyin into canonical display, identity, and tone forms.
pub fn from_numbered(value: &str) -> Result<Pinyin, PinyinError> {
    let normalized = value.nfc().collect::<String>();
    if normalized.trim().is_empty() {
        return Err(PinyinError::Empty);
    }
    if normalized.trim() == "xx5" {
        return Err(PinyinError::NotApplicable);
    }

    let mut syllables = Vec::new();
    let mut letters = String::new();
    for character in normalized.chars() {
        if character.is_ascii_alphabetic() || matches!(character, 'ü' | 'Ü') {
            letters.push(character);
        } else if character == ':' {
            let previous = letters.pop().ok_or(PinyinError::InvalidCharacter)?;
            if previous == 'u' {
                letters.push('ü');
            } else if previous == 'U' {
                letters.push('Ü');
            } else {
                return Err(PinyinError::InvalidCharacter);
            }
        } else if character == 'v' || character == 'V' {
            // This branch is unreachable because v is alphabetic; normalization
            // is performed below after collecting the complete token.
            letters.push(character);
        } else if character.is_ascii_digit() {
            let tone = character
                .to_digit(10)
                .and_then(|number| u8::try_from(number).ok())
                .ok_or(PinyinError::InvalidTone)?;
            if !(1..=5).contains(&tone) {
                return Err(PinyinError::InvalidTone);
            }
            push_numbered_syllable(&mut syllables, &mut letters, tone)?;
        } else if character.is_whitespace() || matches!(character, '-' | '\'' | '’') {
            if !letters.is_empty() {
                return Err(PinyinError::MissingTone);
            }
        } else {
            return Err(PinyinError::InvalidCharacter);
        }
    }
    if !letters.is_empty() {
        return Err(PinyinError::MissingTone);
    }
    if syllables.is_empty() {
        return Err(PinyinError::Empty);
    }
    Ok(make_pinyin(syllables))
}

/// Validates and appends one completed numbered syllable.
fn push_numbered_syllable(
    syllables: &mut Vec<Syllable>,
    letters: &mut String,
    tone: u8,
) -> Result<(), PinyinError> {
    if letters.is_empty() {
        return Err(PinyinError::MissingTone);
    }
    let normalized_letters = letters
        .chars()
        .map(|character| match character {
            'v' => 'ü',
            'V' => 'Ü',
            other => other,
        })
        .collect::<String>();
    validate_syllable(&normalized_letters)?;
    syllables.push(Syllable {
        letters: normalized_letters,
        tone,
    });
    letters.clear();
    Ok(())
}

/// Converts marked Pinyin only when exactly one reviewed segmentation is valid.
pub fn from_marked(value: &str, han_character_count: Option<usize>) -> Result<Pinyin, PinyinError> {
    let normalized = value.nfc().collect::<String>();
    if normalized.trim().is_empty() {
        return Err(PinyinError::Empty);
    }

    let chunks = normalized
        .split(|character: char| character.is_whitespace() || matches!(character, '-' | '\'' | '’'))
        .filter(|chunk| !chunk.is_empty())
        .collect::<Vec<_>>();
    let mut segmentations = vec![Vec::<Syllable>::new()];
    for chunk in chunks {
        let chunk_segmentations = segment_marked_chunk(chunk)?;
        let mut joined = Vec::new();
        for prefix in &segmentations {
            for suffix in &chunk_segmentations {
                let mut candidate = prefix.clone();
                candidate.extend(suffix.clone());
                joined.push(candidate);
                if joined.len() > 2_048 {
                    return Err(PinyinError::AmbiguousSegmentation);
                }
            }
        }
        segmentations = joined;
    }

    if let Some(expected_count) = han_character_count {
        segmentations.retain(|candidate| candidate.len() == expected_count);
    }
    segmentations.sort_by(|left, right| {
        left.iter()
            .map(|syllable| (&syllable.letters, syllable.tone))
            .cmp(
                right
                    .iter()
                    .map(|syllable| (&syllable.letters, syllable.tone)),
            )
    });
    segmentations.dedup();
    match segmentations.len() {
        0 => Err(PinyinError::SyllableCount),
        1 => Ok(make_pinyin(segmentations.pop().expect("one segmentation"))),
        _ => Err(PinyinError::AmbiguousSegmentation),
    }
}

/// Enumerates all valid syllable segmentations of one unseparated marked chunk.
fn segment_marked_chunk(chunk: &str) -> Result<Vec<Vec<Syllable>>, PinyinError> {
    let mut letters = Vec::new();
    let mut tones = Vec::new();
    for character in chunk.chars() {
        let (base, tone) = marked_character(character).ok_or(PinyinError::InvalidCharacter)?;
        letters.push(base);
        tones.push(tone);
    }
    let mut results = Vec::new();
    segment_from(&letters, &tones, 0, &mut Vec::new(), &mut results);
    if results.is_empty() {
        return Err(PinyinError::InvalidSyllable(chunk.to_owned()));
    }
    Ok(results)
}

/// Recursively enumerates segmentations from one character offset.
fn segment_from(
    letters: &[char],
    tones: &[Option<u8>],
    start: usize,
    current: &mut Vec<Syllable>,
    results: &mut Vec<Vec<Syllable>>,
) {
    if start == letters.len() {
        results.push(current.clone());
        return;
    }
    for end in start + 1..=letters.len() {
        let spelling = letters[start..end].iter().collect::<String>();
        if validate_syllable(&spelling).is_err() {
            continue;
        }
        let explicit_tones = tones[start..end]
            .iter()
            .flatten()
            .copied()
            .collect::<HashSet<_>>();
        if explicit_tones.len() > 1 {
            continue;
        }
        let tone = explicit_tones.into_iter().next().unwrap_or(5);
        current.push(Syllable {
            letters: spelling,
            tone,
        });
        segment_from(letters, tones, end, current, results);
        current.pop();
        if results.len() > 2_048 {
            return;
        }
    }
}

/// Checks spelling against the reviewed Hanyu Pinyin syllable vocabulary.
fn validate_syllable(value: &str) -> Result<(), PinyinError> {
    let lowercase = value.to_lowercase();
    if VALID_SYLLABLES.contains(lowercase.as_str()) {
        Ok(())
    } else {
        Err(PinyinError::InvalidSyllable(value.to_owned()))
    }
}

/// Decomposes one accepted marked-Pinyin character into its base and tone.
fn marked_character(character: char) -> Option<(char, Option<u8>)> {
    let result = match character {
        'ā' => ('a', Some(1)),
        'á' => ('a', Some(2)),
        'ǎ' => ('a', Some(3)),
        'à' => ('a', Some(4)),
        'Ā' => ('A', Some(1)),
        'Á' => ('A', Some(2)),
        'Ǎ' => ('A', Some(3)),
        'À' => ('A', Some(4)),
        'ē' => ('e', Some(1)),
        'é' => ('e', Some(2)),
        'ě' => ('e', Some(3)),
        'è' => ('e', Some(4)),
        'Ē' => ('E', Some(1)),
        'É' => ('E', Some(2)),
        'Ě' => ('E', Some(3)),
        'È' => ('E', Some(4)),
        'ī' => ('i', Some(1)),
        'í' => ('i', Some(2)),
        'ǐ' => ('i', Some(3)),
        'ì' => ('i', Some(4)),
        'Ī' => ('I', Some(1)),
        'Í' => ('I', Some(2)),
        'Ǐ' => ('I', Some(3)),
        'Ì' => ('I', Some(4)),
        'ō' => ('o', Some(1)),
        'ó' => ('o', Some(2)),
        'ǒ' => ('o', Some(3)),
        'ò' => ('o', Some(4)),
        'Ō' => ('O', Some(1)),
        'Ó' => ('O', Some(2)),
        'Ǒ' => ('O', Some(3)),
        'Ò' => ('O', Some(4)),
        'ū' => ('u', Some(1)),
        'ú' => ('u', Some(2)),
        'ǔ' => ('u', Some(3)),
        'ù' => ('u', Some(4)),
        'Ū' => ('U', Some(1)),
        'Ú' => ('U', Some(2)),
        'Ǔ' => ('U', Some(3)),
        'Ù' => ('U', Some(4)),
        'ǖ' => ('ü', Some(1)),
        'ǘ' => ('ü', Some(2)),
        'ǚ' => ('ü', Some(3)),
        'ǜ' => ('ü', Some(4)),
        'Ǖ' => ('Ü', Some(1)),
        'Ǘ' => ('Ü', Some(2)),
        'Ǚ' => ('Ü', Some(3)),
        'Ǜ' => ('Ü', Some(4)),
        'ü' | 'Ü' => (character, None),
        plain if plain.is_ascii_alphabetic() => (plain, None),
        _ => return None,
    };
    Some(result)
}

/// Builds the three canonical stored forms from validated syllables.
fn make_pinyin(syllables: Vec<Syllable>) -> Pinyin {
    let spaced_numbers = syllables
        .iter()
        .map(|syllable| format!("{}{}", syllable.letters, syllable.tone))
        .collect::<Vec<_>>()
        .join(" ");
    Pinyin {
        marks: prettify(&spaced_numbers),
        numbers: spaced_numbers.replace(' ', ""),
        tones: syllables.iter().map(|syllable| syllable.tone).collect(),
    }
}

/// Counts characters from the Unicode Han ideograph ranges used for segmentation.
pub fn han_character_count(value: &str) -> usize {
    value
        .chars()
        .filter(|character| {
            matches!(
                *character as u32,
                0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x323AF
            )
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_pinyin_is_canonical_and_case_sensitive() {
        let pinyin = from_numbered("fu4 Ming2").unwrap();
        assert_eq!(pinyin.numbers, "fu4Ming2");
        assert_eq!(pinyin.tones, vec![4, 2]);
        assert_eq!(from_numbered("nu:3").unwrap().numbers, "nü3");
        assert_eq!(from_numbered("nv3").unwrap().numbers, "nü3");
    }

    #[test]
    fn malformed_and_sentinel_readings_are_ineligible() {
        assert_eq!(from_numbered("xx5"), Err(PinyinError::NotApplicable));
        assert!(from_numbered("ma0").is_err());
        assert!(from_numbered("ma").is_err());
        assert!(from_numbered("notpinyin5").is_err());
    }

    #[test]
    fn marked_pinyin_uses_unique_segmentation_and_han_count() {
        let pinyin = from_marked("chéngpiān", Some(2)).unwrap();
        assert_eq!(pinyin.numbers, "cheng2pian1");
        assert_eq!(pinyin.tones, vec![2, 1]);
        assert_eq!(
            from_marked("xian", None),
            Err(PinyinError::AmbiguousSegmentation)
        );
        assert_eq!(from_marked("xian", Some(1)).unwrap().numbers, "xian5");
    }
}
