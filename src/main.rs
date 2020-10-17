/*
 *	@author			::	Preston Wang-Stosur-Bassett <p.wanstobas@gmail.com>
 *	@created		::	October 6, 2020
 *	@description	::	This file converts a cc-cedict file and outputs a Syng Dictionary file
 */
extern crate bincode;
extern crate hsk;
extern crate prettify_pinyin;
extern crate serde;
extern crate serde_derive;
extern crate regex;

mod cedict_utils;
mod dictionary_utils;

use dictionary_utils::{WordEntry, SyngDictionary};
use cedict_utils as cedict;

fn main() {
	println!("\nBuilding Word List");
	let word_list: Vec<WordEntry> = cedict::get_cedict_data();
	println!("\nBuidling Dictionary File");
	let dictionary: SyngDictionary = dictionary_utils::build_dictionary(word_list);
	println!("\nWriting Files");
	dictionary_utils::write_dictionary_files(&dictionary);
	println!("\nFinished.");
}