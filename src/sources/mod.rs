mod cedict;
mod chinese_notes;
mod wiktionary;

use crate::lock::SourceLock;
use crate::model::{AlternativePronunciation, Definition, LexicalId, Pinyin, Source, Sourced};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct ParsedRecord {
    pub source: Source,
    pub order: u64,
    pub locator: String,
    pub simplified: Option<String>,
    pub traditional: Option<String>,
    pub explicit_headwords: Vec<String>,
    pub pinyin: Option<Pinyin>,
    pub definitions: Vec<Definition>,
    pub alternative_pronunciations: Vec<Sourced<AlternativePronunciation>>,
    pub measure_words: Vec<Sourced<LexicalId>>,
    pub cited_cedict: bool,
    pub rejection: Option<String>,
}

impl ParsedRecord {
    pub fn valid_tuple(&self) -> Option<(&str, &str, &Pinyin)> {
        Some((
            self.simplified.as_deref()?,
            self.traditional.as_deref()?,
            self.pinyin.as_ref()?,
        ))
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceReport {
    pub input_records: u64,
    pub admitted_records: u64,
    pub suppressed_records: u64,
    pub rejected_records: u64,
    pub emitted_definitions: u64,
    pub excluded_quotations: u64,
    pub excluded_unknown_examples: u64,
    pub diagnostics: BTreeMap<String, u64>,
}

impl SourceReport {
    pub(crate) fn diagnose(&mut self, code: impl Into<String>) {
        *self.diagnostics.entry(code.into()).or_default() += 1;
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BuildReport {
    pub sources: BTreeMap<Source, SourceReport>,
    pub lexical_units: u64,
    pub definitions: u64,
    pub suppressed_stale_chinese_notes_glosses: u64,
}

pub(crate) fn parse_all(
    source_lock: &SourceLock,
    cache_directory: &Path,
) -> Result<(Vec<ParsedRecord>, BuildReport)> {
    let mut records = Vec::new();
    let mut report = BuildReport::default();

    let cedict_path = artifact_path(source_lock, cache_directory, Source::CcCedict, "dictionary")?;
    let cedict_file =
        File::open(&cedict_path).with_context(|| format!("open {}", cedict_path.display()))?;
    records.extend(cedict::parse(
        BufReader::new(cedict_file),
        report_for(&mut report, Source::CcCedict),
    )?);

    let notes_path = artifact_path(
        source_lock,
        cache_directory,
        Source::ChineseNotes,
        "dictionary",
    )?;
    let notes_file =
        File::open(&notes_path).with_context(|| format!("open {}", notes_path.display()))?;
    records.extend(chinese_notes::parse(
        BufReader::new(notes_file),
        report_for(&mut report, Source::ChineseNotes),
    )?);

    let wiktionary_path = artifact_path(
        source_lock,
        cache_directory,
        Source::Wiktionary,
        "filtered-chinese-jsonl",
    )?;
    let wiktionary_file = File::open(&wiktionary_path)
        .with_context(|| format!("open {}", wiktionary_path.display()))?;
    let decoder = flate2::read::GzDecoder::new(wiktionary_file);
    records.extend(wiktionary::parse(
        BufReader::new(decoder),
        report_for(&mut report, Source::Wiktionary),
    )?);

    Ok((records, report))
}

fn report_for(report: &mut BuildReport, source: Source) -> &mut SourceReport {
    report.sources.entry(source).or_default()
}

fn artifact_path(
    source_lock: &SourceLock,
    cache_directory: &Path,
    source: Source,
    role: &str,
) -> Result<PathBuf> {
    let pin = source_lock
        .artifacts
        .iter()
        .find(|pin| pin.source == source && pin.role == role)
        .with_context(|| format!("source lock has no {source:?}/{role} artifact"))?;
    let path = cache_directory.join(&pin.cache_file);
    if !path.exists() {
        bail!("missing cached source {}", path.display());
    }
    Ok(path)
}

pub(crate) fn rejected_record(
    source: Source,
    order: u64,
    locator: String,
    reason: impl Into<String>,
) -> ParsedRecord {
    ParsedRecord {
        source,
        order,
        locator,
        simplified: None,
        traditional: None,
        explicit_headwords: Vec::new(),
        pinyin: None,
        definitions: Vec::new(),
        alternative_pronunciations: Vec::new(),
        measure_words: Vec::new(),
        cited_cedict: false,
        rejection: Some(reason.into()),
    }
}
