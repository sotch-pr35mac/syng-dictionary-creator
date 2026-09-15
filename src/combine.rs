//! Deterministic two-pass admission and cross-source combination.

use crate::model::{Definition, HskLevel, HskLevels, LexicalId, LexicalUnit, Source, Sourced};
use crate::sources::{BuildReport, ParsedRecord};
use anyhow::{Result, bail};
use hsk::{HskQuery, HskSystem, levels_all};
use std::collections::{BTreeMap, BTreeSet};

struct UnitBuilder {
    unit: LexicalUnit,
    has_cedict: bool,
}

/// Combines parsed records by exact persistent identity and accounts for every outcome.
pub(crate) fn combine(
    mut records: Vec<ParsedRecord>,
    report: &mut BuildReport,
) -> Result<Vec<LexicalUnit>> {
    records.sort_by_key(|record| (source_rank(record.source), record.order));
    let mut units = BTreeMap::<LexicalId, UnitBuilder>::new();
    let mut incomplete = Vec::new();

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
        if record.valid_tuple().is_none() {
            incomplete.push(record);
            continue;
        }
        admit(record, &mut units, report)?;
    }

    for record in incomplete {
        let candidates = units
            .iter()
            .filter(|(_, builder)| record_matches_existing(&record, &builder.unit))
            .map(|(identity, _)| identity.clone())
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            let identity = &candidates[0];
            let builder = units.get_mut(identity).expect("candidate still exists");
            merge_record(builder, record, report);
        } else {
            let source_report = report.sources.entry(record.source).or_default();
            source_report.rejected_records += 1;
            source_report.diagnose(if candidates.is_empty() {
                "unassignable-incomplete-record"
            } else {
                "ambiguous-incomplete-record"
            });
        }
    }

    let identities = units.keys().cloned().collect::<BTreeSet<_>>();
    let mut lexical_units = Vec::with_capacity(units.len());
    for (_, mut builder) in units {
        remove_entity_alternatives(&mut builder.unit, &identities)?;
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
fn admit(
    record: ParsedRecord,
    units: &mut BTreeMap<LexicalId, UnitBuilder>,
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
                id: identity,
                simplified: simplified.to_owned(),
                traditional: traditional.to_owned(),
                pinyin: pinyin.clone(),
                alternative_pronunciations: Vec::new(),
                measure_words: Vec::new(),
                hsk: HskLevels::default(),
                english: Vec::new(),
            },
            has_cedict: false,
        });
    merge_record(builder, record, report);
    Ok(())
}

