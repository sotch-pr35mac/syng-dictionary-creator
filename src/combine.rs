//! Deterministic source-ordered admission and cross-source combination.

use crate::model::{
    AlternativePronunciation, Definition, HskLevel, HskLevels, LexicalId, LexicalUnit, Source,
    Sourced,
};
use crate::sources::{BuildReport, ParsedRecord};
use anyhow::{Result, bail};
use hsk::{HskQuery, HskSystem, levels_all};
use std::collections::{BTreeMap, BTreeSet};

struct UnitBuilder {
    unit: LexicalUnit,
    has_cedict: bool,
}

/// Combines parsed records under source-specific admission rules.
pub(crate) fn combine(
    mut records: Vec<ParsedRecord>,
    report: &mut BuildReport,
) -> Result<Vec<LexicalUnit>> {
    records.sort_by_key(|record| (source_rank(record.source), record.order));
    let mut units = BTreeMap::<LexicalId, UnitBuilder>::new();
    let mut headwords = BTreeMap::<String, BTreeSet<LexicalId>>::new();

    for record in records {
        if let Some(reason) = &record.rejection {
            report
                .sources
                .entry(record.source)
                .or_default()
                .rejected_records += 1;
            report
                .sources
                .entry(record.source)
                .or_default()
                .diagnose(reason.clone());
            continue;
        }
        match record.source {
            Source::CcCedict => {
                if record.valid_tuple().is_some() {
                    admit_exact(record, &mut units, &mut headwords, report)?;
                } else {
                    reject(record, report, "cc-cedict-incomplete-identity");
                }
            }
            Source::Wiktionary => admit_wiktionary(record, &mut units, &mut headwords, report)?,
            Source::ChineseNotes => enrich_from_chinese_notes(record, &mut units, report),
        }
    }

    let mut lexical_units = Vec::with_capacity(units.len());
    for (_, mut builder) in units {
        builder.unit.hsk = hsk_levels(&builder.unit.simplified, &builder.unit.pinyin.numbers);
        lexical_units.push(builder.unit);
    }
    lexical_units.sort_by(|left, right| left.id.cmp(&right.id));
    report.lexical_units = lexical_units.len() as u64;
    report.definitions = lexical_units
        .iter()
        .map(|unit| unit.english.len() as u64)
        .sum();
    Ok(lexical_units)
}

/// Admits a record with a complete identity tuple or merges it into that entity.
fn admit_exact(
    record: ParsedRecord,
    units: &mut BTreeMap<LexicalId, UnitBuilder>,
    headwords: &mut BTreeMap<String, BTreeSet<LexicalId>>,
    report: &mut BuildReport,
) -> Result<()> {
    let (simplified, traditional, pinyin) = record.valid_tuple().expect("checked tuple");
    let identity = LexicalId::new(simplified, traditional, &pinyin.numbers)?;
    if let Some(existing) = units.get(&identity)
        && (existing.unit.simplified != simplified
            || existing.unit.traditional != traditional
            || existing.unit.pinyin.numbers != pinyin.numbers)
    {
        bail!("SHA-256 lexical identity collision for {identity}");
    }
    let builder = units
        .entry(identity.clone())
        .or_insert_with(|| UnitBuilder {
            unit: LexicalUnit {
                id: identity.clone(),
                simplified: simplified.to_owned(),
                traditional: traditional.to_owned(),
                pinyin: pinyin.clone(),
                commonness: 0.0,
                alternative_pronunciations: Vec::new(),
                measure_words: Vec::new(),
                hsk: HskLevels::default(),
                english: Vec::new(),
            },
            has_cedict: false,
        });
    for headword in [&builder.unit.simplified, &builder.unit.traditional] {
        headwords
            .entry(headword.clone())
            .or_default()
            .insert(identity.clone());
    }
    merge_record(builder, record, report);
    Ok(())
}

