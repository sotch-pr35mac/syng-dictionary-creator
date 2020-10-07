/*
 *	@author			::	Preston Wang-Stosur-Bassett <p.wanstobas@gmail.com>
 *	@created		::	October 6, 2020
 *	@description	::	This file builds a searchable Syng Dictionary file
 */

use bincode::serialize_into;
use serde_derive::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::BufWriter;
use regex::Regex;

#[derive(Hash, Serialize)]
pub struct MeasureWord {
	pub traditional: String,
	pub simplified: String,
	pub pinyin_marks: String,
	pub pinyin_numbers: String	
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
	pub hsk: u8,
	pub word_id: u32
}

#[derive(Serialize)]
pub struct SyngDictionary {
	pub pinyin: HashMap<String, Vec<u32>>,
	pub english: HashMap<String, Vec<u32>>,
	pub simplified: HashMap<String, Vec<u32>>,
	pub traditional: HashMap<String, Vec<u32>>,
	pub data: HashMap<u32, WordEntry>
}

pub fn calculate_hash<T: Hash>(t: &T) -> u64 {
	let mut s = DefaultHasher::new();
	t.hash(&mut s);
	s.finish()
}

fn build_searchable_pinyin(marks: String, numbers: String) -> Vec<String> {
	let mut searchable: Vec<String> = Vec::new();
	let regex_1 = Regex::new(r"\s").unwrap();
	let regex_2 = Regex::new(r"\d|\s").unwrap();

	searchable.push(regex_1.split(&marks).filter(|&x| x != "").collect::<String>().to_lowercase());
	searchable.push(regex_1.split(&numbers).filter(|&x| x != "").collect::<String>().to_lowercase());
	searchable.push(regex_2.split(&numbers).filter(|&x| x != "").collect::<String>().to_lowercase());
	
	return searchable;
}

fn build_searchable_english(english: &Vec<String>) -> Vec<String> {
	let mut searchable: Vec<String> = Vec::new();
	
	for term in english {
		let mut formatted: String = (&term).to_string();
		
		// If tehre are paranthesis, remove them and everything in-between
		if term.contains("(") && term.contains(")") {
			let open_paren = term.chars().position(|c| c == '(').unwrap();
			let close_paren = term.chars().position(|c| c == ')').unwrap();
		
			// Remove trailing spaces	
			let mut start = term.chars().skip(0).take(open_paren).collect::<String>();
			let mut end = term.chars().skip(close_paren + 1).take(term.chars().count()).collect::<String>();
			
			if start.chars().count() > 0 {
				start = start.chars().take(start.chars().count() - 1).collect::<String>();
			} else if end.chars().count() > 0 {
				end = end.chars().skip(1).collect::<String>();
			}
			
			formatted = format!("{}{}", start, end);	
		}
		
		// Remove any punctuation if there is any and make all characters lower case
		let regex = Regex::new(r"\d|\p{P}").unwrap();
		formatted = regex.split(&formatted).filter(|&x| x != "").collect::<String>().to_lowercase();
		
		searchable.push(formatted.replace(" ", "%20"));
		
		// Remove "to" at the beginning of verbs
		if formatted.chars().take(3).collect::<String>() == String::from("to ") {
			let formatted_verb: String = formatted.chars().skip(3).collect::<String>();
			searchable.push(formatted_verb.replace(" ", "%20"));	
		}
	}
	
	return searchable;
}

pub fn build_dictionary(word_list: Vec<WordEntry>) -> SyngDictionary {
	let mut dictionary: SyngDictionary = SyngDictionary {
		pinyin: HashMap::new(),
		english: HashMap::new(),
		simplified: HashMap::new(),
		traditional: HashMap::new(),
		data: HashMap::new()	
	};
	
	let mut id: u32 = 0;
	for mut entry in word_list {
		dictionary.traditional.entry((&entry.traditional).to_string()).or_insert(Vec::new()).push(id);
		dictionary.simplified.entry((&entry.simplified).to_string()).or_insert(Vec::new()).push(id);
		let searchable_english: Vec<String> = build_searchable_english(&entry.english);
		let searchable_pinyin: Vec<String> = build_searchable_pinyin((&entry.pinyin_marks).to_string(), (&entry.pinyin_numbers).to_string());
		
		for term in searchable_english {
			dictionary.english.entry(term).or_insert(Vec::new()).push(id);
		}
		for term in searchable_pinyin {
			dictionary.pinyin.entry(term).or_insert(Vec::new()).push(id);	
		}
		
		entry.word_id = id;
		dictionary.data.insert(id, entry);
		id += 1;
	}
	
	return dictionary;
}

fn write_searchable_file(component: &HashMap<String, Vec<u32>>, name: &str) {
	println!("\nWriting {} to file...", name);
	let mut data_file = BufWriter::new(File::create(format!("./out/{}.dictionary", name)).unwrap());
	serialize_into(&mut data_file, &component).unwrap();
	println!("Written.");	
}

fn write_data_file(dictionary: &SyngDictionary) {
	println!("\nWriting dictionary data to file...");
	let mut data_file = BufWriter::new(File::create("./out/data.dictionary").unwrap());
	serialize_into(&mut data_file, &dictionary.data).unwrap();
	println!("Written.");	
}

pub fn write_dictionary_files(dictionary: &SyngDictionary) {
	write_searchable_file(&dictionary.pinyin, "pinyin");
	write_searchable_file(&dictionary.english, "english");
	write_searchable_file(&dictionary.traditional, "traditional");
	write_searchable_file(&dictionary.simplified, "simplified");
	write_data_file(&dictionary);	
}