// @author	::	Preston Wang-Stosur-Bassett <p.wanstobas@gmail.com>
// @created	::	October 6, 2020
// @description	::	This file parses cedict file

use crate::dictionary_utils::{MeasureWord, WordEntry, calculate_hash};
use hsk::Hsk;
use prettify_pinyin::prettify;
use regex::Regex;
use std::fs::File;
use std::hash::Hash;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

static TONE_NUMBER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d").expect("tone-number regex should be valid"));

#[derive(Hash)]
struct CedictEntry {
    traditional: String,
    simplified: String,
    pinyin: String,
    english: Vec<String>,
    measure_words: Vec<MeasureWord>,
}

fn get_tone_marks(pinyin: &str) -> Vec<u8> {
    TONE_NUMBER_REGEX
        .captures_iter(pinyin)
        .map(|capture| capture[0].parse::<u8>().unwrap())
        .filter(|tone| (1..=5).contains(tone))
        .collect()
}

fn is_measure_word(line: &str) -> bool {
    line.starts_with("CL:")
}

fn prune_measure_words(english: Vec<String>) -> Vec<String> {
    english
        .into_iter()
        .filter(|definition| !is_measure_word(definition))
        .collect()
}

fn process_cedict_entry(line: &str) -> CedictEntry {
    let contents = line.split(' ').collect::<Vec<_>>();
    let traditional = contents.first().unwrap();
    let simplified = contents.get(1).unwrap();
    let pinyin_start = line.find('[').unwrap();
    let pinyin_end = line.find(']').unwrap();
    let pinyin = &line[pinyin_start + 1..pinyin_end];
    let english_start = line.find('/').unwrap();
    let raw_english = &line[english_start..];
    let english: Vec<String> = raw_english
        .split('/')
        .filter(|&x| !x.is_empty())
        .map(str::to_owned)
        .collect();

    let mut measure_words = Vec::new();
    for item in &english {
        if is_measure_word(item) {
            let mw_content: String = item.chars().skip(3).collect();
            for word in mw_content.split(',') {
                let mw_pinyin_start = word.find('[').unwrap();
                let mw_pinyin_end = word.find(']').unwrap();
                let mw_pinyin = &word[mw_pinyin_start + 1..mw_pinyin_end];

                let mw_traditional: String;
                let mw_simplified: String;

                if word.contains('|') {
                    // Measure word has different traditional and simplified representations
                    let word_vec: Vec<&str> = word.split('|').collect();
                    mw_traditional = word_vec.first().unwrap().to_string();
                    let raw_simplified = word_vec.get(1).unwrap();
                    let mw_simplified_end = raw_simplified.find('[').unwrap();
                    mw_simplified = (raw_simplified[..mw_simplified_end]).to_string();
                } else {
                    // Measure word has same traditional and simplified representations
                    let character_end = word.find('[').unwrap();
                    mw_simplified = (word[..character_end]).to_string();
                    mw_traditional = (word[..character_end]).to_string();
                }

                let measure_word = MeasureWord {
                    traditional: mw_traditional,
                    simplified: mw_simplified,
                    pinyin_numbers: mw_pinyin.to_string(),
                    pinyin_marks: prettify(mw_pinyin),
                };

                measure_words.push(measure_word);
            }
        }
    }

    CedictEntry {
        traditional: traditional.to_string(),
        simplified: simplified.to_string(),
        pinyin: pinyin.to_string(),
        english: prune_measure_words(english),
        measure_words,
    }
}

pub fn get_cedict_data() -> Vec<WordEntry> {
    let path = Path::new("cc-cedict/");
    let hsk_list = Hsk::new();
    let mut syng_dict = Vec::new();

    for entry in path
        .read_dir()
        .expect("Could not read directory.")
        .flatten()
    {
        let mut file = match File::open(entry.path()) {
            Ok(file) => file,
            Err(e) => {
                panic!("{}", e);
            }
        };

        println!("Now loading in file: {:?}", entry.path());

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .expect("Failed to read file.");

        println!("Processing...");
        let mut id: u32 = 0;
        for line in contents.lines() {
            if !line.starts_with('#') {
                let cedict_entry = process_cedict_entry(line);
                let tone_marks = get_tone_marks(&cedict_entry.pinyin);
                let hsk_level = hsk_list.get_hsk(&cedict_entry.simplified);
                let new_entry = WordEntry {
                    hash: calculate_hash(&cedict_entry),
                    traditional: cedict_entry.traditional,
                    simplified: cedict_entry.simplified,
                    pinyin_numbers: cedict_entry.pinyin.to_string(),
                    pinyin_marks: prettify(&cedict_entry.pinyin),
                    english: cedict_entry.english,
                    measure_words: cedict_entry.measure_words,
                    word_id: id,
                    hsk: hsk_level,
                    tone_marks,
                };

                syng_dict.push(new_entry);
                id += 1;
            }
        }
    }

    syng_dict
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_supported_tone_numbers_in_order() {
        assert_eq!(
            get_tone_marks("ma1 ma2 ma3 ma4 ma5 ma0 ma6"),
            vec![1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn recognizes_only_measure_word_definitions_at_the_start() {
        assert!(is_measure_word("CL:個|个[ge4]"));
        assert!(!is_measure_word("see CL:個|个[ge4]"));
        assert!(!is_measure_word("CL"));
    }

    #[test]
    fn parses_and_prunes_traditional_and_simplified_measure_words() {
        let entry = process_cedict_entry("丈夫 丈夫 [zhang4 fu5] /husband/CL:個|个[ge4],位[wei4]/");

        assert_eq!(entry.traditional, "丈夫");
        assert_eq!(entry.simplified, "丈夫");
        assert_eq!(entry.pinyin, "zhang4 fu5");
        assert_eq!(entry.english, vec!["husband"]);
        assert_eq!(entry.measure_words.len(), 2);

        let first = &entry.measure_words[0];
        assert_eq!(first.traditional, "個");
        assert_eq!(first.simplified, "个");
        assert_eq!(first.pinyin_numbers, "ge4");
        assert_eq!(first.pinyin_marks, "gè");

        let second = &entry.measure_words[1];
        assert_eq!(second.traditional, "位");
        assert_eq!(second.simplified, "位");
        assert_eq!(second.pinyin_numbers, "wei4");
        assert_eq!(second.pinyin_marks, "wèi");
    }

    #[test]
    fn keeps_the_existing_entry_hash_stable() {
        let entry = process_cedict_entry("丈夫 丈夫 [zhang4 fu5] /husband/CL:個|个[ge4]/");

        assert_eq!(calculate_hash(&entry), 1_067_393_186_030_521_090);
    }
}