/// Routes a Wiktionary record without collapsing distinct exact identities.
fn admit_wiktionary(
    mut record: ParsedRecord,
    units: &mut BTreeMap<LexicalId, UnitBuilder>,
    headwords: &mut BTreeMap<String, BTreeSet<LexicalId>>,
    report: &mut BuildReport,
) -> Result<()> {
    if record.pronunciations.is_empty() {
        reject(record, report, "wiktionary-missing-pronunciation");
        return Ok(());
    }
    if record.pronunciations.len() == 1 {
        if record.valid_tuple().is_some() {
            return admit_exact(record, units, headwords, report);
        }
        let candidates = candidate_identities(&record, headwords)
            .iter()
            .filter(|identity| {
                let builder = &units[*identity];
                record_headwords_match(&record, &builder.unit)
                    && record.pronunciations[0].pronunciation.numbers == builder.unit.pinyin.numbers
            })
            .cloned()
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            let builder = units
                .get_mut(&candidates[0])
                .expect("Wiktionary candidate still exists");
            merge_record(builder, record, report);
        } else {
            let reason = if candidates.is_empty() {
                "wiktionary-unassignable-incomplete-record"
            } else {
                "wiktionary-ambiguous-incomplete-record"
            };
            reject(record, report, reason);
        }
        return Ok(());
    }

    let headword_candidates = candidate_identities(&record, headwords);
    let primary_candidates = headword_candidates
        .iter()
        .filter(|identity| {
            let builder = &units[*identity];
            builder.has_cedict
                && record_headwords_match(&record, &builder.unit)
                && record
                    .pronunciations
                    .iter()
                    .any(|value| value.pronunciation.numbers == builder.unit.pinyin.numbers)
        })
        .cloned()
        .collect::<Vec<_>>();
    let candidates =
        if primary_candidates.len() == 1 {
            primary_candidates
        } else if primary_candidates.is_empty() {
            headword_candidates
                .iter()
                .filter(|identity| {
                    let builder = &units[*identity];
                    builder.has_cedict
                        && record_headwords_match(&record, &builder.unit)
                        && record.pronunciations.iter().all(|value| {
                            value.pronunciation.numbers == builder.unit.pinyin.numbers
                                || builder.unit.alternative_pronunciations.iter().any(
                                    |alternative| {
                                        alternative.value.pronunciation.numbers
                                            == value.pronunciation.numbers
                                    },
                                )
                        })
                })
                .cloned()
                .collect()
        } else {
            primary_candidates
        };
    if candidates.len() != 1 {
        let reason = if candidates.is_empty() {
            "wiktionary-unassignable-multiple-pronunciations"
        } else {
            "wiktionary-ambiguous-multiple-pronunciations"
        };
        reject(record, report, reason);
        return Ok(());
    }

    let builder = units
        .get_mut(&candidates[0])
        .expect("Wiktionary candidate still exists");
    for parsed in &record.pronunciations {
        if parsed.pronunciation.numbers == builder.unit.pinyin.numbers {
            continue;
        }
        merge_alternative_pronunciations(
            &mut record.alternative_pronunciations,
            vec![Sourced::one(
                AlternativePronunciation {
                    pronunciation: parsed.pronunciation.clone(),
                    label: parsed
                        .label
                        .clone()
                        .unwrap_or_else(|| "also pr.".to_owned()),
                },
                Source::Wiktionary,
            )],
        );
    }
    merge_record(builder, record, report);
    Ok(())
}

/// Returns the deterministic identity set named by a record's explicit headwords.
fn candidate_identities(
    record: &ParsedRecord,
    headwords: &BTreeMap<String, BTreeSet<LexicalId>>,
) -> BTreeSet<LexicalId> {
    record
        .explicit_headwords
        .iter()
        .filter_map(|headword| headwords.get(headword))
        .flat_map(|identities| identities.iter().cloned())
        .collect()
}