/// Merges one assigned source record while preserving definition order and attribution.
fn merge_record(builder: &mut UnitBuilder, record: ParsedRecord, report: &mut BuildReport) {
    let source_report = report.sources.entry(record.source).or_default();
    let _diagnostic_locator = &record.locator;
    let mut published_anything = false;
    for definition in record.definitions {
        let matching_index = builder.unit.english.iter().position(|existing| {
            existing.gloss.value == definition.gloss.value
                && existing
                    .context
                    .iter()
                    .map(|value| &value.value)
                    .eq(definition.context.iter().map(|value| &value.value))
        });
        if let Some(index) = matching_index {
            merge_definition(&mut builder.unit.english[index], definition);
            published_anything = true;
            continue;
        }
        if record.source == Source::ChineseNotes
            && record.cited_cedict
            && builder.has_cedict
            && !has_non_english_enrichment(&definition)
        {
            report.suppressed_stale_chinese_notes_glosses += 1;
            continue;
        }
        builder.unit.english.push(definition);
        source_report.emitted_definitions += 1;
        published_anything = true;
    }
    merge_values(
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
fn record_matches_existing(record: &ParsedRecord, unit: &LexicalUnit) -> bool {
    let Some(pinyin) = &record.pinyin else {
        return false;
    };
    if pinyin.numbers != unit.pinyin.numbers || record.explicit_headwords.is_empty() {
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
fn merge_definition(existing: &mut Definition, incoming: Definition) {
    merge_sources(&mut existing.gloss.sources, incoming.gloss.sources);
    merge_values(&mut existing.context, incoming.context);
    merge_values(&mut existing.examples, incoming.examples);
    merge_values(&mut existing.commentary, incoming.commentary);
    merge_values(&mut existing.qualifiers, incoming.qualifiers);
    merge_values(&mut existing.lexical_kinds, incoming.lexical_kinds);
    merge_values(&mut existing.parts_of_speech, incoming.parts_of_speech);
    merge_values(
        &mut existing.alternative_pronunciations,
        incoming.alternative_pronunciations,
    );
    merge_values(&mut existing.measure_words, incoming.measure_words);
}

/// Deduplicates equal metadata values while aggregating their source lists.
fn merge_values<T: Eq>(existing: &mut Vec<Sourced<T>>, incoming: Vec<Sourced<T>>) {
    for incoming_value in incoming {
        if let Some(existing_value) = existing
            .iter_mut()
            .find(|existing_value| existing_value.value == incoming_value.value)
        {
            merge_sources(&mut existing_value.sources, incoming_value.sources);
        } else {
            existing.push(incoming_value);
        }
    }
}

/// Deduplicates source attribution in stable source-priority order.
fn merge_sources(existing: &mut Vec<Source>, incoming: Vec<Source>) {
    for source in incoming {
        if !existing.contains(&source) {
            existing.push(source);
        }
    }
    existing.sort_by_key(|source| source_rank(*source));
}

/// Tests whether a Chinese Notes definition contributes more than a differing gloss.
fn has_non_english_enrichment(definition: &Definition) -> bool {
    !definition.context.is_empty()
        || !definition.examples.is_empty()
        || !definition.commentary.is_empty()
        || !definition.qualifiers.is_empty()
        || !definition.lexical_kinds.is_empty()
        || !definition.parts_of_speech.is_empty()
        || !definition.alternative_pronunciations.is_empty()
        || !definition.measure_words.is_empty()
}

/// Removes alternates whose exact tuple is already represented by a lexical entity.
fn remove_entity_alternatives(
    unit: &mut LexicalUnit,
    identities: &BTreeSet<LexicalId>,
) -> Result<()> {
    let simplified = &unit.simplified;
    let traditional = &unit.traditional;
    unit.alternative_pronunciations.retain(|alternative| {
        LexicalId::new(
            simplified,
            traditional,
            &alternative.value.pronunciation.numbers,
        )
        .map_or(true, |identity| !identities.contains(&identity))
    });
    for definition in &mut unit.english {
        definition.alternative_pronunciations.retain(|alternative| {
            LexicalId::new(
                simplified,
                traditional,
                &alternative.value.pronunciation.numbers,
            )
            .map_or(true, |identity| !identities.contains(&identity))
        });
    }
    Ok(())
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
        Source::ChineseNotes => 1,
        Source::Wiktionary => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PartOfSpeech, Source};
    use crate::pinyin::from_numbered;

    fn record(source: Source, pinyin: &str, gloss: &str) -> ParsedRecord {
        ParsedRecord {
            source,
            order: 1,
            locator: "fixture".to_owned(),
            simplified: Some("烟火".to_owned()),
            traditional: Some("煙火".to_owned()),
            explicit_headwords: vec!["烟火".to_owned(), "煙火".to_owned()],
            pinyin: Some(from_numbered(pinyin).unwrap()),
            definitions: vec![Definition::new(gloss.to_owned(), source)],
            alternative_pronunciations: Vec::new(),
            measure_words: Vec::new(),
            cited_cedict: false,
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
    fn exact_glosses_share_attribution_but_metadata_stays_sourced() {
        let cedict = record(Source::CcCedict, "yan1 huo3", "fireworks");
        let mut notes = record(Source::ChineseNotes, "yan1 huo3", "fireworks");
        notes.definitions[0]
            .parts_of_speech
            .push(Sourced::one(PartOfSpeech::Noun, Source::ChineseNotes));
        let units = combine(vec![cedict, notes], &mut BuildReport::default()).unwrap();
        assert_eq!(units[0].english.len(), 1);
        assert_eq!(
            units[0].english[0].gloss.sources,
            vec![Source::CcCedict, Source::ChineseNotes]
        );
        assert_eq!(
            units[0].english[0].parts_of_speech[0].sources,
            vec![Source::ChineseNotes]
        );
    }

    #[test]
    fn suppresses_only_unenriched_cited_stale_glosses() {
        let cedict = record(Source::CcCedict, "yan1 huo3", "fireworks");
        let mut stale = record(Source::ChineseNotes, "yan1 huo3", "firecrackers");
        stale.cited_cedict = true;
        let mut report = BuildReport::default();
        let units = combine(vec![cedict, stale], &mut report).unwrap();
        assert_eq!(units[0].english.len(), 1);
        assert_eq!(report.suppressed_stale_chinese_notes_glosses, 1);
    }

    #[test]
    fn source_only_entities_are_admitted() {
        let units = combine(
            vec![record(Source::ChineseNotes, "yan1 huo3", "fireworks")],
            &mut BuildReport::default(),
        )
        .unwrap();
        assert_eq!(units.len(), 1);
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
}
