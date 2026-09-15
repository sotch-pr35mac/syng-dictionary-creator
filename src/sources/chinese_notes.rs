use super::{ParsedRecord, SourceReport, rejected_record};
use crate::model::{
    Definition, LexicalKind, PartOfSpeech, Qualifier, QualifierCategory, Source, Sourced,
    normalize_headword, normalize_text,
};
use crate::pinyin::{from_marked, han_character_count};
use anyhow::{Context, Result, bail};
use std::io::BufRead;

pub(crate) fn parse(reader: impl BufRead, report: &mut SourceReport) -> Result<Vec<ParsedRecord>> {
    let mut records = Vec::new();
    for (line_index, line) in reader.lines().enumerate() {
        let line_number = u64::try_from(line_index + 1).expect("line number fits u64");
        let line = line.with_context(|| format!("read Chinese Notes line {line_number}"))?;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        report.input_records += 1;
        match parse_line(&line, line_number) {
            Ok(record) => records.push(record),
            Err(error) if error.to_string().starts_with("unknown-grammar:") => return Err(error),
            Err(error) => {
                records.push(rejected_record(
                    Source::ChineseNotes,
                    line_number,
                    format!("line:{line_number}"),
                    error.to_string(),
                ));
            }
        }
    }
    Ok(records)
}

pub(crate) fn parse_line(line: &str, line_number: u64) -> Result<ParsedRecord> {
    let columns = line.split('\t').collect::<Vec<_>>();
    if columns.len() != 16 {
        bail!("column-count:{}-expected-16", columns.len());
    }
    let simplified = normalize_headword(columns[1]).context("invalid-simplified-headword")?;
    let traditional = if columns[2] == r"\N" {
        simplified.clone()
    } else {
        normalize_headword(columns[2]).context("invalid-traditional-headword")?
    };
    let expected_syllables = han_character_count(&simplified);
    let pinyin = from_marked(
        columns[3],
        (expected_syllables > 0).then_some(expected_syllables),
    )
    .context("invalid-or-ambiguous-marked-pinyin")?;
    let gloss = normalize_text(columns[4]);
    if gloss.is_empty() || gloss == r"\N" {
        bail!("missing-english-gloss");
    }
    let mut definition = Definition::new(gloss, Source::ChineseNotes);
    match map_grammar(columns[5])? {
        Some(Grammar::PartOfSpeech(value)) => definition
            .parts_of_speech
            .push(Sourced::one(value, Source::ChineseNotes)),
        Some(Grammar::LexicalKind(value)) => definition
            .lexical_kinds
            .push(Sourced::one(value, Source::ChineseNotes)),
        None => {}
    }

    for value in [columns[9], columns[11]] {
        if is_value(value) {
            definition.qualifiers.push(Sourced::one(
                Qualifier {
                    category: QualifierCategory::Domain,
                    value: normalize_text(value),
                },
                Source::ChineseNotes,
            ));
        }
    }
    if is_value(columns[7]) {
        definition.qualifiers.push(Sourced::one(
            Qualifier {
                category: QualifierCategory::Information,
                value: normalize_text(columns[7]),
            },
            Source::ChineseNotes,
        ));
    }

    let notes = columns[14];
    let cited_cedict = notes.contains("CC-CEDICT");
    if let Some(commentary) = substantive_commentary(notes) {
        definition
            .commentary
            .push(Sourced::one(commentary, Source::ChineseNotes));
    }

    Ok(ParsedRecord {
        source: Source::ChineseNotes,
        order: line_number,
        locator: format!("line:{line_number};row:{}", columns[0]),
        simplified: Some(simplified.clone()),
        traditional: Some(traditional.clone()),
        explicit_headwords: vec![simplified, traditional],
        pinyin: Some(pinyin),
        definitions: vec![definition],
        alternative_pronunciations: Vec::new(),
        measure_words: Vec::new(),
        cited_cedict,
        rejection: None,
    })
}

fn is_value(value: &str) -> bool {
    !value.is_empty() && value != r"\N"
}

enum Grammar {
    PartOfSpeech(PartOfSpeech),
    LexicalKind(LexicalKind),
}