/// Imports only whitelisted metadata from an exact Chinese Notes identity/sense match.
fn enrich_from_chinese_notes(
    record: ParsedRecord,
    units: &mut BTreeMap<LexicalId, UnitBuilder>,
    report: &mut BuildReport,
) {
    let Some((simplified, traditional, pinyin)) = record.valid_tuple() else {
        suppress(record, report, "chinese-notes-incomplete-identity");
        return;
    };
    let Ok(identity) = LexicalId::new(simplified, traditional, &pinyin.numbers) else {
        suppress(record, report, "chinese-notes-invalid-identity");
        return;
    };
    let Some(builder) = units.get_mut(&identity) else {
        suppress(record, report, "chinese-notes-unmatched-identity");
        return;
    };

    let mut published = false;
    for incoming in record.definitions {
        let candidates = builder
            .unit
            .english
            .iter()
            .enumerate()
            .filter(|(_, definition)| definition.gloss.value == incoming.gloss.value)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            report
                .sources
                .entry(Source::ChineseNotes)
                .or_default()
                .diagnose(if candidates.is_empty() {
                    "chinese-notes-unmatched-definition"
                } else {
                    "chinese-notes-ambiguous-definition"
                });
            continue;
        }
        let target = &mut builder.unit.english[candidates[0]];
        let mut changed = false;
        changed |= merge_values(&mut target.parts_of_speech, incoming.parts_of_speech);
        changed |= merge_values(&mut target.lexical_kinds, incoming.lexical_kinds);
        changed |= merge_values(
            &mut target.qualifiers,
            incoming
                .qualifiers
                .into_iter()
                .filter(|qualifier| {
                    qualifier.value.category == crate::model::QualifierCategory::Domain
                })
                .collect(),
        );
        published |= changed;
    }
    let source_report = report.sources.entry(Source::ChineseNotes).or_default();
    if published {
        source_report.admitted_records += 1;
        report.chinese_notes_enriched_records += 1;
    } else {
        source_report.suppressed_records += 1;
    }
}

fn reject(record: ParsedRecord, report: &mut BuildReport, reason: &'static str) {
    let source_report = report.sources.entry(record.source).or_default();
    source_report.rejected_records += 1;
    source_report.diagnose(reason);
}

fn suppress(record: ParsedRecord, report: &mut BuildReport, reason: &'static str) {
    let source_report = report.sources.entry(record.source).or_default();
    source_report.suppressed_records += 1;
    source_report.diagnose(reason);
}

/// Merges one assigned source record while preserving definition order and attribution.
fn merge_record(builder: &mut UnitBuilder, record: ParsedRecord, report: &mut BuildReport) {
    let source_report = report.sources.entry(record.source).or_default();
    let _diagnostic_locator = &record.locator;
    let mut published_anything = false;
    for mut definition in record.definitions {
        deduplicate_definition_metadata(&mut definition);
        let matching_index = builder
            .unit
            .english
            .iter()
            .position(|existing| existing.gloss.value == definition.gloss.value);
        if let Some(index) = matching_index {
            merge_definition(&mut builder.unit.english[index], definition);
            published_anything = true;
            continue;
        }
        builder.unit.english.push(definition);
        source_report.emitted_definitions += 1;
        published_anything = true;
    }
    merge_alternative_pronunciations(
        &mut builder.unit.alternative_pronunciations,
        record.alternative_pronunciations,
    );
    merge_values(&mut builder.unit.measure_words, record.measure_words);
    if !builder.unit.alternative_pronunciations.is_empty() || !builder.unit.measure_words.is_empty()
    {
        published_anything = true;
    }
    if record.source == Source::CcCedict {
        builder.has_cedict = true;
    }
    if published_anything {
        source_report.admitted_records += 1;
    } else {
        source_report.suppressed_records += 1;
    }
}

/// Tests whether incomplete evidence uniquely and noncontradictorily names an entity.
fn record_headwords_match(record: &ParsedRecord, unit: &LexicalUnit) -> bool {
    if record.explicit_headwords.is_empty() {
        return false;
    }
    if record
        .simplified
        .as_ref()
        .is_some_and(|value| value != &unit.simplified)
        || record
            .traditional
            .as_ref()
            .is_some_and(|value| value != &unit.traditional)
    {
        return false;
    }
    record
        .explicit_headwords
        .iter()
        .any(|headword| headword == &unit.simplified || headword == &unit.traditional)
}

/// Aggregates independently sourced metadata for an exact definition match.
fn merge_definition(existing: &mut Definition, mut incoming: Definition) {
    debug_assert_eq!(existing.gloss.value, incoming.gloss.value);
    deduplicate_definition_metadata(existing);
    deduplicate_definition_metadata(&mut incoming);
    merge_sources(&mut existing.gloss.sources, incoming.gloss.sources);
    merge_values(&mut existing.examples, incoming.examples);
    merge_values(&mut existing.commentary, incoming.commentary);
    merge_values(&mut existing.qualifiers, incoming.qualifiers);
    merge_values(&mut existing.lexical_kinds, incoming.lexical_kinds);
    merge_values(&mut existing.parts_of_speech, incoming.parts_of_speech);
    merge_alternative_pronunciations(
        &mut existing.alternative_pronunciations,
        incoming.alternative_pronunciations,
    );
    merge_values(&mut existing.measure_words, incoming.measure_words);
}

