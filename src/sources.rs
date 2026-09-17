//! Build-time adapters and diagnostics for the pinned dictionary sources.

mod cedict;
mod chinese_notes;
mod wiktionary;

use crate::lock::{LockedArtifactSource, SourceLock};
use crate::model::{AlternativePronunciation, Definition, LexicalId, Pinyin, Source, Sourced};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Debug)]
pub(crate) struct ParsedPronunciation {
    pub pronunciation: Pinyin,
    pub label: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedRecord {
    pub source: Source,
    pub order: u64,
    pub locator: String,
    pub simplified: Option<String>,
    pub traditional: Option<String>,
    pub explicit_headwords: Vec<String>,
    pub pronunciations: Vec<ParsedPronunciation>,
    pub definitions: Vec<Definition>,
    pub alternative_pronunciations: Vec<Sourced<AlternativePronunciation>>,
    pub measure_words: Vec<Sourced<LexicalId>>,
    pub rejection: Option<String>,
}

impl ParsedRecord {
    /// Returns the complete identity tuple when exactly one pronunciation exists.
    pub fn valid_tuple(&self) -> Option<(&str, &str, &Pinyin)> {
        let [pronunciation] = self.pronunciations.as_slice() else {
            return None;
        };
        Some((
            self.simplified.as_deref()?,
            self.traditional.as_deref()?,
            &pronunciation.pronunciation,
        ))
    }
}

/// Source-scoped build counts and categorized diagnostics.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceReport {
    /// Number of raw records presented to the source adapter.
    pub input_records: u64,
    /// Number of records that contributed published content.
    pub admitted_records: u64,
    /// Number of valid records intentionally suppressed by combination policy.
    pub suppressed_records: u64,
    /// Number of records rejected because they could not be published safely.
    pub rejected_records: u64,
    /// Number of definitions first introduced into the combined output.
    pub emitted_definitions: u64,
    /// Number of Wiktionary quotations excluded from the artifact.
    pub excluded_quotations: u64,
    /// Number of example-like values excluded because their type was not reviewed.
    pub excluded_unknown_examples: u64,
    /// Stable diagnostic code counts for rejected or excluded source material.
    pub diagnostics: BTreeMap<String, u64>,
}

impl SourceReport {
    /// Increments one stable diagnostic code.
    pub(crate) fn diagnose(&mut self, code: impl Into<String>) {
        *self.diagnostics.entry(code.into()).or_default() += 1;
    }
}

/// Aggregate accounting for one complete dictionary build.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BuildReport {
    /// Per-source input and outcome counts.
    pub sources: BTreeMap<Source, SourceReport>,
    /// Number of lexical units in the completed combination.
    pub lexical_units: u64,
    /// Number of definitions in the completed combination.
    pub definitions: u64,
    /// Number of Chinese Notes records that enriched an exact existing sense.
    pub chinese_notes_enriched_records: u64,
    /// Number of identities present in the generated commonness database.
    pub commonness_database_records: u64,
    /// Number of published lexical units assigned a score greater than zero.
    pub commonness_nonzero_units: u64,
    /// SHA-256 digest of the generated commonness database used for this build.
    pub commonness_database_sha256: String,
}

/// Parses every required source artifact in deterministic source order.
pub(crate) fn parse_all(
    source_lock: &SourceLock,
    cache_directory: &Path,
) -> Result<(Vec<ParsedRecord>, BuildReport)> {
    let mut records = Vec::new();
    let mut report = BuildReport::default();

    let stage_started = Instant::now();
    eprintln!("Parsing CC-CEDICT...");
    let cedict_path = artifact_path(
        source_lock,
        cache_directory,
        LockedArtifactSource::CcCedict,
        "dictionary",
    )?;
    let cedict_file =
        File::open(&cedict_path).with_context(|| format!("open {}", cedict_path.display()))?;
    let parsed = cedict::parse(
        BufReader::new(cedict_file),
        report_for(&mut report, Source::CcCedict),
    )?;
    eprintln!(
        "Parsed {} CC-CEDICT records in {:.1?}.",
        parsed.len(),
        stage_started.elapsed()
    );
    records.extend(parsed);

    let stage_started = Instant::now();
    eprintln!("Parsing Chinese Notes...");
    let notes_path = artifact_path(
        source_lock,
        cache_directory,
        LockedArtifactSource::ChineseNotes,
        "dictionary",
    )?;
    let notes_file =
        File::open(&notes_path).with_context(|| format!("open {}", notes_path.display()))?;
    let parsed = chinese_notes::parse(
        BufReader::new(notes_file),
        report_for(&mut report, Source::ChineseNotes),
    )?;
    eprintln!(
        "Parsed {} Chinese Notes records in {:.1?}.",
        parsed.len(),
        stage_started.elapsed()
    );
    records.extend(parsed);

    let stage_started = Instant::now();
    eprintln!("Parsing Wiktionary...");
    let wiktionary_path = artifact_path(
        source_lock,
        cache_directory,
        LockedArtifactSource::Wiktionary,
        "filtered-chinese-jsonl",
    )?;
    let wiktionary_file = File::open(&wiktionary_path)
        .with_context(|| format!("open {}", wiktionary_path.display()))?;
    let decoder = flate2::read::GzDecoder::new(wiktionary_file);
    let parsed = wiktionary::parse(
        BufReader::new(decoder),
        report_for(&mut report, Source::Wiktionary),
    )?;
    eprintln!(
        "Parsed {} Wiktionary records in {:.1?}.",
        parsed.len(),
        stage_started.elapsed()
    );
    records.extend(parsed);

    Ok((records, report))
}

/// Returns the report bucket for a source, creating it on first use.
fn report_for(report: &mut BuildReport, source: Source) -> &mut SourceReport {
    report.sources.entry(source).or_default()
}

/// Locates one required cached artifact without triggering an implicit fetch.
pub(crate) fn artifact_path(
    source_lock: &SourceLock,
    cache_directory: &Path,
    source: LockedArtifactSource,
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

/// Constructs a rejected record while preserving its diagnostic provenance.
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
        pronunciations: Vec::new(),
        definitions: Vec::new(),
        alternative_pronunciations: Vec::new(),
        measure_words: Vec::new(),
        rejection: Some(reason.into()),
    }
}
