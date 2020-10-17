/*
 *	@author			::	Preston Wang-Stosur-Bassett <p.wanstobas@gmail.com>
 *	@created		::	October 6, 2020
 *	@description	::	This file parses cedict file
 */

#[path = "./dictionary_utils.rs"] mod dictionary_utils;

use dictionary_utils::calculate_hash;
use dictionary_utils::MeasureWord;
use dictionary_utils::WordEntry;
use hsk::Hsk;
use prettify_pinyin::prettify;
use std::fs::File;
use std::hash::{Hash};
use std::io::Read;
use std::path::Path;
use regex::Regex;

#[derive(Hash)]
struct CEDictEntry {
	traditional: String,
	simplified: String,
	pinyin: String,
	english: Vec<String>,
	measure_words: Vec<MeasureWord>
}

fn get_tone_marks(pinyin: &str) -> Vec<u8> {
	let mut tone_marks: Vec<u8> = Vec::new();
	let re = Regex::new(r"\d").unwrap();
	
	for elem in re.captures_iter(pinyin) {
		let tone = elem[0].parse::<u8>().unwrap();
		if tone == (5 as u8) || tone == (4 as u8) || tone == (3 as u8) || tone == (2 as u8) || tone == (1 as u8) {
			tone_marks.push(tone);	
		}
	}
	
	return tone_marks;
}

fn is_measure_word(line: &str) -> bool {
	return line.to_string().chars().skip(0).take(3).collect::<String>() == String::from("CL:");	
}

fn prune_measure_words(english: Vec<String>) -> Vec<String> {
	return english.iter().filter(|&x| !is_measure_word(x)).map(|x| x.to_string()).collect::<Vec<String>>();	
}

fn process_cedict_entry(raw_line: &str) -> CEDictEntry {
	let line = String::from(raw_line);
	
	let contents = line.split(" ").collect::<Vec<_>>();
	let traditional = contents.get(0).unwrap();
	let simplified = contents.get(1).unwrap();
	let pinyin_start = line.find("[").unwrap();
	let pinyin_end = line.find("]").unwrap();
	let pinyin = &line[pinyin_start .. pinyin_end].split("[").collect::<String>();
	let english_start = line.find("/").unwrap();
	let raw_english = &line[english_start..];
	let english: Vec<String> = raw_english.split("/").filter(|&x| x != "").map(|x| x.to_string()).collect::<Vec<String>>();
	
	let mut measure_words: Vec<MeasureWord> = Vec::new();
	for item in &english {
		if is_measure_word(item) {
			let mw_content: String = item.to_string().chars().skip(3).collect::<String>();
			for word in mw_content.split(",") {
				let mw_pinyin_start = word.find("[").unwrap();
				let mw_pinyin_end = word.find("]").unwrap();
				let mw_pinyin_raw = (&word[mw_pinyin_start .. mw_pinyin_end]).to_string();
				let mw_pinyin = mw_pinyin_raw.chars().skip(1).collect::<String>();
				
				let mw_traditional: String;
				let mw_simplified: String;
				
				if word.contains("|") {
					// Measure word has different traditional and simplified representations
					let word_vec: Vec<&str> = word.split("|").collect::<Vec<&str>>();
					mw_traditional = word_vec.get(0).unwrap().to_string();
					let raw_simplified: String = word_vec.get(1).unwrap().to_string();
					let mw_simplified_end = raw_simplified.find("[").unwrap();
					mw_simplified = (&raw_simplified[.. mw_simplified_end]).to_string();
					
				} else {
					// Measure word has same traditional and simplified representations
					let character_end = word.find("[").unwrap();
					mw_simplified = (&word[.. character_end]).to_string();
					mw_traditional = (&word[.. character_end]).to_string();
				}
				
				let measure_word: MeasureWord = MeasureWord {
					traditional: mw_traditional,
					simplified: mw_simplified,
					pinyin_numbers: (&mw_pinyin).to_string(),
					pinyin_marks: prettify((&mw_pinyin).to_string())
				};
				
				measure_words.push(measure_word);
			}	
		}
	}
	
	let entry: CEDictEntry = CEDictEntry {
		traditional: traditional.to_string(),
		simplified: simplified.to_string(),
		pinyin: pinyin.to_string(),
		english: prune_measure_words(english),
		measure_words: measure_words
	};
	
	return entry;
}

pub fn get_cedict_data() -> Vec<WordEntry> {
	let path = Path::new("cc-cedict/");
	let hsk_list = Hsk::new();
	let mut syng_dict: Vec<WordEntry> = Vec::new();
	
	for entry in path.read_dir().expect("Could not read directory.") {
		if let Ok(entry) = entry {
			let mut file = match File::open(entry.path()) {
				Ok(file) => file,
				Err(e) => {
					panic!("{}", e);	
				}
			};
			
			println!("Now loading in file: {:?}", entry.path());
			
			let mut contents = String::new();
			file.read_to_string(&mut contents).expect("Failed to read file.");
			
			println!("Processing...");
			let mut id: u32 = 0;
			for line in contents.lines() {
				if line.chars().nth(0) != Some('#') {
					let cedict_entry: CEDictEntry = process_cedict_entry(&line);
					let tone_marks: Vec<u8> = get_tone_marks(&cedict_entry.pinyin);
					let hsk_level: u8 = hsk_list.get_hsk(&cedict_entry.simplified);
					let new_entry: WordEntry = WordEntry {
						hash: calculate_hash(&cedict_entry),
						traditional: cedict_entry.traditional,
						simplified: cedict_entry.simplified,
						pinyin_numbers: (&cedict_entry.pinyin).to_string(),
						pinyin_marks: prettify((&cedict_entry.pinyin).to_string()),
						english: cedict_entry.english,
						measure_words: cedict_entry.measure_words,
						word_id: id,
						hsk: hsk_level,
						tone_marks: tone_marks
					};
					
					syng_dict.push(new_entry);
					id = id + 1;
				}	
			}
		}	
	}
	
	return syng_dict;
}