/// Collapses metadata that becomes identical after source-specific normalization.
fn deduplicate_definition_metadata(definition: &mut Definition) {
    deduplicate_values(&mut definition.examples);
    deduplicate_values(&mut definition.commentary);
    deduplicate_values(&mut definition.qualifiers);
    deduplicate_values(&mut definition.lexical_kinds);
    deduplicate_values(&mut definition.parts_of_speech);
    deduplicate_alternative_pronunciations(&mut definition.alternative_pronunciations);
    deduplicate_values(&mut definition.measure_words);
}

/// Deduplicates one already-collected attributed value list without changing first-seen order.
fn deduplicate_values<T: Eq>(values: &mut Vec<Sourced<T>>) {
    let incoming = std::mem::take(values);
    merge_values(values, incoming);
}

/// Deduplicates pronunciation values by canonical Pinyin rather than by display label.
fn deduplicate_alternative_pronunciations(values: &mut Vec<Sourced<AlternativePronunciation>>) {
    let incoming = std::mem::take(values);
    merge_alternative_pronunciations(values, incoming);
}

/// Merges labels and attribution for records naming the same canonical pronunciation.
fn merge_alternative_pronunciations(
    existing: &mut Vec<Sourced<AlternativePronunciation>>,
    incoming: Vec<Sourced<AlternativePronunciation>>,
) -> bool {
    let mut changed = false;
    for incoming_value in incoming {
        if let Some(existing_value) = existing.iter_mut().find(|existing_value| {
            existing_value.value.pronunciation == incoming_value.value.pronunciation
        }) {
            changed |= merge_pronunciation_labels(
                &mut existing_value.value.label,
                &incoming_value.value.label,
            );
            changed |= merge_sources(&mut existing_value.sources, incoming_value.sources);
        } else {
            existing.push(incoming_value);
            changed = true;
        }
    }
    changed
}

/// Unions slash-separated pronunciation scope labels in deterministic lexical order.
fn merge_pronunciation_labels(existing: &mut String, incoming: &str) -> bool {
    let labels = existing
        .split('/')
        .chain(incoming.split('/'))
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .collect::<BTreeSet<_>>();
    let merged = labels.into_iter().collect::<Vec<_>>().join("/");
    if *existing == merged {
        false
    } else {
        *existing = merged;
        true
    }
}

/// Deduplicates equal metadata values while aggregating their source lists.
fn merge_values<T: Eq>(existing: &mut Vec<Sourced<T>>, incoming: Vec<Sourced<T>>) -> bool {
    let mut changed = false;
    for incoming_value in incoming {
        if let Some(existing_value) = existing
            .iter_mut()
            .find(|existing_value| existing_value.value == incoming_value.value)
        {
            changed |= merge_sources(&mut existing_value.sources, incoming_value.sources);
        } else {
            existing.push(incoming_value);
            changed = true;
        }
    }
    changed
}

/// Deduplicates source attribution in stable source-priority order.
fn merge_sources(existing: &mut Vec<Source>, incoming: Vec<Source>) -> bool {
    let mut changed = false;
    for source in incoming {
        if !existing.contains(&source) {
            existing.push(source);
            changed = true;
        }
    }
    existing.sort_by_key(|source| source_rank(*source));
    changed
}

/// Resolves all supported HSK memberships for a lexical tuple.
fn hsk_levels(simplified: &str, pinyin: &str) -> HskLevels {
    let matches = levels_all(HskQuery::new(simplified).pinyin(pinyin))
        .or_else(|_| levels_all(HskQuery::new(simplified)))
        .unwrap_or_default();
    let mut result = HskLevels::default();
    for (system, levels) in matches {
        let converted = levels
            .into_iter()
            .map(|level| match level {
                hsk::HskLevel::One => HskLevel::One,
                hsk::HskLevel::Two => HskLevel::Two,
                hsk::HskLevel::Three => HskLevel::Three,
                hsk::HskLevel::Four => HskLevel::Four,
                hsk::HskLevel::Five => HskLevel::Five,
                hsk::HskLevel::Six => HskLevel::Six,
                hsk::HskLevel::SevenToNine => HskLevel::SevenToNine,
            })
            .collect();
        match system {
            HskSystem::Hsk2015 => result.hsk_2015 = converted,
            HskSystem::ProficiencyStandard2021 => {
                result.proficiency_standard_2021 = converted;
            }
            HskSystem::HskExamSyllabus2025 => result.hsk_exam_syllabus_2025 = converted,
            _ => {}
        }
    }
    result
}

