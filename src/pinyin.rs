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
     liang liao lie lin ling liu lo long lou lu lü luan lüan lue lüe lun luo m ma mai man \
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

/// Returns whether a source character separates pronunciation syllables.
fn is_separator(character: char) -> bool {
    character.is_whitespace() || matches!(character, '-' | '\'' | '’' | ',' | '·')
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
        } else if is_separator(character) {
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
        .split(is_separator)
        .filter(|chunk| !chunk.is_empty())
        .collect::<Vec<_>>();
    if chunks.is_empty() {
        return Err(PinyinError::Empty);
    }
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
    for character in chunk.nfd() {
        if character.is_ascii_alphabetic() {
            letters.push(character);
            tones.push(None);
            continue;
        }

        let last_letter = letters.last_mut().ok_or(PinyinError::InvalidCharacter)?;
        let last_tone = tones.last_mut().expect("letters and tones remain aligned");
        if character == '\u{0308}' {
            *last_letter = match *last_letter {
                'u' => 'ü',
                'U' => 'Ü',
                _ => return Err(PinyinError::InvalidCharacter),
            };
            continue;
        }
        let tone = match character {
            '\u{0304}' => 1,
            '\u{0301}' => 2,
            '\u{030c}' => 3,
            '\u{0300}' => 4,
            _ => return Err(PinyinError::InvalidCharacter),
        };
        if !matches!(
            *last_letter,
            'a' | 'A'
                | 'e'
                | 'E'
                | 'i'
                | 'I'
                | 'o'
                | 'O'
                | 'u'
                | 'U'
                | 'ü'
                | 'Ü'
                | 'm'
                | 'M'
                | 'n'
                | 'N'
        ) || last_tone.replace(tone).is_some()
        {
            return Err(PinyinError::InvalidCharacter);
        }
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
            .collect::<Vec<_>>();
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

/// Builds the three canonical stored forms from validated syllables.
fn make_pinyin(syllables: Vec<Syllable>) -> Pinyin {
    let spaced_numbers = syllables
        .iter()
        .map(|syllable| format!("{}{}", syllable.letters, syllable.tone))
        .collect::<Vec<_>>()
        .join(" ");
    let marks = syllables
        .iter()
        .map(render_syllable)
        .collect::<Vec<_>>()
        .join(" ");
    Pinyin {
        marks,
        numbers: spaced_numbers.replace(' ', ""),
        tones: syllables.iter().map(|syllable| syllable.tone).collect(),
    }
}

/// Renders a syllable, including tones on syllabic nasals without vowels.
fn render_syllable(syllable: &Syllable) -> String {
    if syllable.letters.chars().any(|character| {
        matches!(
            character,
            'a' | 'A' | 'e' | 'E' | 'i' | 'I' | 'o' | 'O' | 'u' | 'U' | 'ü' | 'Ü'
        )
    }) {
        return prettify(&format!("{}{}", syllable.letters, syllable.tone));
    }
    if syllable.tone == 5 {
        return syllable.letters.clone();
    }
    let mark = match syllable.tone {
        1 => '\u{0304}',
        2 => '\u{0301}',
        3 => '\u{030c}',
        4 => '\u{0300}',
        _ => unreachable!("validated tone"),
    };
    let target = if syllable.letters.contains(['m', 'M']) {
        ['m', 'M']
    } else {
        ['n', 'N']
    };
    let mut rendered = String::new();
    let mut marked = false;
    for character in syllable.letters.chars() {
        rendered.push(character);
        if !marked && target.contains(&character) {
            rendered.push(mark);
            marked = true;
        }
    }
    rendered.nfc().collect()
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
    fn approved_boundaries_are_omitted_from_numbered_pinyin() {
        let idiom = from_numbered("yi1 bu4 zuo4, er4 bu4 xiu1").unwrap();
        assert_eq!(idiom.numbers, "yi1bu4zuo4er4bu4xiu1");
        assert_eq!(idiom.tones, vec![1, 4, 4, 4, 4, 1]);

        let name = from_numbered("Ya4 dang1 · Si1 mi4").unwrap();
        assert_eq!(name.numbers, "Ya4dang1Si1mi4");
        assert_eq!(name.tones, vec![4, 1, 1, 4]);
        assert_eq!(from_numbered("yi1 bu, er4"), Err(PinyinError::MissingTone));
    }

    #[test]
    fn syllabic_nasals_round_trip_with_tones() {
        let expected_marks = [(1, "m̄"), (2, "ḿ"), (3, "m̌"), (4, "m̀")];
        for (tone, marks) in expected_marks {
            let numbered = from_numbered(&format!("m{tone}")).unwrap();
            assert_eq!(numbered.marks, marks);
            assert_eq!(from_marked(marks, Some(1)).unwrap(), numbered);
        }

        for spelling in ["n", "ng", "hm", "hng"] {
            for tone in 1..=5 {
                let numbered = from_numbered(&format!("{spelling}{tone}")).unwrap();
                assert_eq!(from_marked(&numbered.marks, Some(1)).unwrap(), numbered);
            }
        }
        assert_eq!(from_marked("ńg", Some(1)).unwrap().numbers, "ng2");
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

    #[test]
    fn marked_pinyin_accepts_shared_boundaries_and_rejects_extra_tone_marks() {
        let pinyin = from_marked("yī bù zuò, èr bù xiū", Some(6)).unwrap();
        assert_eq!(pinyin.numbers, "yi1bu4zuo4er4bu4xiu1");
        assert!(from_marked("m\u{0301}\u{0301}", Some(1)).is_err());
        assert!(from_marked("m\u{0301}\u{0300}", Some(1)).is_err());
    }

    #[test]
    fn marked_pinyin_rejects_separator_only_input_as_empty() {
        for value in ["-", ",", "'", "’", "·", "-,' ’·"] {
            assert_eq!(from_marked(value, None), Err(PinyinError::Empty));
        }
    }
}
