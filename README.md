# __Syng | 词应__ Dictionary Creator
#### Create a dictionary file for [Syng | 词应 Chinese-English Dictionary](http://getsyng.com)

## About
This project takes a CC-CEDICT file and generates a series of `.dictionary` files to be used in conjunction with Syng Dictionary. 

## __Result__
The resulting `.dictionary` files will have the words from the CC-CEDICT file in the following format:
```rust
struct MeasureWord {
        traditional: String,
        simplified: String,
        pinyin_marks: String,
        pinyin_numbers: String
}

struct WordEntry {
        traditional: String,
        simplified: String,
        pinyin_marks: String,
        pinyin_numbers: String,
        english: Vec<String>,
        tone_marks: Vec<u8>,
        hash: u64,
        measure_words: Vec<MeasureWord>,
        hsk: u8,
        word_id: u32
}

struct SyngDictionary {
        pinyin: HashMap<String, Vec<u32>>,
        english: HashMap<String, Vec<u32>>,
        simplified: HashMap<String, Vec<u32>>,
        traditional: HashMap<String, Vec<u32>>,
        data: HashMap<u32, WordEntry>
}
```

## __Usage__
1. Run `cargo run`
2. Take the resulting `.dictionary` files and move them into the the chinese_dictionary project. 

## __License__
This software is licensed under the [GNU Public License v3](https://www.gnu.org/licenses/gpl-3.0.en.html).
The CC-CEDICT and resulting `.dictionary` files are licensed under the [Creative Commons Attribution-Share Alike 4.0 License](https://creativecommons.org/licenses/by-sa/4.0/).
