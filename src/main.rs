/*
 *  @author         :: Preston Wang-Stosur-Bassett <http://stosur.info>
 *  @date           :: Nov 17, 2017
 *  @description    :: This file takes a cc-cedict file and outputs it as a json file with some additions to support the Syng dictionary
*/

extern crate serde;
extern crate regex;

#[macro_use]
extern crate serde_json;

use std::fs::File;
use std::io::Read;
use std::path::Path;
use regex::Regex;

fn main() {
    let path = Path::new("cc-cedict/");
    let mut syng_cedict: Vec<serde_json::Value> = Vec::new();

    for entry in path.read_dir().expect("Could not read directory") {
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
            for line in contents.lines() {
                let starting_char = line.chars().nth(0);
                if starting_char != Some('#') {
                    // Line is not a comment, add the characters to the output json file
                    let word_contents = line.split(" ").collect::<Vec<_>>();
                    let raw_traditional = word_contents.get(0).unwrap();
                    let raw_simplified = word_contents.get(1).unwrap();
                    let pronunciation_start_bytes = line.find("[").unwrap();
                    let pronunciation_end_bytes = line.find("]").unwrap();
                    let raw_pronunciation = &line[pronunciation_start_bytes .. pronunciation_end_bytes];
                    let pronunciation = raw_pronunciation.split("[").collect::<String>();
                    let definitions_start_bytes = line.find("/").unwrap();
                    let raw_defintions = &line[definitions_start_bytes..];
                    let definitions: Vec<_> = raw_defintions.split("/").filter(|&x| x != "").collect::<Vec<_>>();
                    
                    // Process Pinyin
                    //let individual_words = raw_pronunciation.split(" ");

                    let mut tone_marks: Vec<i32> = Vec::new();
                    let re = Regex::new(r"\d").unwrap();

                    for elem in re.captures_iter(&pronunciation) {
                        let tone = elem[0].parse::<i32>().unwrap();
                        if tone == (5 as i32) || tone == (4 as i32) || tone == (4 as i32) || tone == (3 as i32) || tone == (2 as i32) || tone == (1 as i32) {
                            tone_marks.push(tone);
                        }
                    }

                    let pinyin_regex = Regex::new(r"\d|\s").unwrap();
                    let searchable_pinyin: String = pinyin_regex.split(&pronunciation).filter(|&x| x != "").collect::<String>().to_lowercase();

                    // Process English
                    let mut searchable_english: Vec<String> = Vec::new();

                    let english_regex = Regex::new(r"\d|\s|\p{P}").unwrap();
                    for english_word in &definitions {
                        let searchable_english_word: String = english_regex.split(&english_word).filter(|&x| x != "").collect::<String>().to_lowercase();
                        searchable_english.push(searchable_english_word);
                    }

                    // Create JSON Object
                    let word_object = json!({
                        "traditional": raw_traditional,
                        "simplified": raw_simplified,
                        "pronunciation": pronunciation,
                        "definitions": definitions,
                        "toneMarks": tone_marks,
                        "searchablePinyin": searchable_pinyin,
                        "searchableEnglish": searchable_english
                    });

                    syng_cedict.push(word_object);
                    println!("Added new word: {}", raw_simplified);
                } 
            }

            let buffer = File::create("cc-cedict.json").expect("There was a problem");
            serde_json::to_writer(buffer, &syng_cedict).unwrap();
        }
    }

    println!("Finished.");
}