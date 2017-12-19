/*
 *  @author         :: Preston Wang-Stosur-Bassett <http://stosur.info>
 *  @date           :: Nov 17, 2017
 *  @description    :: This file takes a cc-cedict file and outputs it as a json file with some additions to support the Syng dictionary
*/

extern crate serde;
extern crate regex;
extern crate prettify_pinyin;

#[macro_use]
extern crate serde_json;

use std::fs::File;
use std::io::Read;
use std::path::Path;
use regex::Regex;
use prettify_pinyin::prettify;

fn get_searchable_definitions(definitions: &Vec<&str>) -> Vec<String> {
    let mut searchable: Vec<String> = Vec::new();

    for def in definitions {
        let mut formatted: String = def.to_string();

        // If there are paranthesis, remove them and everything in-between them
        if def.contains("(") && def.contains(")") {
            let open_paren_index = def.chars().position(|c| c == '(').unwrap();
            let close_paren_index = def.chars().position(|c| c == ')').unwrap();

            let mut start = def.chars().skip(0).take(open_paren_index).collect::<String>();
            let mut end = def.chars().skip(close_paren_index + 1).take(def.chars().count()).collect::<String>();

            // Remove trailing spaces
            if start.chars().count() > 0 {
                start = start.chars().take(start.chars().count() - 1).collect::<String>();
            } else if end.chars().count() > 0 {
                end = end.chars().skip(1).collect::<String>();
            }

            formatted = format!("{}{}", start, end);
        }

        // Remove any punctuation if there is any and make all characters lower case
        let english_regex = Regex::new(r"\d|\p{P}").unwrap();
        formatted = english_regex.split(&formatted).filter(|&x| x != "").collect::<String>().to_lowercase();

        searchable.push(formatted.replace(" ", "%20"));

        // Remove "to" at the beginning of verbs
        if formatted.chars().take(3).collect::<String>() == String::from("to ") {
            let formatted_verb: String = formatted.chars().skip(3).collect::<String>();
            searchable.push(formatted_verb.replace(" ", "%20"));
        }
    }

    return searchable;
}


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

                    if raw_traditional.chars().count() <= 4 {
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

                        let pinyin_tones_regex = Regex::new(r"\s").unwrap();
                        let searchable_pinyin_tones: String = pinyin_tones_regex.split(&pronunciation).filter(|&x| x != "").collect::<String>().to_lowercase();

                        // Process English
                        let searchable_english: Vec<String> = get_searchable_definitions(&definitions);

                        // Create JSON Object
                        let word_object = json!({
                            "t": raw_traditional,
                            "s": raw_simplified,
                            "p": prettify(pronunciation),
                            "d": definitions,
                            "u": tone_marks,
                            "v": searchable_pinyin,
                            "w": searchable_pinyin_tones,
                            "x": searchable_english
                        });

                        syng_cedict.push(word_object);
                        println!("Added new word: {}", raw_simplified);
                    } else {
                        continue;
                    }
                }
            }

            let buffer = File::create("cc-cedict.json").expect("There was a problem");
            serde_json::to_writer(buffer, &syng_cedict).unwrap();
        }
    }

    println!("Finished.");
}
