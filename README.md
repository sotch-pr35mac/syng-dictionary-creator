# __Syng | 词应__ Dictionary Creator
#### Create a dictionary file for [Syng | 词应 Chinese-English Dictionary](http://getsyng.com)

## About
This project takes a CC-CEDICT file and generates a series of `.dictionary` files and a Chinese tokenization FST to be used in conjunction with Syng Dictionary.

## __Result__
The resulting `.dictionary` files will have the words from the CC-CEDICT file in the following format:
```rust
struct MeasureWord {
        traditional: String,
        simplified: String,
        pinyin_marks: String,
        pinyin_numbers: String
}

enum HskLevel {
        One,
        Two,
        Three,
        Four,
        Five,
        Six,
        SevenToNine
}

struct HskLevels {
        hsk_2015: Vec<HskLevel>,
        proficiency_standard_2021: Vec<HskLevel>,
        hsk_exam_syllabus_2025: Vec<HskLevel>
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
        hsk: HskLevels,
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

The generator also writes `chinese.fst`, an `fst` 0.4 set containing the deduplicated union of the simplified and traditional dictionary headwords.

## __Usage__
1. Run `cargo run`
2. Copy the resulting `.dictionary` files and `chinese.fst` into the `chinese_dictionary/data` directory.

## __License__
This software is licensed under the [GNU Public License v3](https://www.gnu.org/licenses/gpl-3.0.en.html).
The CC-CEDICT and resulting `.dictionary` files are licensed under the [Creative Commons Attribution-Share Alike 4.0 License](https://creativecommons.org/licenses/by-sa/4.0/).
