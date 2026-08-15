// @author	::	Preston Wang-Stosur-Bassett <p.wanstobas@gmail.com>
// @created	::	October 6, 2020
// @description	::	This file converts a cc-cedict file and outputs a Syng Dictionary file

mod cedict_utils;
mod dictionary_utils;

use cedict_utils as cedict;

fn main() {
    println!("\nBuilding Word List");
    let word_list = cedict::get_cedict_data();
    println!("\nBuidling Dictionary File");
    let dictionary = dictionary_utils::build_dictionary(word_list);
    println!("\nWriting Files");
    dictionary_utils::write_dictionary_files(&dictionary);
    println!("\nFinished.");
}
