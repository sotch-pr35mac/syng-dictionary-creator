//! Streaming adapter for retained Chinese entries from English Wiktionary JSONL.

use super::{ParsedPronunciation, ParsedRecord, SourceReport, rejected_record};
use crate::model::{
    Definition, Example, LexicalKind, PartOfSpeech, Qualifier, QualifierCategory, Source, Sourced,
    normalize_headword, normalize_text,
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
    for sense in entry.senses {
        if explicitly_non_mandarin(&sense.tags) {
            report.diagnose("excluded-non-mandarin-sense");
            continue;
        }
        let Some((leaf, parents)) = sense.glosses.split_last() else {
            report.diagnose("sense-without-gloss");
            continue;
        };
        let leaf = normalize_text(leaf);
        if leaf.is_empty() {
            report.diagnose("empty-leaf-gloss");
            continue;
        }
        let mut definition = Definition::new(leaf, Source::Wiktionary);
        definition.context = parents
            .iter()
            .map(|context| Sourced::one(normalize_text(context), Source::Wiktionary))
            .collect();
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
        for topic in sense.topics {
            if reviewed_topic(&topic) {
                definition.qualifiers.push(Sourced::one(
                    Qualifier {
                        category: QualifierCategory::Domain,
                        value: topic,
                    },
                    Source::Wiktionary,
                ));
            } else {
                report.diagnose("unmapped-topic");
            }
        }
        for tag in sense.tags {
            if let Some(qualifier) = map_tag(&tag) {
                definition
                    .qualifiers
                    .push(Sourced::one(qualifier, Source::Wiktionary));
            }
        }
        definition.examples = parse_examples(sense.examples, report);
        definitions.push(definition);
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
        measure_words: Vec::new(),
        rejection: None,
    })
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
    fn preserves_context_and_only_structured_examples() {
        let json = entry_json(
            r#"[{"glosses":["fire-related things","fireworks"],"topics":["arts"],"examples":[{"type":"example","text":"看煙火","english":"watch fireworks"},{"type":"quotation","text":"quoted"},{"text":"unknown"}]}]"#,
            r#"[{"zh_pron":"yānhuǒ","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
        let mut report = SourceReport::default();
        let record = parse_entry(wrapper.raw, wrapper.source_line, &mut report).unwrap();
        assert_eq!(
            record.definitions[0].context[0].value,
            "fire-related things"
        );
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
    fn preserves_wiktionary_semicolons_inside_one_leaf_gloss() {
        let json = entry_json(
            r#"[{"glosses":["quality","perfect; excellent; flawless"]}]"#,
            r#"[{"zh_pron":"yuánmǎn","tags":["Mandarin","Pinyin"]}]"#,
        );
        let wrapper: Wrapper = serde_json::from_str(&json).unwrap();
        let record = parse_entry(wrapper.raw, 7, &mut SourceReport::default()).unwrap();

        assert_eq!(record.definitions.len(), 1);
        assert_eq!(
            record.definitions[0].gloss.value,
            "perfect; excellent; flawless"
        );
        assert_eq!(record.definitions[0].context[0].value, "quality");
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
