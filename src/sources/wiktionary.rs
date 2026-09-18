//! Streaming adapter for retained Chinese entries from English Wiktionary JSONL.

use super::{ParsedPronunciation, ParsedRecord, SourceReport, rejected_record};
use crate::model::{
    ChineseVariety, Definition, Example, LexicalKind, MeasureWordReference, PartOfSpeech,
    Qualifier, QualifierCategory, Source, Sourced, normalize_headword, normalize_text,
};
use crate::pinyin::{from_marked, han_character_count};
use anyhow::{Context, Result, bail};
use character_converter::{simplified_to_traditional, traditional_to_simplified};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;

#[derive(Debug, Deserialize)]
struct Wrapper {
    source_line: u64,
    raw: WikiEntry,
}

#[derive(Debug, Deserialize)]
struct WikiEntry {
    word: String,
    #[serde(default)]
    lang: String,
    #[serde(default)]
    lang_code: String,
    pos: String,
    #[serde(default)]
    forms: Vec<WikiForm>,
    #[serde(default)]
    sounds: Vec<WikiSound>,
    #[serde(default)]
    senses: Vec<WikiSense>,
    #[serde(default)]
    redirect: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WikiForm {
    form: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WikiSound {
    #[serde(default, alias = "zh-pron")]
    zh_pron: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WikiSense {
    #[serde(default)]
    glosses: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    topics: Vec<String>,
    #[serde(default)]
    examples: Vec<WikiExample>,
}

#[derive(Debug, Deserialize)]
struct WikiExample {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    english: Option<String>,
    #[serde(default)]
    translation: Option<String>,
}

/// Streams JSONL entries and produces bounded, source-scoped parser records.
pub(crate) fn parse(reader: impl BufRead, report: &mut SourceReport) -> Result<Vec<ParsedRecord>> {
    let mut records = Vec::new();
    for (retained_index, line) in reader.lines().enumerate() {
        let retained_line = u64::try_from(retained_index + 1).expect("line number fits u64");
        let line =
            line.with_context(|| format!("read Wiktionary retained line {retained_line}"))?;
        report.input_records += 1;
        let wrapper = match serde_json::from_str::<Wrapper>(&line) {
            Ok(wrapper) => wrapper,
            Err(error) => {
                report.diagnose("malformed-json-or-field-type");
                records.push(rejected_record(
                    Source::Wiktionary,
                    retained_line,
                    format!("retained-line:{retained_line}"),
                    format!("malformed-json-or-field-type:{error}"),
                ));
                continue;
            }
        };
        match parse_entry(wrapper.raw, wrapper.source_line, report) {
            Ok(record) => records.push(record),
            Err(error) if error.to_string().starts_with("unknown-pos:") => return Err(error),
            Err(error) => {
                records.push(rejected_record(
                    Source::Wiktionary,
                    wrapper.source_line,
                    format!("source-line:{}", wrapper.source_line),
                    error.to_string(),
                ));
            }
        }
    }
    Ok(records)
}

/// Assigns explicit Mandarin pronunciation evidence and converts publishable senses.
fn parse_entry(
    entry: WikiEntry,
    source_line: u64,
    report: &mut SourceReport,
) -> Result<ParsedRecord> {
    if entry.redirect.is_some() || entry.pos == "soft-redirect" {
        bail!("redirect");
    }
    if matches!(
        entry.pos.as_str(),
        "romanization" | "symbol" | "punct" | "character"
    ) {
        bail!("non-lexical-pos");
    }
    let (part_of_speech, lexical_kind) = map_pos(&entry.pos)?;
    let word = normalize_headword(&entry.word).context("invalid-headword")?;
    let mut simplified_forms = BTreeSet::new();
    let mut traditional_forms = BTreeSet::new();
    for form in &entry.forms {
        // Alternative spellings are lexical variants, not evidence for this
        // record's canonical simplified/traditional identity.
        if form.tags.iter().any(|tag| tag == "alternative") {
            continue;
        }
        if form.tags.iter().any(|tag| tag == "Simplified-Chinese") {
            simplified_forms.insert(normalize_headword(&form.form).context("invalid-form")?);
            traditional_forms.insert(word.clone());
        }
        if form.tags.iter().any(|tag| tag == "Traditional-Chinese") {
            traditional_forms.insert(normalize_headword(&form.form).context("invalid-form")?);
            simplified_forms.insert(word.clone());
        }
    }
    if simplified_forms.len() > 1 || traditional_forms.len() > 1 {
        bail!("ambiguous-script-forms");
    }
    let simplified = simplified_forms.into_iter().next();
    let traditional = traditional_forms.into_iter().next();
    let expected_syllables = simplified
        .as_deref()
        .or(traditional.as_deref())
        .map(han_character_count)
        .filter(|count| *count > 0);

    let mut pronunciations = BTreeMap::new();
    for sound in &entry.sounds {
        let is_mandarin_pinyin = sound.tags.iter().any(|tag| tag == "Mandarin")
            && sound.tags.iter().any(|tag| tag == "Pinyin")
            && !sound.tags.iter().any(|tag| tag == "Tongyong-Pinyin");
        if !is_mandarin_pinyin {
            continue;
        }
        if let Some(value) = &sound.zh_pron
            && let Ok(pronunciation) = from_marked(value, expected_syllables)
        {
            let labels = pronunciations
                .entry(pronunciation)
                .or_insert_with(BTreeSet::new);
            if sound.tags.iter().any(|tag| tag == "Taiwan") {
                labels.insert("Taiwan pr.".to_owned());
            }
            if sound.tags.iter().any(|tag| tag == "Mainland-China") {
                labels.insert("Mainland China pr.".to_owned());
            }
        }
    }
    if pronunciations.is_empty() {
        bail!("missing-explicit-mandarin-pinyin");
    }
    let pronunciations = pronunciations
        .into_iter()
        .map(|(pronunciation, labels)| ParsedPronunciation {
            pronunciation,
            label: (!labels.is_empty()).then(|| labels.into_iter().collect::<Vec<_>>().join("/")),
        })
        .collect::<Vec<_>>();

    let mut definitions = Vec::new();
    let mut measure_words = Vec::new();
    for sense in entry.senses {
        if explicitly_non_mandarin(&sense.tags) {
            report.diagnose("excluded-non-mandarin-sense");
            continue;
        }
        let Some(leaf) = sense.glosses.last() else {
            report.diagnose("sense-without-gloss");
            continue;
        };
        let parsed_gloss = parse_leaf_gloss(leaf, report);
        if parsed_gloss.glosses.is_empty() {
            report.diagnose("empty-leaf-gloss");
            continue;
        }
        merge_measure_word_references(&mut measure_words, parsed_gloss.measure_words);
        // Examples belong to the Wiktionary sense, not to each semicolon
        // alternative inside its leaf gloss. Keep them on the first emitted
        // definition so splitting alternatives cannot publish duplicates.
        let mut examples = parse_examples(sense.examples, report);
        let qualifiers = sense
            .topics
            .iter()
            .filter_map(|topic| {
                if reviewed_topic(topic) {
                    Some(Sourced::one(
                        Qualifier {
                            category: QualifierCategory::Domain,
                            value: topic.clone(),
                        },
                        Source::Wiktionary,
                    ))
                } else {
                    report.diagnose("unmapped-topic");
                    None
                }
            })
            .chain(sense.tags.iter().filter_map(|tag| {
                map_tag(tag).map(|qualifier| Sourced::one(qualifier, Source::Wiktionary))
            }))
            .collect::<Vec<_>>();
        for gloss in parsed_gloss.glosses {
            let mut definition = Definition::new(gloss, Source::Wiktionary);
            if let Some(value) = part_of_speech {
                definition
                    .parts_of_speech
                    .push(Sourced::one(value, Source::Wiktionary));
            }
            if let Some(value) = lexical_kind {
                definition
                    .lexical_kinds
                    .push(Sourced::one(value, Source::Wiktionary));
            }
            definition.qualifiers = qualifiers.clone();
            definition.examples = std::mem::take(&mut examples);
            definitions.push(definition);
        }
    }
    if definitions.is_empty() {
        bail!("no-publishable-mandarin-definitions");
    }

    let mut explicit_headwords = vec![word];
    explicit_headwords.extend(simplified.iter().cloned());
    explicit_headwords.extend(traditional.iter().cloned());
    explicit_headwords.sort();
    explicit_headwords.dedup();
    Ok(ParsedRecord {
        source: Source::Wiktionary,
        order: source_line,
        locator: format!(
            "source-line:{source_line};lang:{};code:{}",
            entry.lang, entry.lang_code
        ),
        simplified,
        traditional,
        explicit_headwords,
        pronunciations,
        definitions,
        alternative_pronunciations: Vec::new(),
        measure_words,
        rejection: None,
    })
}

struct ParsedWiktionaryGloss {
    glosses: Vec<String>,
    measure_words: Vec<Sourced<MeasureWordReference>>,
}

/// Classifiers qualify the headword rather than one split English alternative.
/// Normalize duplicate source annotations before they enter the entry-wide list.
fn merge_measure_word_references(
    existing: &mut Vec<Sourced<MeasureWordReference>>,
    incoming: Vec<Sourced<MeasureWordReference>>,
) {
    for incoming_reference in incoming {
        let matching_reference = existing.iter_mut().find(|existing_reference| {
            existing_reference.value.traditional == incoming_reference.value.traditional
                && existing_reference.value.simplified == incoming_reference.value.simplified
        });
        let Some(existing_reference) = matching_reference else {
            existing.push(incoming_reference);
            continue;
        };

        for incoming_variety in incoming_reference.value.varieties {
            if !existing_reference
                .value
                .varieties
                .contains(&incoming_variety)
            {
                existing_reference.value.varieties.push(incoming_variety);
            }
        }
        for incoming_source in incoming_reference.sources {
            if !existing_reference.sources.contains(&incoming_source) {
                existing_reference.sources.push(incoming_source);
            }
        }
    }
}

/// Removes one standard terminal classifier annotation before normalizing a leaf.
///
/// Invalid annotations deliberately remain literal gloss text. That is safer
/// than silently erasing source prose or publishing a partly parsed classifier.
fn parse_leaf_gloss(value: &str, report: &mut SourceReport) -> ParsedWiktionaryGloss {
    let normalized = normalize_text(value);
    if normalized.is_empty() {
        return ParsedWiktionaryGloss {
            glosses: Vec::new(),
            measure_words: Vec::new(),
        };
    }
    let Some(without_close) = normalized.strip_suffix(')') else {
        return ParsedWiktionaryGloss {
            glosses: split_standalone_glosses(&normalized),
            measure_words: Vec::new(),
        };
    };
    let Some(annotation_start) = without_close.rfind("(Classifier:") else {
        return ParsedWiktionaryGloss {
            glosses: split_standalone_glosses(&normalized),
            measure_words: Vec::new(),
        };
    };
    let gloss = normalize_text(&without_close[..annotation_start]);
    let annotation = &without_close[annotation_start + "(Classifier:".len()..];
    if gloss.is_empty() || annotation.contains(['(', ')']) {
        report.diagnose("malformed-classifier-annotation");
        return ParsedWiktionaryGloss {
            glosses: vec![normalized],
            measure_words: Vec::new(),
        };
    }
    match parse_wiktionary_classifiers(annotation) {
        Ok(measure_words) => ParsedWiktionaryGloss {
            glosses: split_standalone_glosses(&gloss),
            measure_words,
        },
        Err(()) => {
            report.diagnose("malformed-classifier-annotation");
            ParsedWiktionaryGloss {
                glosses: vec![normalized],
                measure_words: Vec::new(),
            }
        }
    }
}

/// Parses the semicolon-delimited contents of Wiktionary's `Classifier:` item.
fn parse_wiktionary_classifiers(
    value: &str,
) -> std::result::Result<Vec<Sourced<MeasureWordReference>>, ()> {
    let mut references = Vec::new();
    for raw_reference in value.split(';') {
        let mut fields = raw_reference.split_whitespace();
        let Some(forms) = fields.next() else {
            return Err(());
        };
        let (traditional, simplified) = forms
            .split_once('／')
            .or_else(|| forms.split_once('/'))
            .map_or((forms, forms), |(traditional, simplified)| {
                (traditional, simplified)
            });
        let traditional = normalize_headword(traditional).map_err(|_| ())?;
        let simplified = normalize_headword(simplified).map_err(|_| ())?;
        let mut varieties = Vec::new();
        for raw_variety in fields {
            let variety = classifier_variety(raw_variety).ok_or(())?;
            if !varieties.contains(&variety) {
                varieties.push(variety);
            }
        }
        references.push(Sourced::one(
            MeasureWordReference {
                traditional,
                simplified,
                lexical_id: None,
                varieties,
            },
            Source::Wiktionary,
        ));
    }
    (!references.is_empty()).then_some(references).ok_or(())
}

/// Maps only the reviewed `Module:zh-pron` abbreviations used by `zh-mw`.
fn classifier_variety(value: &str) -> Option<ChineseVariety> {
    Some(match value {
        "m" | "m-x" | "m-nj" => ChineseVariety::Mandarin,
        "m-s" => ChineseVariety::Sichuanese,
        "dg" => ChineseVariety::Dungan,
        "c" | "c-dg" | "c-yj" => ChineseVariety::Cantonese,
        "c-t" => ChineseVariety::Taishanese,
        "g" => ChineseVariety::Gan,
        "h" => ChineseVariety::Hakka,
        "j" => ChineseVariety::Jin,
        "mb" => ChineseVariety::NorthernMin,
        "mc" => ChineseVariety::MiddleChinese,
        "md" => ChineseVariety::EasternMin,
        "mn" => ChineseVariety::Hokkien,
        "mn-l" => ChineseVariety::LeizhouMin,
        "mn-t" => ChineseVariety::Teochew,
        "oc" => ChineseVariety::OldChinese,
        "px" => ChineseVariety::PuxianMin,
        "sp" => ChineseVariety::SouthernPinghua,
        "w" | "w-j" => ChineseVariety::Wu,
        "x" => ChineseVariety::Xiang,
        "x-l" => ChineseVariety::LoudiXiang,
        "x-h" => ChineseVariety::HengyangXiang,
        _ => return None,
    })
}

/// Splits only independent top-level semicolon alternatives in a leaf gloss.
fn split_standalone_glosses(value: &str) -> Vec<String> {
    let mut boundaries = Vec::new();
    let mut parentheses = 0_u32;
    let mut brackets = 0_u32;
    let mut braces = 0_u32;
    let mut quote_end = None;
    for (offset, character) in value.char_indices() {
        if let Some(expected_end) = quote_end {
            if character == expected_end {
                quote_end = None;
            }
            continue;
        }
        match character {
            '"' => quote_end = Some('"'),
            '“' => quote_end = Some('”'),
            '‘' => quote_end = Some('’'),
            '\'' if begins_ascii_single_quote(value, offset) => quote_end = Some('\''),
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            ';' if parentheses == 0 && brackets == 0 && braces == 0 => boundaries.push(offset),
            _ => {}
        }
    }
    if boundaries.is_empty() {
        return vec![normalize_text(value)];
    }
    let mut segments = Vec::with_capacity(boundaries.len() + 1);
    let mut start = 0;
    for boundary in boundaries {
        let segment = normalize_text(&value[start..boundary]);
        if !segment.is_empty() {
            segments.push(segment);
        }
        start = boundary + ';'.len_utf8();
    }
    let final_segment = normalize_text(&value[start..]);
    if !final_segment.is_empty() {
        segments.push(final_segment);
    }
    let mut glosses = Vec::<String>::with_capacity(segments.len());
    for segment in segments {
        if let Some(previous) = glosses.last_mut()
            && is_dependent_continuation(previous, &segment)
        {
            previous.push_str("; ");
            previous.push_str(&segment);
        } else {
            glosses.push(segment);
        }
    }
    glosses
}

/// Avoids treating apostrophes in contractions as quotation delimiters.
fn begins_ascii_single_quote(value: &str, offset: usize) -> bool {
    let boundary = value[..offset].chars().next_back().is_none_or(|character| {
        character.is_whitespace() || matches!(character, '(' | '[' | '{' | ':' | ';' | ',')
    });
    boundary && value[offset + '\''.len_utf8()..].contains('\'')
}

/// Recognizes high-confidence prose that continues an earlier gloss.
///
/// Common prepositions and words such as `used` and `until` are deliberately
/// not sufficient on their own: Wiktionary also uses them at the start of
/// independent translations (`used to`, `until the end`, and so on). The
/// accepted forms below encode the surrounding syntax that makes continuation
/// substantially more certain while leaving ambiguous alternatives split.
fn is_dependent_continuation(previous: &str, value: &str) -> bool {
    let lower = value.trim_start().to_ascii_lowercase();
    if lower.starts_with(['(', '[', '{', ',', ':', '—', '–', '-']) {
        return true;
    }

    if [
        "especially ",
        "particularly ",
        "chiefly ",
        "usually ",
        "often ",
        "including ",
        "such as ",
        "e.g.",
        "i.e.",
        "etc.",
        "also used ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }

    if [
        "used in ",
        "used after ",
        "used before ",
        "used with ",
        "used for ",
        "used as ",
        "used primarily ",
        "used especially ",
        "used to indicate ",
        "used to mark ",
        "used to express ",
        "used to refer ",
        "used to mean ",
        "used other than ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }

    if [
        "until none ",
        "until no ",
        "until all ",
        "until it ",
        "until they ",
        "until one ",
        "until this ",
        "until that ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }

    lower.starts_with("namely ")
        && previous
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("the ")
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ExampleScript {
    Simplified,
    Traditional,
    Both,
}

#[derive(Clone, Debug)]
struct ExampleCandidate {
    order: usize,
    chinese: String,
    english: Option<String>,
    script: ExampleScript,
}

/// Pairs script variants using exact English and converter-derived character keys.
fn parse_examples(examples: Vec<WikiExample>, report: &mut SourceReport) -> Vec<Sourced<Example>> {
    let mut groups = BTreeMap::<(Option<String>, String, String), Vec<ExampleCandidate>>::new();
    for (order, example) in examples.into_iter().enumerate() {
        match example.kind.as_deref() {
            Some("quotation") => {
                report.excluded_quotations += 1;
                continue;
            }
            Some("example") => {}
            _ => {
                report.excluded_unknown_examples += 1;
                continue;
            }
        }
        let Some(chinese) = example
            .text
            .map(|text| normalize_text(&text))
            .filter(|text| !text.is_empty())
        else {
            report.diagnose("example-without-text");
            continue;
        };
        let english = example
            .english
            .or(example.translation)
            .map(|english| normalize_text(&english))
            .filter(|english| !english.is_empty());
        let simplified_key = traditional_to_simplified(&chinese);
        let traditional_key = simplified_to_traditional(&chinese);
        let tagged_simplified = example.tags.iter().any(|tag| tag == "Simplified-Chinese");
        let tagged_traditional = example.tags.iter().any(|tag| tag == "Traditional-Chinese");
        let script = match (tagged_simplified, tagged_traditional) {
            (true, true) => ExampleScript::Both,
            (true, false) => {
                if chinese != simplified_key {
                    report.diagnose("example-script-tag-conversion-mismatch");
                }
                ExampleScript::Simplified
            }
            (false, true) => {
                if chinese != traditional_key {
                    report.diagnose("example-script-tag-conversion-mismatch");
                }
                ExampleScript::Traditional
            }
            (false, false) if chinese == simplified_key && chinese == traditional_key => {
                report.diagnose("inferred-script-neutral-example");
                ExampleScript::Both
            }
            (false, false) if chinese == simplified_key => {
                report.diagnose("inferred-simplified-example");
                ExampleScript::Simplified
            }
            (false, false) if chinese == traditional_key => {
                report.diagnose("inferred-traditional-example");
                ExampleScript::Traditional
            }
            (false, false) => {
                report.diagnose("unclassifiable-mixed-script-example");
                continue;
            }
        };
        groups
            .entry((
                english.clone(),
                simplified_key.into_owned(),
                traditional_key.into_owned(),
            ))
            .or_default()
            .push(ExampleCandidate {
                order,
                chinese,
                english,
                script,
            });
    }

    let mut paired = Vec::<(usize, Example)>::new();
    for candidates in groups.values_mut() {
        candidates.sort_by_key(|candidate| candidate.order);
        let mut seen = BTreeSet::new();
        candidates.retain(|candidate| seen.insert((candidate.chinese.clone(), candidate.script)));
        let mut simplified = candidates
            .iter()
            .filter(|candidate| candidate.script == ExampleScript::Simplified)
            .collect::<Vec<_>>();
        let mut traditional = candidates
            .iter()
            .filter(|candidate| candidate.script == ExampleScript::Traditional)
            .collect::<Vec<_>>();
        let both = candidates
            .iter()
            .filter(|candidate| candidate.script == ExampleScript::Both)
            .collect::<Vec<_>>();
        let english = candidates
            .first()
            .expect("nonempty example group")
            .english
            .clone();

        if simplified.len() > 1
            || traditional.len() > 1
            || both.len() > 1
            || (!both.is_empty() && (!simplified.is_empty() || !traditional.is_empty()))
        {
            report.diagnose("ambiguous-example-conversion-class");
            for candidate in candidates.iter() {
                let (simplified, traditional) = match candidate.script {
                    ExampleScript::Simplified => (Some(candidate.chinese.clone()), None),
                    ExampleScript::Traditional => (None, Some(candidate.chinese.clone())),
                    ExampleScript::Both => (
                        Some(candidate.chinese.clone()),
                        Some(candidate.chinese.clone()),
                    ),
                };
                paired.push((
                    candidate.order,
                    complete_example_scripts(
                        simplified,
                        traditional,
                        candidate.english.clone(),
                        report,
                    ),
                ));
            }
            continue;
        }

        if let Some(candidate) = both.first() {
            paired.push((
                candidate.order,
                complete_example_scripts(
                    Some(candidate.chinese.clone()),
                    Some(candidate.chinese.clone()),
                    english,
                    report,
                ),
            ));
            continue;
        }
        let simplified = simplified.pop();
        let traditional = traditional.pop();
        if simplified.is_some() && traditional.is_some() {
            report.diagnose("paired-script-example");
        }
        let order = simplified
            .map(|candidate| candidate.order)
            .into_iter()
            .chain(traditional.map(|candidate| candidate.order))
            .min()
            .expect("nonempty example group");
        paired.push((
            order,
            complete_example_scripts(
                simplified.map(|candidate| candidate.chinese.clone()),
                traditional.map(|candidate| candidate.chinese.clone()),
                english,
                report,
            ),
        ));
    }
    paired.sort_by_key(|(order, _)| *order);
    let mut seen = BTreeSet::new();
    paired.retain(|(_, example)| {
        let unique = seen.insert(example.clone());
        if !unique {
            report.diagnose("deduplicated-normalized-example");
        }
        unique
    });
    paired
        .into_iter()
        .map(|(_, example)| Sourced::one(example, Source::Wiktionary))
        .collect()
}

/// Preserves source-attested text and generates only a missing script counterpart.
fn complete_example_scripts(
    simplified: Option<String>,
    traditional: Option<String>,
    english: Option<String>,
    report: &mut SourceReport,
) -> Example {
    match (simplified, traditional) {
        (Some(simplified), Some(traditional)) => Example {
            simplified: Some(simplified),
            traditional: Some(traditional),
            english,
        },
        (Some(simplified), None) => {
            let traditional = simplified_to_traditional(&simplified).into_owned();
            report.diagnose("generated-traditional-example");
            Example {
                simplified: Some(simplified),
                traditional: Some(traditional),
                english,
            }
        }
        (None, Some(traditional)) => {
            let simplified = traditional_to_simplified(&traditional).into_owned();
            report.diagnose("generated-simplified-example");
            Example {
                simplified: Some(simplified),
                traditional: Some(traditional),
                english,
            }
        }
        (None, None) => unreachable!("accepted example has a classified Chinese form"),
    }
}

/// Maps the reviewed Wiktionary part-of-speech vocabulary into closed enums.
fn map_pos(value: &str) -> Result<(Option<PartOfSpeech>, Option<LexicalKind>)> {
    let result = match value {
        "noun" => (Some(PartOfSpeech::Noun), None),
        "verb" => (Some(PartOfSpeech::Verb), None),
        "adj" => (Some(PartOfSpeech::Adjective), None),
        "adv" => (Some(PartOfSpeech::Adverb), None),
        "name" => (Some(PartOfSpeech::ProperNoun), None),
        "pron" => (Some(PartOfSpeech::Pronoun), None),
        "intj" => (Some(PartOfSpeech::Interjection), None),
        "conj" => (Some(PartOfSpeech::Conjunction), None),
        "classifier" => (
            Some(PartOfSpeech::MeasureWord),
            Some(LexicalKind::Classifier),
        ),
        "num" => (Some(PartOfSpeech::Number), None),
        "prep" | "circumpos" => (Some(PartOfSpeech::Preposition), None),
        "postp" => (Some(PartOfSpeech::Postposition), None),
        "particle" => (Some(PartOfSpeech::Particle), None),
        "det" => (Some(PartOfSpeech::Determiner), None),
        "phrase" | "idiom" => (None, Some(LexicalKind::Phrase)),
        "proverb" => (None, Some(LexicalKind::Proverb)),
        "prefix" => (None, Some(LexicalKind::Prefix)),
        "suffix" => (None, Some(LexicalKind::Suffix)),
        "infix" => (None, Some(LexicalKind::Infix)),
        "contraction" => (None, Some(LexicalKind::Contraction)),
        other => bail!("unknown-pos:{other}"),
    };
    Ok(result)
}

/// Detects tags that explicitly exclude a sense from Mandarin lexical entities.
fn explicitly_non_mandarin(tags: &[String]) -> bool {
    const NON_MANDARIN: &[&str] = &[
        "Cantonese",
        "Dungan",
        "Gan",
        "Hainanese",
        "Hakka",
        "Hokkien",
        "Huizhou",
        "Jin",
        "Min",
        "Pinghua",
        "Taishanese",
        "Teochew",
        "Waxiang",
        "Wu",
        "Xiang",
    ];
    tags.iter().any(|tag| {
        NON_MANDARIN.iter().any(|family| {
            tag == family
                || tag
                    .strip_prefix(family)
                    .is_some_and(|suffix| suffix.starts_with('-'))
                || tag
                    .strip_suffix(family)
                    .is_some_and(|prefix| prefix.ends_with('-'))
        })
    })
}

/// Returns whether a topic is approved for structured domain publication.
fn reviewed_topic(value: &str) -> bool {
    matches!(
        value,
        "arts"
            | "biology"
            | "botany"
            | "business"
            | "chemistry"
            | "computing"
            | "economics"
            | "engineering"
            | "finance"
            | "food"
            | "grammar"
            | "law"
            | "linguistics"
            | "mathematics"
            | "medicine"
            | "military"
            | "music"
            | "physics"
            | "politics"
            | "religion"
            | "sciences"
            | "sports"
            | "technology"
            | "transport"
            | "zoology"
    )
}

/// Maps a reviewed usage tag into its structured qualifier category.
fn map_tag(value: &str) -> Option<Qualifier> {
    let (category, mapped_value) = match value {
        "archaic" | "dated" | "obsolete" | "rare" => (QualifierCategory::Usage, value),
        "colloquial" | "formal" | "informal" | "literary" | "slang" | "vulgar" => {
            (QualifierCategory::Register, value)
        }
        "Hong-Kong" | "Mainland-China" | "Taiwan" => (QualifierCategory::Region, value),
        "historical" | "figuratively" | "humorous" | "idiomatic" | "metaphoric" => {
            (QualifierCategory::Information, value)
        }
        "only-in" | "chiefly" | "usually" => (QualifierCategory::Restriction, value),
        _ => return None,
    };
    Some(Qualifier {
        category,
        value: mapped_value.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_json(senses: &str, sounds: &str) -> String {
        format!(
            r#"{{"source_line":7,"raw":{{"word":"煙火","lang":"Chinese","lang_code":"zh","pos":"noun","forms":[{{"form":"烟火","tags":["Simplified-Chinese"]}}],"sounds":{sounds},"senses":{senses}}}}}"#
        )
    }

    #[test]
    fn preserves_leaf_gloss_and_only_structured_examples() {
        let json = entry_json(
            r#"[{"glosses":["fire-related things","fireworks"],"topics":["arts"],"examples":[{"type":"example","text":"看煙火","english":"watch fireworks"},{"type":"quotation","text":"quoted"},{"text":"unknown"}]}]"#,
            r#"[{"zh_pron":"yānhuǒ","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
        let mut report = SourceReport::default();
        let record = parse_entry(wrapper.raw, wrapper.source_line, &mut report).unwrap();
        assert_eq!(record.definitions[0].gloss.value, "fireworks");
        assert_eq!(record.definitions[0].examples.len(), 1);
        assert_eq!(
            record.definitions[0].examples[0]
                .value
                .traditional
                .as_deref(),
            Some("看煙火")
        );
        assert_eq!(
            record.definitions[0].examples[0]
                .value
                .simplified
                .as_deref(),
            Some("看烟火")
        );
        assert_eq!(report.diagnostics["generated-simplified-example"], 1);
        assert_eq!(report.excluded_quotations, 1);
        assert_eq!(report.excluded_unknown_examples, 1);
    }

    #[test]
    fn supports_historical_zh_pron_and_retains_multiple_readings() {
        let json = entry_json(
            r#"[{"glosses":["fireworks"]}]"#,
            r#"[{"zh-pron":"yānhuǒ","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
        assert!(parse_entry(wrapper.raw, 7, &mut SourceReport::default()).is_ok());

        let mixed = entry_json(
            r#"[{"glosses":["fireworks"]}]"#,
            r#"[{"zh_pron":"yānhuǒ","tags":["Mandarin","Pinyin"]},{"zh_pron":"yānhuō","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&mixed).unwrap();
        let record = parse_entry(wrapper.raw, 7, &mut SourceReport::default()).unwrap();
        assert_eq!(record.pronunciations.len(), 2);
    }

    #[test]
    fn excludes_explicitly_non_mandarin_senses() {
        let json = entry_json(
            r#"[{"glosses":["Mandarin gloss"]},{"glosses":["Cantonese gloss"],"tags":["Cantonese"]}]"#,
            r#"[{"zh_pron":"yānhuǒ","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
        let mut report = SourceReport::default();
        let record = parse_entry(wrapper.raw, 7, &mut report).unwrap();
        assert_eq!(record.definitions.len(), 1);
        assert_eq!(report.diagnostics["excluded-non-mandarin-sense"], 1);
    }

    #[test]
    fn alternative_script_forms_never_define_the_canonical_pair() {
        for (word, alternative) in [("癌症", "癌癥"), ("落井下石", "落穽下石")] {
            let json = format!(
                r#"{{"source_line":7,"raw":{{"word":"{word}","lang":"Chinese","lang_code":"zh","pos":"noun","forms":[{{"form":"{alternative}","tags":["alternative","Traditional-Chinese"]}}],"sounds":[{{"zh_pron":"ái","tags":["Mandarin","Pinyin"]}}],"senses":[{{"glosses":["fixture"]}}]}}}}"#
            );
            let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
            let record = parse_entry(wrapper.raw, 7, &mut SourceReport::default()).unwrap();
            assert_eq!(record.simplified, None);
            assert_eq!(record.traditional, None);
            assert_eq!(record.explicit_headwords, vec![word]);
            assert!(
                !record
                    .explicit_headwords
                    .iter()
                    .any(|value| value == alternative)
            );
        }
    }

    #[test]
    fn retains_a_canonical_counterpart_beside_an_alternative_form() {
        let json = r#"{"source_line":7,"raw":{"word":"煙火","lang":"Chinese","lang_code":"zh","pos":"noun","forms":[{"form":"焰火","tags":["alternative","Simplified-Chinese"]},{"form":"烟火","tags":["Simplified-Chinese"]}],"sounds":[{"zh_pron":"yānhuǒ","tags":["Mandarin","Pinyin"]}],"senses":[{"glosses":["fireworks"]}]}}"#;
        let wrapper: Wrapper = serde_json::from_str(json).unwrap();
        let record = parse_entry(wrapper.raw, 7, &mut SourceReport::default()).unwrap();
        assert_eq!(record.simplified.as_deref(), Some("烟火"));
        assert_eq!(record.traditional.as_deref(), Some("煙火"));
        assert_eq!(record.explicit_headwords, vec!["烟火", "煙火"]);
    }

    #[test]
    fn recognizes_reviewed_non_mandarin_families_and_variants() {
        let excluded = [
            "Cantonese",
            "Dungan",
            "Gan",
            "Hainanese",
            "Hakka",
            "Hokkien",
            "Huizhou",
            "Jin",
            "Min",
            "Pinghua",
            "Taishanese",
            "Teochew",
            "Waxiang",
            "Wu",
            "Xiang",
            "Min-Nan",
            "Coastal-Min",
            "Puxian-Min",
            "Leizhou-Min",
            "Taiwanese-Hokkien",
            "Hokkien-Xiamen",
            "Northern-Pinghua",
            "Mandalay-Taishanese",
        ];
        for tag in excluded {
            assert!(
                explicitly_non_mandarin(&[tag.to_owned()]),
                "expected {tag} to be excluded"
            );
        }

        for tag in [
            "Mandarin",
            "Sichuanese",
            "Taiwanese-Mandarin",
            "Mainland-China",
            "Jianghuai-Mandarin",
        ] {
            assert!(
                !explicitly_non_mandarin(&[tag.to_owned()]),
                "expected {tag} to be retained"
            );
        }
    }

    #[test]
    fn excludes_the_leizhou_min_sense_from_hezi() {
        let json = r#"{"source_line":830424,"raw":{"word":"核子","lang":"Chinese","lang_code":"zh","pos":"noun","forms":[{"form":"𣝗子","tags":["alternative"]}],"sounds":[{"zh_pron":"hézǐ","tags":["Mandarin","Pinyin"]}],"senses":[{"glosses":["nucleon"]},{"glosses":["nucleus"],"tags":["Taiwan"]},{"glosses":["testicle"],"tags":["Leizhou-Min"]},{"glosses":["pit; seed"],"tags":["Wu"]}]}}"#;
        let wrapper: Wrapper = serde_json::from_str(json).unwrap();
        let mut report = SourceReport::default();
        let record = parse_entry(wrapper.raw, wrapper.source_line, &mut report).unwrap();
        let glosses = record
            .definitions
            .iter()
            .map(|definition| definition.gloss.value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(glosses, vec!["nucleon", "nucleus"]);
        assert_eq!(report.diagnostics["excluded-non-mandarin-sense"], 2);
    }

    #[test]
    fn splits_standalone_wiktionary_gloss_alternatives() {
        let json = entry_json(
            r#"[{"glosses":["quality","perfect; excellent; flawless"]}]"#,
            r#"[{"zh_pron":"yuánmǎn","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
        let record = parse_entry(wrapper.raw, 7, &mut SourceReport::default()).unwrap();

        assert_eq!(record.definitions.len(), 3);
        assert_eq!(
            record
                .definitions
                .iter()
                .map(|definition| definition.gloss.value.as_str())
                .collect::<Vec<_>>(),
            vec!["perfect", "excellent", "flawless"]
        );
    }

    #[test]
    fn extracts_classifier_references_and_preserves_malformed_prose() {
        let valid = entry_json(
            r#"[{"glosses":["bacterium; germ (Classifier: 種／种 m c; 樖 c)"]}]"#,
            r#"[{"zh_pron":"xìjūn","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&valid).unwrap();
        let record = parse_entry(wrapper.raw, 7, &mut SourceReport::default()).unwrap();
        assert_eq!(record.definitions.len(), 2);
        assert!(
            record
                .definitions
                .iter()
                .all(|definition| definition.measure_words.is_empty())
        );
        assert_eq!(record.measure_words.len(), 2);
        let first = &record.measure_words[0].value;
        assert_eq!(first.traditional, "種");
        assert_eq!(first.simplified, "种");
        assert_eq!(first.lexical_id, None);
        assert_eq!(
            first.varieties,
            vec![ChineseVariety::Mandarin, ChineseVariety::Cantonese]
        );
        assert_eq!(
            record.measure_words[1].value.varieties,
            vec![ChineseVariety::Cantonese]
        );

        let malformed = entry_json(
            r#"[{"glosses":["thing (Classifier: 樖 c mystery)"]}]"#,
            r#"[{"zh_pron":"yānhuǒ","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&malformed).unwrap();
        let mut report = SourceReport::default();
        let record = parse_entry(wrapper.raw, 7, &mut report).unwrap();
        assert_eq!(
            record.definitions[0].gloss.value,
            "thing (Classifier: 樖 c mystery)"
        );
        assert!(record.definitions[0].measure_words.is_empty());
        assert_eq!(report.diagnostics["malformed-classifier-annotation"], 1);
    }

    #[test]
    fn preserves_nested_quoted_and_dependent_semicolon_prose() {
        assert_eq!(
            split_standalone_glosses("a (b; c); d [e; f]; \"g; h\"; 'j; k'"),
            vec!["a (b; c)", "d [e; f]", "\"g; h\"", "'j; k'"]
        );
        assert_eq!(
            split_standalone_glosses("to march; especially in a procession"),
            vec!["to march; especially in a procession"]
        );
        for (value, expected) in [
            (
                "to approach, be close to; used in 斥近",
                vec!["to approach, be close to; used in 斥近"],
            ),
            (
                "Particle used after verbs to show exhaustion or completion; until none is left",
                vec![
                    "Particle used after verbs to show exhaustion or completion; until none is left",
                ],
            ),
            (
                "the four divisions of Buddhist disciples; namely 比丘 (bǐqiū), 比丘尼 (bǐqiūní), 優婆塞 /优婆塞 (yōupósè), 優婆夷 /优婆夷 (yōupóyí)",
                vec![
                    "the four divisions of Buddhist disciples; namely 比丘 (bǐqiū), 比丘尼 (bǐqiūní), 優婆塞 /优婆塞 (yōupósè), 優婆夷 /优婆夷 (yōupóyí)",
                ],
            ),
        ] {
            assert_eq!(split_standalone_glosses(value), expected);
        }
        for (value, expected) in [
            (
                "once; before; used to; in the past",
                vec!["once", "before", "used to", "in the past"],
            ),
            (
                "until the end; to the finish; until something is done",
                vec!["until the end", "to the finish", "until something is done"],
            ),
        ] {
            assert_eq!(split_standalone_glosses(value), expected);
        }
    }

    #[test]
    fn does_not_duplicate_examples_across_split_leaf_glosses() {
        let json = entry_json(
            r#"[{"glosses":["to test; to examine"],"examples":[{"type":"example","text":"通過測驗","english":"to pass a test","tags":["Traditional-Chinese"]},{"type":"example","text":"通过测验","english":"to pass a test","tags":["Simplified-Chinese"]}]}]"#,
            r#"[{"zh_pron":"cèyàn","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
        let record = parse_entry(wrapper.raw, 7, &mut SourceReport::default()).unwrap();

        assert_eq!(record.definitions.len(), 2);
        assert_eq!(record.definitions[0].examples.len(), 1);
        assert!(record.definitions[1].examples.is_empty());
        assert_eq!(
            record.definitions[0].examples[0].value.english.as_deref(),
            Some("to pass a test")
        );
    }

    #[test]
    fn pairs_script_examples_by_conversion_with_and_without_english() {
        let json = entry_json(
            r#"[{"glosses":["complete"],"examples":[{"type":"example","text":"事情圓滿完成。","english":"It was completed successfully."},{"type":"example","text":"事情圆满完成。","english":"It was completed successfully."},{"type":"example","text":"圓滿結束。"},{"type":"example","text":"圆满结束。"}]}]"#,
            r#"[{"zh_pron":"yuánmǎn","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
        let record = parse_entry(wrapper.raw, 7, &mut SourceReport::default()).unwrap();
        let examples = &record.definitions[0].examples;
        assert_eq!(examples.len(), 2);
        assert_eq!(
            examples[0].value.simplified.as_deref(),
            Some("事情圆满完成。")
        );
        assert_eq!(
            examples[0].value.traditional.as_deref(),
            Some("事情圓滿完成。")
        );
        assert_eq!(examples[1].value.simplified.as_deref(), Some("圆满结束。"));
        assert_eq!(examples[1].value.traditional.as_deref(), Some("圓滿結束。"));
        assert_eq!(examples[1].value.english, None);
    }

    #[test]
    fn deduplicates_examples_that_converge_after_script_completion() {
        let json = entry_json(
            r#"[{"glosses":["complete"],"examples":[{"type":"example","text":"123","english":"number"},{"type":"example","text":"123","english":"number","tags":["Simplified-Chinese"]}]}]"#,
            r#"[{"zh_pron":"yuánmǎn","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
        let mut report = SourceReport::default();
        let record = parse_entry(wrapper.raw, 7, &mut report).unwrap();

        assert_eq!(record.definitions[0].examples.len(), 1);
        assert_eq!(report.diagnostics["deduplicated-normalized-example"], 1);
    }

    #[test]
    fn generates_only_the_missing_example_script() {
        let json = entry_json(
            r#"[{"glosses":["complete"],"examples":[{"type":"example","text":"学习中文。","tags":["Simplified-Chinese"]},{"type":"example","text":"觀看煙火。","tags":["Traditional-Chinese"]}]}]"#,
            r#"[{"zh_pron":"yuánmǎn","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
        let mut report = SourceReport::default();
        let record = parse_entry(wrapper.raw, 7, &mut report).unwrap();
        let examples = &record.definitions[0].examples;

        assert_eq!(examples.len(), 2);
        assert_eq!(examples[0].value.simplified.as_deref(), Some("学习中文。"));
        assert_eq!(examples[0].value.traditional.as_deref(), Some("學習中文。"));
        assert_eq!(examples[1].value.simplified.as_deref(), Some("观看烟火。"));
        assert_eq!(examples[1].value.traditional.as_deref(), Some("觀看煙火。"));
        assert_eq!(report.diagnostics["generated-traditional-example"], 1);
        assert_eq!(report.diagnostics["generated-simplified-example"], 1);
    }
}