fn map_grammar(value: &str) -> Result<Option<Grammar>> {
    if !is_value(value) {
        return Ok(None);
    }
    let grammar = match value {
        "adjective" => Grammar::PartOfSpeech(PartOfSpeech::Adjective),
        "adverb" => Grammar::PartOfSpeech(PartOfSpeech::Adverb),
        "auxiliary verb" => Grammar::PartOfSpeech(PartOfSpeech::AuxiliaryVerb),
        "conjunction" => Grammar::PartOfSpeech(PartOfSpeech::Conjunction),
        "interjection" => Grammar::PartOfSpeech(PartOfSpeech::Interjection),
        "interrogative pronoun" => Grammar::PartOfSpeech(PartOfSpeech::InterrogativePronoun),
        "measure word" => Grammar::PartOfSpeech(PartOfSpeech::MeasureWord),
        "noun" => Grammar::PartOfSpeech(PartOfSpeech::Noun),
        "number" => Grammar::PartOfSpeech(PartOfSpeech::Number),
        "onomatopoeia" => Grammar::PartOfSpeech(PartOfSpeech::Onomatopoeia),
        "ordinal" => Grammar::PartOfSpeech(PartOfSpeech::Ordinal),
        "particle" => Grammar::PartOfSpeech(PartOfSpeech::Particle),
        "preposition" => Grammar::PartOfSpeech(PartOfSpeech::Preposition),
        "pronoun" => Grammar::PartOfSpeech(PartOfSpeech::Pronoun),
        "proper noun" => Grammar::PartOfSpeech(PartOfSpeech::ProperNoun),
        "quantity" => Grammar::PartOfSpeech(PartOfSpeech::Quantity),
        "verb" => Grammar::PartOfSpeech(PartOfSpeech::Verb),
        "boilerplate" => Grammar::LexicalKind(LexicalKind::Boilerplate),
        "bound form" => Grammar::LexicalKind(LexicalKind::BoundForm),
        "expression" => Grammar::LexicalKind(LexicalKind::Expression),
        "foreign" => Grammar::LexicalKind(LexicalKind::Foreign),
        "infix" => Grammar::LexicalKind(LexicalKind::Infix),
        "pattern" => Grammar::LexicalKind(LexicalKind::Pattern),
        "phrase" => Grammar::LexicalKind(LexicalKind::Phrase),
        "phonetic" => Grammar::LexicalKind(LexicalKind::Phonetic),
        "prefix" => Grammar::LexicalKind(LexicalKind::Prefix),
        "radical" => Grammar::LexicalKind(LexicalKind::Radical),
        "set phrase" => Grammar::LexicalKind(LexicalKind::SetPhrase),
        "suffix" => Grammar::LexicalKind(LexicalKind::Suffix),
        other => bail!("unknown-grammar:{other}"),
    };
    Ok(Some(grammar))
}

fn substantive_commentary(notes: &str) -> Option<String> {
    if !is_value(notes) {
        return None;
    }
    let trimmed = notes.trim();
    if trimmed.ends_with(')')
        && let Some(open) = trimmed.rfind('(')
    {
        let bibliography = &trimmed[open + 1..trimmed.len() - 1];
        if is_bibliography(bibliography) {
            let prose = trimmed[..open].trim();
            return (!prose.is_empty()).then(|| prose.to_owned());
        }
    }
    if is_bibliography(trimmed.trim_matches(['(', ')'])) {
        None
    } else if contains_citation_marker(trimmed) {
        // Mixed citation/prose without a clean trailing bibliography boundary is
        // withheld instead of guessing which words are substantive.
        None
    } else {
        Some(normalize_text(trimmed))
    }
}

fn is_bibliography(value: &str) -> bool {
    contains_citation_marker(value)
        && value
            .split(';')
            .all(|item| contains_citation_marker(item.trim()))
}

fn contains_citation_marker(value: &str) -> bool {
    [
        "CC-CEDICT",
        "Guoyu",
        "ABC",
        "Mathews",
        "NCCED",
        "CEDICT",
        "Unihan",
    ]
    .iter()
    .any(|marker| value.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(traditional: &str, grammar: &str, notes: &str) -> String {
        [
            "7",
            "炒米",
            traditional,
            "chǎomǐ",
            "roasted rice; parched rice",
            grammar,
            r"\N",
            r"\N",
            "现代汉语",
            "Modern Chinese",
            r"\N",
            r"\N",
            r"\N",
            r"\N",
            notes,
            "6",
        ]
        .join("\t")
    }

    #[test]
    fn parses_sentinel_and_keeps_english_whole() {
        let record = parse_line(&row(r"\N", "noun", "(CC-CEDICT '炒米')"), 2).unwrap();
        assert_eq!(record.traditional.as_deref(), Some("炒米"));
        assert_eq!(
            record.definitions[0].gloss.value,
            "roasted rice; parched rice"
        );
        assert!(record.definitions[0].commentary.is_empty());
        assert!(record.cited_cedict);
    }

    #[test]
    fn separates_substantive_notes_from_bibliography() {
        let record = parse_line(
            &row(
                r"\N",
                "phrase",
                "Traditional Mongolian food (ABC 'chǎomǐ'; CC-CEDICT '炒米')",
            ),
            2,
        )
        .unwrap();
        assert_eq!(
            record.definitions[0].commentary[0].value,
            "Traditional Mongolian food"
        );
    }

    #[test]
    fn new_grammar_blocks_publication() {
        assert!(parse_line(&row(r"\N", "future-kind", r"\N"), 2).is_err());
    }
}