/// Returns deterministic precedence for definitions and source attribution.
const fn source_rank(source: Source) -> u8 {
    match source {
        Source::CcCedict => 0,
        Source::Wiktionary => 1,
        Source::ChineseNotes => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AlternativePronunciation, Example, PartOfSpeech, Source};
    use crate::pinyin::from_numbered;
    use crate::sources::ParsedPronunciation;

    fn record(source: Source, pinyin: &str, gloss: &str) -> ParsedRecord {
        ParsedRecord {
            source,
            order: 1,
            locator: "fixture".to_owned(),
            simplified: Some("烟火".to_owned()),
            traditional: Some("煙火".to_owned()),
            explicit_headwords: vec!["烟火".to_owned(), "煙火".to_owned()],
            pronunciations: vec![ParsedPronunciation {
                pronunciation: from_numbered(pinyin).unwrap(),
                label: None,
            }],
            definitions: vec![Definition::new(gloss.to_owned(), source)],
            alternative_pronunciations: Vec::new(),
            measure_words: Vec::new(),
            rejection: None,
        }
    }

    #[test]
    fn never_merges_distinct_tones_or_capitalization() {
        let mut second = record(Source::CcCedict, "yan1 huo5", "fireworks");
        second.order = 2;
        let units = combine(
            vec![record(Source::CcCedict, "yan1 huo3", "fireworks"), second],
            &mut BuildReport::default(),
        )
        .unwrap();
        assert_eq!(units.len(), 2);
    }

    #[test]
    fn chinese_notes_enriches_only_an_exact_identity_and_gloss() {
        let cedict = record(Source::CcCedict, "yan1 huo3", "fireworks");
        let mut notes = record(Source::ChineseNotes, "yan1 huo3", "fireworks");
        notes.definitions[0]
            .parts_of_speech
            .push(Sourced::one(PartOfSpeech::Noun, Source::ChineseNotes));
        let mut report = BuildReport::default();
        let units = combine(vec![cedict, notes], &mut report).unwrap();
        assert_eq!(units[0].english.len(), 1);
        assert_eq!(units[0].english[0].gloss.sources, vec![Source::CcCedict]);
        assert_eq!(
            units[0].english[0].parts_of_speech[0].sources,
            vec![Source::ChineseNotes]
        );
        assert_eq!(report.sources[&Source::CcCedict].admitted_records, 1);
        assert_eq!(report.sources[&Source::ChineseNotes].admitted_records, 1);
        assert_eq!(report.sources[&Source::CcCedict].emitted_definitions, 1);
        assert_eq!(report.sources[&Source::ChineseNotes].emitted_definitions, 0);
    }

    #[test]
    fn chinese_notes_never_adds_a_differing_gloss() {
        let cedict = record(Source::CcCedict, "yan1 huo3", "fireworks");
        let stale = record(Source::ChineseNotes, "yan1 huo3", "firecrackers");
        let mut report = BuildReport::default();
        let units = combine(vec![cedict, stale], &mut report).unwrap();
        assert_eq!(units[0].english.len(), 1);
        assert_eq!(
            report.sources[&Source::ChineseNotes].diagnostics["chinese-notes-unmatched-definition"],
            1
        );
    }

    #[test]
    fn chinese_notes_cannot_seed_an_entity() {
        let units = combine(
            vec![record(Source::ChineseNotes, "yan1 huo3", "fireworks")],
            &mut BuildReport::default(),
        )
        .unwrap();
        assert!(units.is_empty());
    }

    #[test]
    fn incomplete_records_attach_only_to_one_exact_tuple() {
        let anchor = record(Source::CcCedict, "yan1 huo3", "fireworks");
        let mut wiktionary = record(Source::Wiktionary, "yan1 huo3", "pyrotechnics");
        wiktionary.simplified = None;
        wiktionary.traditional = None;
        wiktionary.explicit_headwords = vec!["煙火".to_owned()];
        let units = combine(vec![anchor, wiktionary], &mut BuildReport::default()).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].english.len(), 2);
    }

    #[test]
    fn multiple_wiktionary_readings_enrich_one_cedict_owner() {
        let mut cedict = record(Source::CcCedict, "ta1 shi5", "steady");
        cedict.simplified = Some("踏实".to_owned());
        cedict.traditional = Some("踏實".to_owned());
        cedict.explicit_headwords = vec!["踏实".to_owned(), "踏實".to_owned()];
        cedict.alternative_pronunciations.push(Sourced::one(
            AlternativePronunciation {
                pronunciation: from_numbered("ta4 shi2").unwrap(),
                label: "Taiwan pr.".to_owned(),
            },
            Source::CcCedict,
        ));
        let mut adjective = record(Source::Wiktionary, "ta1 shi5", "dependable");
        adjective.simplified = Some("踏实".to_owned());
        adjective.traditional = Some("踏實".to_owned());
        adjective.explicit_headwords = vec!["踏实".to_owned(), "踏實".to_owned()];
        adjective.pronunciations.push(ParsedPronunciation {
            pronunciation: from_numbered("ta4 shi2").unwrap(),
            label: Some("Taiwan pr.".to_owned()),
        });
        let mut verb = record(Source::Wiktionary, "ta4 shi2", "to walk steadily");
        verb.simplified = Some("踏实".to_owned());
        verb.traditional = Some("踏實".to_owned());
        verb.explicit_headwords = vec!["踏实".to_owned(), "踏實".to_owned()];

        let units = combine(vec![cedict, adjective, verb], &mut BuildReport::default()).unwrap();
        assert_eq!(units.len(), 2);
        let modern = units
            .iter()
            .find(|unit| unit.pinyin.numbers == "ta1shi5")
            .unwrap();
        assert_eq!(modern.english.len(), 2);
        assert!(modern.alternative_pronunciations.iter().any(|alternative| {
            alternative.value.pronunciation.numbers == "ta4shi2"
                && alternative.value.label == "Taiwan pr."
        }));
        assert!(units.iter().any(|unit| {
            unit.pinyin.numbers == "ta4shi2" && unit.english[0].gloss.value == "to walk steadily"
        }));
    }

    #[test]
    fn normalized_examples_are_deduplicated_before_publication() {
        let mut wiktionary = record(Source::Wiktionary, "yuan2 man3", "satisfactory");
        wiktionary.simplified = Some("圆满".to_owned());
        wiktionary.traditional = Some("圓滿".to_owned());
        wiktionary.explicit_headwords = vec!["圆满".to_owned(), "圓滿".to_owned()];
        let example = Example {
            simplified: Some("结局非常圆满。".to_owned()),
            traditional: Some("結局非常圓滿。".to_owned()),
            english: Some("The ending is very satisfactory.".to_owned()),
        };
        wiktionary.definitions[0].examples = vec![
            Sourced::one(example.clone(), Source::Wiktionary),
            Sourced::one(example, Source::Wiktionary),
        ];

        let units = combine(vec![wiktionary], &mut BuildReport::default()).unwrap();

        assert_eq!(units[0].english[0].examples.len(), 1);
        assert_eq!(
            units[0].english[0].examples[0].sources,
            vec![Source::Wiktionary]
        );
    }

    #[test]
    fn alternative_pronunciation_identity_ignores_scope_label() {
        let pronunciation = from_numbered("cong1 rong2").unwrap();
        let mut alternatives = vec![Sourced::one(
            AlternativePronunciation {
                pronunciation: pronunciation.clone(),
                label: "Taiwan pr.".to_owned(),
            },
            Source::CcCedict,
        )];

        merge_alternative_pronunciations(
            &mut alternatives,
            vec![Sourced::one(
                AlternativePronunciation {
                    pronunciation,
                    label: "Mainland China pr./Taiwan pr.".to_owned(),
                },
                Source::Wiktionary,
            )],
        );

        assert_eq!(alternatives.len(), 1);
        assert_eq!(alternatives[0].value.label, "Mainland China pr./Taiwan pr.");
        assert_eq!(
            alternatives[0].sources,
            vec![Source::CcCedict, Source::Wiktionary]
        );
    }
}
