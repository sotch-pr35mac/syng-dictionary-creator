// @author	::	Preston Wang-Stosur-Bassett <p.wanstobas@gmail.com>
// @created	::	October 6, 2020
// @description	::	This file builds a searchable Syng Dictionary file

use bincode::serialize_into;
use fst::Set;
use hsk::{HskLevel as LibraryHskLevel, HskSystem};
use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Write};
use std::sync::LazyLock;

static WHITESPACE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s").expect("whitespace regex should be valid"));
static TONE_OR_WHITESPACE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d|\s").expect("pinyin regex should be valid"));
static DIGIT_OR_PUNCTUATION_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d|\p{P}").expect("English search regex should be valid"));

#[derive(Hash, Serialize)]
pub struct MeasureWord {
    pub traditional: String,
    pub simplified: String,
    pub pinyin_marks: String,
    pub pinyin_numbers: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum HskLevel {
    One,
    Two,
    Three,
    Four,
    Five,
    Six,
    SevenToNine,
}

impl From<LibraryHskLevel> for HskLevel {
    fn from(level: LibraryHskLevel) -> Self {
        match level {
            LibraryHskLevel::One => Self::One,
            LibraryHskLevel::Two => Self::Two,
            LibraryHskLevel::Three => Self::Three,
            LibraryHskLevel::Four => Self::Four,
            LibraryHskLevel::Five => Self::Five,
            LibraryHskLevel::Six => Self::Six,
            LibraryHskLevel::SevenToNine => Self::SevenToNine,
        }
    }
}

#[derive(Default, Serialize)]
pub struct HskLevels {
    pub hsk_2015: Vec<HskLevel>,
    pub proficiency_standard_2021: Vec<HskLevel>,
    pub hsk_exam_syllabus_2025: Vec<HskLevel>,
}

impl HskLevels {
    pub fn from_matches(matches: Vec<(HskSystem, Vec<LibraryHskLevel>)>) -> Self {
        let mut levels = Self::default();

        for (system, system_levels) in matches {
            let target = match system {
                HskSystem::Hsk2015 => &mut levels.hsk_2015,
                HskSystem::ProficiencyStandard2021 => &mut levels.proficiency_standard_2021,
                HskSystem::HskExamSyllabus2025 => &mut levels.hsk_exam_syllabus_2025,
                _ => panic!("new HSK system requires a dictionary schema update"),
            };
            *target = system_levels.into_iter().map(HskLevel::from).collect();
        }

        levels
    }
}

#[derive(Serialize)]
pub struct WordEntry {
    pub traditional: String,
    pub simplified: String,
    pub pinyin_marks: String,
    pub pinyin_numbers: String,
    pub english: Vec<String>,
    pub tone_marks: Vec<u8>,
    pub hash: u64,
    pub measure_words: Vec<MeasureWord>,
    pub hsk: HskLevels,
    pub word_id: u32,
}

#[derive(Serialize)]
pub struct SyngDictionary {
    pub pinyin: HashMap<String, Vec<u32>>,
    pub english: HashMap<String, Vec<u32>>,
    pub simplified: HashMap<String, Vec<u32>>,
    pub traditional: HashMap<String, Vec<u32>>,
    pub data: HashMap<u32, WordEntry>,
}

pub fn calculate_hash<T: Hash>(t: &T) -> u64 {
    let mut s = DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}

fn build_searchable_pinyin(marks: &str, numbers: &str) -> Vec<String> {
    vec![
        WHITESPACE_REGEX
            .split(marks)
            .filter(|&x| !x.is_empty())
            .collect::<String>()
            .to_lowercase(),
        WHITESPACE_REGEX
            .split(numbers)
            .filter(|&x| !x.is_empty())
            .collect::<String>()
            .to_lowercase(),
        TONE_OR_WHITESPACE_REGEX
            .split(numbers)
            .filter(|&x| !x.is_empty())
            .collect::<String>()
            .to_lowercase(),
    ]
}

fn build_searchable_english(english: &[String]) -> Vec<String> {
    let mut searchable = Vec::new();

    for term in english {
        let mut formatted = term.clone();

        // If tehre are paranthesis, remove them and everything in-between
        if term.contains('(') && term.contains(')') {
            let open_paren = term.chars().position(|c| c == '(').unwrap();
            let close_paren = term.chars().position(|c| c == ')').unwrap();

            // Remove trailing spaces
            let mut start = term.chars().take(open_paren).collect::<String>();
            let mut end = term
                .chars()
                .skip(close_paren + 1)
                .take(term.chars().count())
                .collect::<String>();

            if !start.is_empty() {
                start = start
                    .chars()
                    .take(start.chars().count() - 1)
                    .collect::<String>();
            } else if !end.is_empty() {
                end = end.chars().skip(1).collect::<String>();
            }

            formatted = format!("{start}{end}");
        }

        // Remove any punctuation if there is any and make all characters lower case
        formatted = DIGIT_OR_PUNCTUATION_REGEX
            .split(&formatted)
            .filter(|&x| !x.is_empty())
            .collect::<String>()
            .to_lowercase();

        searchable.push(formatted.replace(' ', "%20"));

        // Remove "to" at the beginning of verbs
        if formatted.starts_with("to ") {
            let formatted_verb: String = formatted.chars().skip(3).collect();
            searchable.push(formatted_verb.replace(' ', "%20"));
        }
    }

    searchable
}

pub fn build_dictionary(word_list: Vec<WordEntry>) -> SyngDictionary {
    let mut dictionary = SyngDictionary {
        pinyin: HashMap::new(),
        english: HashMap::new(),
        simplified: HashMap::new(),
        traditional: HashMap::new(),
        data: HashMap::new(),
    };

    for (id, mut entry) in (0_u32..).zip(word_list) {
        dictionary
            .traditional
            .entry(entry.traditional.clone())
            .or_default()
            .push(id);
        dictionary
            .simplified
            .entry(entry.simplified.clone())
            .or_default()
            .push(id);
        let searchable_english = build_searchable_english(&entry.english);
        let searchable_pinyin = build_searchable_pinyin(&entry.pinyin_marks, &entry.pinyin_numbers);

        for term in searchable_english {
            dictionary.english.entry(term).or_default().push(id);
        }
        for term in searchable_pinyin {
            dictionary.pinyin.entry(term).or_default().push(id);
        }

        entry.word_id = id;
        dictionary.data.insert(id, entry);
    }

    dictionary
}

fn write_searchable_file(component: &HashMap<String, Vec<u32>>, name: &str) {
    println!("\nWriting {name} to file...");
    let mut data_file = BufWriter::new(File::create(format!("./out/{name}.dictionary")).unwrap());
    serialize_into(&mut data_file, &component).unwrap();
    println!("Written.");
}

fn write_data_file(dictionary: &SyngDictionary) {
    println!("\nWriting dictionary data to file...");
    let mut data_file = BufWriter::new(File::create("./out/data.dictionary").unwrap());
    serialize_into(&mut data_file, &dictionary.data).unwrap();
    println!("Written.");
}

fn build_chinese_fst(dictionary: &SyngDictionary) -> Set<Vec<u8>> {
    let mut keys: Vec<&str> = dictionary
        .simplified
        .keys()
        .chain(dictionary.traditional.keys())
        .map(String::as_str)
        .collect();

    keys.sort_unstable();
    keys.dedup();

    Set::from_iter(keys).expect("Failed to build Chinese tokenizer FST")
}

fn write_chinese_fst_file(dictionary: &SyngDictionary) {
    println!("\nWriting Chinese tokenizer FST to file...");
    let set = build_chinese_fst(dictionary);
    let mut data_file = BufWriter::new(File::create("./out/chinese.fst").unwrap());
    data_file.write_all(set.as_fst().as_bytes()).unwrap();
    data_file.flush().unwrap();
    println!("Written.");
}

pub fn write_dictionary_files(dictionary: &SyngDictionary) {
    write_searchable_file(&dictionary.pinyin, "pinyin");
    write_searchable_file(&dictionary.english, "english");
    write_searchable_file(&dictionary.traditional, "traditional");
    write_searchable_file(&dictionary.simplified, "simplified");
    write_chinese_fst_file(dictionary);
    write_data_file(dictionary);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn empty_dictionary() -> SyngDictionary {
        SyngDictionary {
            pinyin: HashMap::new(),
            english: HashMap::new(),
            simplified: HashMap::new(),
            traditional: HashMap::new(),
            data: HashMap::new(),
        }
    }

    fn insert_keys(map: &mut HashMap<String, Vec<u32>>, keys: &[&str]) {
        for (id, key) in keys.iter().enumerate() {
            map.insert((*key).to_string(), vec![id as u32]);
        }
    }

    fn word_entry(traditional: &str, simplified: &str, english: Vec<&str>) -> WordEntry {
        WordEntry {
            traditional: traditional.to_string(),
            simplified: simplified.to_string(),
            pinyin_marks: "nǐ hǎo".to_string(),
            pinyin_numbers: "ni3 hao3".to_string(),
            english: english.into_iter().map(str::to_string).collect(),
            tone_marks: vec![3, 3],
            hash: 42,
            measure_words: Vec::new(),
            hsk: HskLevels {
                hsk_2015: vec![HskLevel::One],
                ..HskLevels::default()
            },
            word_id: 99,
        }
    }

    fn append_string(bytes: &mut Vec<u8>, value: &str) {
        bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
        bytes.extend_from_slice(value.as_bytes());
    }

    #[test]
    fn builds_all_existing_pinyin_search_forms() {
        assert_eq!(
            build_searchable_pinyin("Nǐ Hǎo", "Ni3 Hao3"),
            vec!["nǐhǎo", "ni3hao3", "nihao"]
        );
    }

    #[test]
    fn normalizes_english_search_terms_with_existing_rules() {
        let english = vec![
            "to look (at)".to_string(),
            "3-D printer!".to_string(),
            "(dialect) hello".to_string(),
        ];

        assert_eq!(
            build_searchable_english(&english),
            vec!["to%20look", "look", "d%20printer", "hello"]
        );
    }

    #[test]
    fn builds_duplicate_indexes_and_reassigns_word_ids() {
        let dictionary = build_dictionary(vec![
            word_entry("你好", "你好", vec!["hello"]),
            word_entry("你好", "你好", vec!["hello"]),
        ]);

        assert_eq!(dictionary.traditional.get("你好"), Some(&vec![0, 1]));
        assert_eq!(dictionary.simplified.get("你好"), Some(&vec![0, 1]));
        assert_eq!(dictionary.english.get("hello"), Some(&vec![0, 1]));
        assert_eq!(dictionary.pinyin.get("ni3hao3"), Some(&vec![0, 1]));
        assert_eq!(dictionary.data.get(&0).unwrap().word_id, 0);
        assert_eq!(dictionary.data.get(&1).unwrap().word_id, 1);
    }

    #[test]
    fn preserves_the_shared_advanced_hsk_band() {
        let levels = HskLevels::from_matches(vec![(
            HskSystem::HskExamSyllabus2025,
            vec![LibraryHskLevel::SevenToNine],
        )]);

        assert_eq!(levels.hsk_exam_syllabus_2025, vec![HskLevel::SevenToNine]);
    }

    #[test]
    fn builds_chinese_fst_from_the_distinct_union_of_searchable_keys() {
        let simplified = ["以后", "用于", "万", "舍不得", "今天"];
        let traditional = ["以後", "用於", "萬", "捨不得", "今天", "万"];
        let mut dictionary = empty_dictionary();
        insert_keys(&mut dictionary.simplified, &simplified);
        insert_keys(&mut dictionary.traditional, &traditional);

        let expected: HashSet<&str> = simplified
            .iter()
            .chain(traditional.iter())
            .copied()
            .collect();
        let set = build_chinese_fst(&dictionary);

        for key in &expected {
            assert!(set.contains(key));
        }
        assert_eq!(set.len(), expected.len());
        assert!(!set.contains("明天"));

        let stored_keys = set.stream().into_strs().unwrap();
        assert_eq!(
            stored_keys
                .iter()
                .filter(|key| key.as_str() == "今天")
                .count(),
            1
        );
        assert_eq!(
            stored_keys
                .iter()
                .filter(|key| key.as_str() == "万")
                .count(),
            1
        );
    }

    #[test]
    fn builds_chinese_fst_deterministically_across_map_insertion_orders() {
        let mut first = empty_dictionary();
        insert_keys(
            &mut first.simplified,
            &["以后", "用于", "万", "舍不得", "今天"],
        );
        insert_keys(
            &mut first.traditional,
            &["以後", "用於", "萬", "捨不得", "今天", "万"],
        );

        let mut second = empty_dictionary();
        insert_keys(
            &mut second.simplified,
            &["今天", "舍不得", "万", "用于", "以后"],
        );
        insert_keys(
            &mut second.traditional,
            &["万", "今天", "捨不得", "萬", "用於", "以後"],
        );

        let first = build_chinese_fst(&first);
        let second = build_chinese_fst(&second);

        assert_eq!(first.as_fst().as_bytes(), second.as_fst().as_bytes());
    }

    #[test]
    fn preserves_word_entry_bincode_layout() {
        let entry = WordEntry {
            traditional: "T".to_string(),
            simplified: "S".to_string(),
            pinyin_marks: "M".to_string(),
            pinyin_numbers: "N".to_string(),
            english: vec!["E".to_string()],
            tone_marks: vec![1, 5],
            hash: 0x0102_0304_0506_0708,
            measure_words: vec![MeasureWord {
                traditional: "A".to_string(),
                simplified: "B".to_string(),
                pinyin_marks: "C".to_string(),
                pinyin_numbers: "D".to_string(),
            }],
            hsk: HskLevels {
                hsk_2015: vec![HskLevel::Six],
                ..HskLevels::default()
            },
            word_id: 0x0a0b_0c0d,
        };

        let mut expected = Vec::new();
        append_string(&mut expected, "T");
        append_string(&mut expected, "S");
        append_string(&mut expected, "M");
        append_string(&mut expected, "N");
        expected.extend_from_slice(&1_u64.to_le_bytes());
        append_string(&mut expected, "E");
        expected.extend_from_slice(&2_u64.to_le_bytes());
        expected.extend_from_slice(&[1, 5]);
        expected.extend_from_slice(&0x0102_0304_0506_0708_u64.to_le_bytes());
        expected.extend_from_slice(&1_u64.to_le_bytes());
        append_string(&mut expected, "A");
        append_string(&mut expected, "B");
        append_string(&mut expected, "C");
        append_string(&mut expected, "D");
        expected.extend_from_slice(&bincode::serialize(&entry.hsk).unwrap());
        expected.extend_from_slice(&0x0a0b_0c0d_u32.to_le_bytes());

        assert_eq!(bincode::serialize(&entry).unwrap(), expected);
    }
}
