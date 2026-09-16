//! Serialization, licensing notices, publication, and validation of output bundles.

use crate::dictionary_archive::{
    ARCHIVE_CONTRACT, ArchivedChinesePostings, ArchivedDictionaryArchive, ArchivedPostingRange,
    ChinesePostings, DictionaryArchive, IdentityMap, PostingRange,
};
use crate::english::{self, EnglishSearchReport};
use crate::lock::{LockedArtifactSource, SourceLock};
use crate::model::{IDENTITY_VERSION, LexicalId, LexicalUnit, SCHEMA_VERSION, Sourced};
use crate::pinyin::from_numbered;
use crate::sources::BuildReport;
use anyhow::{Context, Result, bail};
use fst::Map;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const CHECKSUMMED_FILES: &[&str] = &[
    "dictionary.rkyv.zst",
    "english.search.zst",
    "LICENSE-DATA.txt",
    "LICENSE-WORDNET.txt",
    "NOTICE.md",
    "wiktionary-attribution.json",
];
const LEGACY_DICTIONARY_FILES: &[&str] = &[
    "data.dictionary",
    "data.dictionary.zst",
    "simplified.dictionary",
    "simplified.dictionary.zst",
    "traditional.dictionary",
    "traditional.dictionary.zst",
    "pinyin.dictionary",
    "pinyin.dictionary.zst",
    "identity.dictionary",
    "identity.dictionary.zst",
    "chinese.fst",
];
const ZSTD_LEVEL: i32 = 20;
const BUNDLE_LICENSE: &str = "CC-BY-SA-4.0";
const BUNDLE_LICENSE_URL: &str = "https://creativecommons.org/licenses/by-sa/4.0/";
const BUNDLE_MODIFICATIONS: &str = "Syng Dictionary Creator parsed, normalized, filtered, structurally annotated, deduplicated exact matches, merged source material, assigned stable identities, and built search indexes. It excluded Wiktionary quotations.";
const WIKTIONARY_ATTRIBUTION_EXPLANATION: &str = "Each key identifies a LexicalUnit containing English Wiktionary material. The linked entry pages provide the contributor history used for author attribution.";
const LICENSE_DATA_TEXT: &str = include_str!("../LICENSE-DATA");
const WORDNET_LICENSE_TEXT: &str = include_str!("../LICENSE-WORDNET.txt");

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
    archive_contract: String,
    creator_version: String,
    bundle_license: String,
    bundle_license_url: String,
    modification_notice: String,
    source_pins: Vec<ManifestSource>,
    record_counts: BTreeMap<String, u64>,
    file_checksums: BTreeMap<String, String>,
    english_search: EnglishSearchReport,
    attribution_requirement: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestSource {
    source: LockedArtifactSource,
    role: String,
    revision: String,
    url: String,
    content_sha256: String,
    license: String,
    license_url: String,
    license_evidence_url: String,
    copyright_notice: String,
    attribution: String,
    modifications: String,
    parser_version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct WiktionaryAttribution {
    schema_version: u32,
    explanation: String,
    entries: BTreeMap<LexicalId, Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReportFile {
    schema_version: u32,
    report: BuildReport,
    file_checksums: BTreeMap<String, String>,
    english_search: EnglishSearchReport,
}

type RuntimeKey = u32;
type DataMap = BTreeMap<RuntimeKey, LexicalUnit>;
type SearchIndex = BTreeMap<String, Vec<RuntimeKey>>;
type IdentityIndex = BTreeMap<LexicalId, RuntimeKey>;

/// Writes, validates, and atomically publishes a complete bundle directory.
pub(crate) fn write_bundle(
    output_directory: &Path,
    cache_directory: &Path,
    source_lock: &SourceLock,
    lexical_units: Vec<LexicalUnit>,
    report: BuildReport,
) -> Result<()> {
    let started = Instant::now();
    let lexical_unit_count = lexical_units.len();
    let temporary_directory = temporary_output_path(output_directory);
    if temporary_directory.exists() {
        fs::remove_dir_all(&temporary_directory).with_context(|| {
            format!(
                "remove stale temporary output {}",
                temporary_directory.display()
            )
        })?;
    }
    fs::create_dir_all(&temporary_directory).with_context(|| {
        format!(
            "create temporary output directory {}",
            temporary_directory.display()
        )
    })?;

    eprintln!(
        "Writing {lexical_unit_count} lexical units to temporary bundle {}...",
        temporary_directory.display()
    );
    let bundle_result = write_temporary_bundle(
        &temporary_directory,
        cache_directory,
        source_lock,
        lexical_units,
        report,
    )
    .and_then(|()| {
        eprintln!("Validating temporary bundle...");
        validate_bundle(&temporary_directory)
    });
    if let Err(error) = bundle_result {
        let _ = fs::remove_dir_all(&temporary_directory);
        return Err(error);
    }

    eprintln!("Publishing validated bundle atomically...");
    publish_directory(&temporary_directory, output_directory)?;
    eprintln!(
        "Published {} in {:.1?}.",
        output_directory.display(),
        started.elapsed()
    );
    Ok(())
}

/// Builds every ordered artifact inside an unpublished temporary directory.
fn write_temporary_bundle(
    directory: &Path,
    cache_directory: &Path,
    source_lock: &SourceLock,
    lexical_units: Vec<LexicalUnit>,
    report: BuildReport,
) -> Result<()> {
    let mut data = DataMap::new();
    let mut identity = IdentityIndex::new();
    let mut pinyin = SearchIndex::new();
    let mut wiktionary_entries = BTreeMap::new();

    for (runtime_index, unit) in lexical_units.into_iter().enumerate() {
        let runtime_key =
            u32::try_from(runtime_index).context("more than u32::MAX lexical units")?;
        if has_source(&unit, crate::model::Source::Wiktionary) {
            let mut urls = BTreeSet::new();
            for headword in [&unit.simplified, &unit.traditional] {
                urls.insert(format!(
                    "https://en.wiktionary.org/wiki/{}",
                    utf8_percent_encode(headword, NON_ALPHANUMERIC)
                ));
            }
            wiktionary_entries.insert(unit.id.clone(), urls.into_iter().collect());
        }
        identity.insert(unit.id.clone(), runtime_key);
        for key in pinyin_keys(&unit) {
            insert_index(&mut pinyin, key, runtime_key);
        }
        data.insert(runtime_key, unit);
    }
    let (simplified, traditional) = headword_indexes(&data);
    sort_index_values(&mut pinyin);

    let wordnet_path = crate::sources::artifact_path(
        source_lock,
        cache_directory,
        LockedArtifactSource::PrincetonWordNet,
        "morphology",
    )?;
    eprintln!("Compiling English lexical search...");
    let english_search = english::build(&data, &wordnet_path)?;
    for (section, bytes) in &english_search.report.section_raw_bytes {
        eprintln!("  {section}: {bytes} bytes");
    }
    eprintln!(
        "English search records: {} tokens, {} texts, {} bindings, {} occurrences, {} direct hits, {} morphology families, {} morphology surfaces",
        english_search.report.tokens,
        english_search.report.texts,
        english_search.report.bindings,
        english_search.report.occurrences,
        english_search.report.direct_hits,
        english_search.report.morphology_families,
        english_search.report.morphology_surfaces,
    );
    let ratio = 100.0 * english_search.report.compressed_bytes as f64
        / english_search.report.uncompressed_bytes.max(1) as f64;
    eprintln!(
        "English search: {} -> {} bytes ({ratio:.1}% compressed), sha256 {}",
        english_search.report.uncompressed_bytes,
        english_search.report.compressed_bytes,
        english_search.report.compressed_sha256,
    );
    let decoded = zstd::stream::decode_all(english_search.compressed.as_slice())?;
    if decoded != english_search.raw {
        bail!("English search compression did not round-trip");
    }
    if english_search.report.compressed_bytes > english::MAXIMUM_SIZE {
        bail!(
            "english.search.zst is {} bytes, above the 22 MiB publication gate",
            english_search.report.compressed_bytes
        );
    }
    fs::write(
        directory.join("english.search.zst"),
        &english_search.compressed,
    )
    .context("write english.search.zst")?;
    let (chinese_fst, chinese_postings, chinese_runtime_keys) =
        build_chinese_index(&simplified, &traditional)?;
    let (pinyin_fst, pinyin_postings, pinyin_runtime_keys) = build_index(&pinyin)?;
    let mut identity_entries = identity
        .iter()
        .map(|(lexical_id, runtime_key)| (*lexical_id.digest(), *runtime_key))
        .collect::<Vec<_>>();
    identity_entries.sort_unstable_by_key(|(digest, _)| *digest);
    let archive = DictionaryArchive::new(
        data.into_values().collect(),
        IdentityMap::new(identity_entries),
        chinese_fst,
        chinese_postings,
        chinese_runtime_keys,
        pinyin_fst,
        pinyin_postings,
        pinyin_runtime_keys,
    );
    write_archive(directory, &archive)?;
    fs::write(directory.join("LICENSE-DATA.txt"), LICENSE_DATA_TEXT)
        .context("write LICENSE-DATA.txt")?;
    fs::write(directory.join("LICENSE-WORDNET.txt"), WORDNET_LICENSE_TEXT)
        .context("write LICENSE-WORDNET.txt")?;
    fs::write(directory.join("NOTICE.md"), notice(source_lock)).context("write NOTICE.md")?;
    write_json(
        directory.join("wiktionary-attribution.json"),
        &WiktionaryAttribution {
            schema_version: SCHEMA_VERSION,
            explanation: WIKTIONARY_ATTRIBUTION_EXPLANATION.to_owned(),
            entries: wiktionary_entries,
        },
    )?;

    let file_checksums = checksums(directory, CHECKSUMMED_FILES)?;
    let source_pins = source_lock
        .artifacts
        .iter()
        .map(|pin| ManifestSource {
            source: pin.source,
            role: pin.role.clone(),
            revision: pin.revision.clone(),
            url: pin.url.clone(),
            content_sha256: pin.content_sha256.clone(),
            license: pin.license.clone(),
            license_url: pin.license_url.clone(),
            license_evidence_url: pin.license_evidence_url.clone(),
            copyright_notice: pin.copyright_notice.clone(),
            attribution: pin.attribution.clone(),
            modifications: pin.modifications.clone(),
            parser_version: pin.parser_version,
        })
        .collect();
    let record_counts = report
        .sources
        .iter()
        .flat_map(|(source, source_report)| {
            [
                (format!("{source:?}.input"), source_report.input_records),
                (
                    format!("{source:?}.admitted"),
                    source_report.admitted_records,
                ),
                (
                    format!("{source:?}.suppressed"),
                    source_report.suppressed_records,
                ),
                (
                    format!("{source:?}.rejected"),
                    source_report.rejected_records,
                ),
            ]
        })
        .collect();
    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        archive_contract: String::from_utf8_lossy(ARCHIVE_CONTRACT).into_owned(),
        creator_version: env!("CARGO_PKG_VERSION").to_owned(),
        bundle_license: BUNDLE_LICENSE.to_owned(),
        bundle_license_url: BUNDLE_LICENSE_URL.to_owned(),
        modification_notice: BUNDLE_MODIFICATIONS.to_owned(),
        source_pins,
        record_counts,
        file_checksums: file_checksums.clone(),
        english_search: english_search.report.clone(),
        attribution_requirement: "Redistributions must remain under CC-BY-SA-4.0 for the CC-licensed adapted material, preserve the separate WordNet license for WordNet-derived morphology, and include LICENSE-DATA.txt, LICENSE-WORDNET.txt, NOTICE.md, manifest.json, and wiktionary-attribution.json. Do not imply endorsement by any upstream project or contributor."
            .to_owned(),
    };
    write_json(directory.join("manifest.json"), &manifest)?;
    write_json(
        directory.join("build-report.json"),
        &ReportFile {
            schema_version: SCHEMA_VERSION,
            report,
            file_checksums,
            english_search: english_search.report,
        },
    )?;
    Ok(())
}

/// Renders the redistributable source and licensing notice from locked metadata.
fn notice(source_lock: &SourceLock) -> String {
    let mut notice = format!(
        "# Syng dictionary data notices\n\nThe generated dictionary bundle is an adapted database licensed under [{BUNDLE_LICENSE}]({BUNDLE_LICENSE_URL}). The creator software is separate and licensed under GPL-3.0-only.\n\n**Changes made:** {BUNDLE_MODIFICATIONS}\n\nNo upstream project or contributor endorses Syng or this bundle. Source claims and data are provided without warranties.\n"
    );
    for pin in &source_lock.artifacts {
        notice.push_str(&format!(
            "\n## {:?} — {}\n\n- Revision: `{}`\n- Material: {}\n- License: [{}]({})\n- License evidence: {}\n- Copyright notice: {}\n- Attribution: {}\n- Changes: {}\n",
            pin.source,
            pin.role,
            pin.revision,
            pin.url,
            pin.license,
            pin.license_url,
            pin.license_evidence_url,
            pin.copyright_notice,
            pin.attribution,
            pin.modifications
        ));
    }
    notice.push_str("\nFor English Wiktionary material, `wiktionary-attribution.json` links each affected lexical identity to its entry page and contributor history. Wiktionary is also offered upstream under the GFDL; this bundle uses the CC-BY-SA-4.0 option.\n");
    notice.push_str("\nWordNet-derived morphology is included under the separate Princeton WordNet license. The complete license text is provided in `LICENSE-WORDNET.txt`, which must accompany any redistribution containing that material.\n");
    notice
}

/// Returns whether any published assertion in an entity cites a given source.
fn has_source(unit: &LexicalUnit, source: crate::model::Source) -> bool {
    unit.alternative_pronunciations
        .iter()
        .any(|value| value.sources.contains(&source))
        || unit
            .measure_words
            .iter()
            .any(|value| value.sources.contains(&source))
        || unit.english.iter().any(|definition| {
            definition.gloss.sources.contains(&source)
                || definition
                    .context
                    .iter()
                    .any(|value| value.sources.contains(&source))
                || definition
                    .examples
                    .iter()
                    .any(|value| value.sources.contains(&source))
                || definition
                    .commentary
                    .iter()
                    .any(|value| value.sources.contains(&source))
                || definition
                    .qualifiers
                    .iter()
                    .any(|value| value.sources.contains(&source))
                || definition
                    .lexical_kinds
                    .iter()
                    .any(|value| value.sources.contains(&source))
                || definition
                    .parts_of_speech
                    .iter()
                    .any(|value| value.sources.contains(&source))
                || definition
                    .alternative_pronunciations
                    .iter()
                    .any(|value| value.sources.contains(&source))
                || definition
                    .measure_words
                    .iter()
                    .any(|value| value.sources.contains(&source))
        })
}

/// Generates lookup keys for primary, entity-scoped, and definition-scoped Pinyin.
fn pinyin_keys(unit: &LexicalUnit) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    add_pinyin_keys(&mut keys, &unit.pinyin);
    for alternative in &unit.alternative_pronunciations {
        add_pinyin_keys(&mut keys, &alternative.value.pronunciation);
    }
    for definition in &unit.english {
        for alternative in &definition.alternative_pronunciations {
            add_pinyin_keys(&mut keys, &alternative.value.pronunciation);
        }
    }
    keys
}

/// Adds marked, numbered, and toneless lookup forms for one pronunciation.
fn add_pinyin_keys(keys: &mut BTreeSet<String>, pinyin: &crate::model::Pinyin) {
    keys.insert(
        pinyin
            .marks
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
            .to_lowercase(),
    );
    keys.insert(pinyin.numbers.to_lowercase());
    keys.insert(
        pinyin
            .numbers
            .chars()
            .filter(|character| !character.is_ascii_digit())
            .collect::<String>()
            .to_lowercase(),
    );
}

/// Appends a runtime key to one lookup term before final sorting.
fn insert_index(index: &mut SearchIndex, key: String, runtime_key: RuntimeKey) {
    index.entry(key).or_default().push(runtime_key);
}

/// Sorts and deduplicates each lookup posting list.
fn sort_index_values(index: &mut SearchIndex) {
    for runtime_keys in index.values_mut() {
        runtime_keys.sort_unstable();
        runtime_keys.dedup();
    }
}

/// Reconstructs both headword indexes from their authoritative runtime records.
fn headword_indexes(data: &DataMap) -> (SearchIndex, SearchIndex) {
    let mut simplified = SearchIndex::new();
    let mut traditional = SearchIndex::new();
    for (runtime_key, unit) in data {
        insert_index(&mut simplified, unit.simplified.clone(), *runtime_key);
        insert_index(&mut traditional, unit.traditional.clone(), *runtime_key);
    }
    sort_index_values(&mut simplified);
    sort_index_values(&mut traditional);
    (simplified, traditional)
}

/// Requires exact headword coverage and associations for every runtime record.
fn validate_headword_indexes(
    data: &DataMap,
    simplified: &SearchIndex,
    traditional: &SearchIndex,
) -> Result<()> {
    let (expected_simplified, expected_traditional) = headword_indexes(data);
    if *simplified != expected_simplified {
        bail!("simplified headword index is inconsistent with data");
    }
    if *traditional != expected_traditional {
        bail!("traditional headword index is inconsistent with data");
    }
    Ok(())
}

/// Serializes and deterministically compresses the aligned rkyv root.
fn write_archive(directory: &Path, archive: &DictionaryArchive) -> Result<()> {
    let raw =
        rkyv::to_bytes::<rkyv::rancor::Error>(archive).context("serialize dictionary archive")?;
    let compressed = compress_zstd(&raw)?;
    let archive_name = "dictionary.rkyv.zst";
    fs::write(directory.join(archive_name), &compressed)
        .with_context(|| format!("write {archive_name}"))?;
    let ratio = 100.0 * compressed.len() as f64 / raw.len().max(1) as f64;
    eprintln!(
        "{archive_name}: {} -> {} bytes ({ratio:.1}% compressed)",
        raw.len(),
        compressed.len()
    );
    Ok(())
}

fn compress_zstd(raw: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), ZSTD_LEVEL)?;
    encoder.include_checksum(true)?;
    encoder.include_contentsize(true)?;
    encoder.set_pledged_src_size(Some(u64::try_from(raw.len())?))?;
    encoder.write_all(raw)?;
    Ok(encoder.finish()?)
}

/// Builds one sorted FST map and its flat postings storage.
fn build_index(index: &SearchIndex) -> Result<(Vec<u8>, Vec<PostingRange>, Vec<RuntimeKey>)> {
    let mut postings = Vec::with_capacity(index.len());
    let mut runtime_keys = Vec::new();
    let mut terms = Vec::with_capacity(index.len());
    for (ordinal, (term, values)) in index.iter().enumerate() {
        terms.push((term.as_str(), u64::try_from(ordinal)?));
        postings.push(append_postings(&mut runtime_keys, values)?);
    }
    let map = Map::from_iter(terms).context("build lookup FST")?;
    Ok((map.as_fst().as_bytes().to_vec(), postings, runtime_keys))
}

/// Builds the shared Chinese FST while retaining script-specific associations.
fn build_chinese_index(
    simplified: &SearchIndex,
    traditional: &SearchIndex,
) -> Result<(Vec<u8>, Vec<ChinesePostings>, Vec<RuntimeKey>)> {
    let terms = simplified
        .keys()
        .chain(traditional.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut fst_entries = Vec::with_capacity(terms.len());
    let mut postings = Vec::with_capacity(terms.len());
    let mut runtime_keys = Vec::new();
    for (ordinal, term) in terms.iter().enumerate() {
        fst_entries.push((term.as_str(), u64::try_from(ordinal)?));
        postings.push(ChinesePostings {
            simplified: append_postings(
                &mut runtime_keys,
                simplified.get(term).map_or(&[], Vec::as_slice),
            )?,
            traditional: append_postings(
                &mut runtime_keys,
                traditional.get(term).map_or(&[], Vec::as_slice),
            )?,
        });
    }
    let map = Map::from_iter(fst_entries).context("build Chinese FST")?;
    Ok((map.as_fst().as_bytes().to_vec(), postings, runtime_keys))
}

fn append_postings(flat: &mut Vec<RuntimeKey>, values: &[RuntimeKey]) -> Result<PostingRange> {
    let start = u32::try_from(flat.len()).context("posting offset exceeds u32")?;
    let len = u32::try_from(values.len()).context("posting length exceeds u32")?;
    flat.extend_from_slice(values);
    Ok(PostingRange { start, len })
}

/// Writes stable pretty JSON with one trailing newline.
fn write_json(path: PathBuf, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Validates schema, licensing, checksums, identities, attribution, and indexes.
pub fn validate_bundle(directory: &Path) -> Result<()> {
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(directory.join("manifest.json")).context("read manifest.json")?,
    )
    .context("parse manifest.json")?;
    if manifest.schema_version != SCHEMA_VERSION {
        bail!("unsupported manifest schema {}", manifest.schema_version);
    }
    if manifest.archive_contract.as_bytes() != ARCHIVE_CONTRACT {
        bail!("unsupported archive contract {}", manifest.archive_contract);
    }
    for name in LEGACY_DICTIONARY_FILES {
        if directory.join(name).exists() {
            bail!("legacy uncompressed artifact {name} must not be published");
        }
    }
    if manifest.bundle_license != BUNDLE_LICENSE
        || manifest.bundle_license_url != BUNDLE_LICENSE_URL
        || manifest.modification_notice != BUNDLE_MODIFICATIONS
    {
        bail!("manifest has an unsupported or incomplete bundle license notice");
    }
    validate_manifest_licensing(&manifest)?;
    if fs::read_to_string(directory.join("LICENSE-DATA.txt")).context("read LICENSE-DATA.txt")?
        != LICENSE_DATA_TEXT
    {
        bail!("LICENSE-DATA.txt is not the reviewed bundle license notice");
    }
    if fs::read_to_string(directory.join("LICENSE-WORDNET.txt"))
        .context("read LICENSE-WORDNET.txt")?
        != WORDNET_LICENSE_TEXT
    {
        bail!("LICENSE-WORDNET.txt is not the reviewed WordNet license notice");
    }
    let notice_text = fs::read_to_string(directory.join("NOTICE.md")).context("read NOTICE.md")?;
    for source in &manifest.source_pins {
        for required_notice in [
            source.license_url.as_str(),
            source.license_evidence_url.as_str(),
            source.copyright_notice.as_str(),
            source.attribution.as_str(),
            source.modifications.as_str(),
        ] {
            if !notice_text.contains(required_notice) {
                bail!("NOTICE.md omits licensing metadata for {:?}", source.source);
            }
        }
    }
    for source in [
        crate::model::Source::CcCedict,
        crate::model::Source::ChineseNotes,
        crate::model::Source::Wiktionary,
    ] {
        let prefix = format!("{source:?}");
        let input = manifest
            .record_counts
            .get(&format!("{prefix}.input"))
            .copied()
            .unwrap_or_default();
        let accounted = ["admitted", "suppressed", "rejected"]
            .iter()
            .map(|outcome| {
                manifest
                    .record_counts
                    .get(&format!("{prefix}.{outcome}"))
                    .copied()
                    .unwrap_or_default()
            })
            .sum::<u64>();
        if input != accounted {
            bail!("{prefix} input records are not fully accounted for");
        }
    }
    let actual_checksums = checksums(directory, CHECKSUMMED_FILES)?;
    if actual_checksums != manifest.file_checksums {
        bail!("bundle file checksums do not match manifest");
    }

    let compressed_archive =
        fs::read(directory.join("dictionary.rkyv.zst")).context("read dictionary.rkyv.zst")?;
    let decompressed_archive = zstd::stream::decode_all(compressed_archive.as_slice())
        .context("decompress dictionary.rkyv.zst")?;
    let mut archive_bytes = rkyv::util::AlignedVec::<16>::with_capacity(decompressed_archive.len());
    archive_bytes.extend_from_slice(&decompressed_archive);
    let archive = rkyv::access::<ArchivedDictionaryArchive, rkyv::rancor::Error>(&archive_bytes)
        .context("validate dictionary archive")?;
    if &archive.archive_contract != ARCHIVE_CONTRACT
        || archive.schema_version.to_native() != SCHEMA_VERSION
        || archive.identity_version != IDENTITY_VERSION
    {
        bail!("dictionary archive has incompatible format metadata");
    }
    let record_count = archive.lexical_units.len();
    if archive.identities.len() != record_count {
        bail!("identity index and data have different record counts");
    }
    let chinese_fst = Map::new(archive.chinese_fst.as_slice()).context("validate Chinese FST")?;
    let pinyin_fst = Map::new(archive.pinyin_fst.as_slice()).context("validate Pinyin FST")?;
    validate_archived_index(
        "Chinese",
        chinese_fst.len(),
        archive.chinese_postings.len(),
        archive.chinese_runtime_keys.as_slice(),
        archive.chinese_postings.iter().flat_map(|postings| {
            [&postings.simplified, &postings.traditional]
                .map(|range| (range.start.to_native(), range.len.to_native()))
        }),
        record_count,
    )?;
    validate_archived_index(
        "Pinyin",
        pinyin_fst.len(),
        archive.pinyin_postings.len(),
        archive.pinyin_runtime_keys.as_slice(),
        archive
            .pinyin_postings
            .iter()
            .map(|range| (range.start.to_native(), range.len.to_native())),
        record_count,
    )?;
    if directory.join("english.dictionary").exists() {
        bail!("legacy english.dictionary must not be published");
    }
    let compressed_english =
        fs::read(directory.join("english.search.zst")).context("read english.search.zst")?;
    let raw_english = zstd::stream::decode_all(compressed_english.as_slice())
        .context("decompress english.search.zst")?;
    english::validate_raw(&raw_english, u32::try_from(record_count)?)?;
    if manifest.english_search.compressed_bytes != compressed_english.len() as u64
        || manifest.english_search.uncompressed_bytes != raw_english.len() as u64
        || manifest.english_search.compressed_sha256
            != format!("{:x}", Sha256::digest(&compressed_english))
        || manifest.english_search.uncompressed_sha256
            != format!("{:x}", Sha256::digest(&raw_english))
    {
        bail!("English search manifest metadata does not match artifact");
    }
    let report_file: ReportFile = serde_json::from_slice(
        &fs::read(directory.join("build-report.json")).context("read build-report.json")?,
    )
    .context("parse build-report.json")?;
    if report_file.schema_version != SCHEMA_VERSION
        || report_file.file_checksums != manifest.file_checksums
        || report_file.english_search != manifest.english_search
    {
        bail!("build report is inconsistent with the manifest");
    }
    let wiktionary_attribution: WiktionaryAttribution = serde_json::from_slice(
        &fs::read(directory.join("wiktionary-attribution.json"))
            .context("read wiktionary-attribution.json")?,
    )
    .context("parse wiktionary-attribution.json")?;
    if wiktionary_attribution.schema_version != SCHEMA_VERSION {
        bail!("unsupported Wiktionary attribution schema");
    }
    if wiktionary_attribution.explanation != WIKTIONARY_ATTRIBUTION_EXPLANATION {
        bail!("unsupported Wiktionary attribution explanation");
    }
    let data = archive
        .lexical_units
        .iter()
        .enumerate()
        .map(|(runtime_key, unit)| {
            Ok((
                u32::try_from(runtime_key)?,
                rkyv::deserialize::<LexicalUnit, rkyv::rancor::Error>(unit)
                    .context("deserialize lexical unit during semantic validation")?,
            ))
        })
        .collect::<Result<DataMap>>()?;
    let (simplified, traditional) = headword_indexes(&data);
    let mut pinyin = SearchIndex::new();
    for (runtime_key, unit) in &data {
        for key in pinyin_keys(unit) {
            insert_index(&mut pinyin, key, *runtime_key);
        }
    }
    sort_index_values(&mut pinyin);
    validate_headword_indexes(&data, &simplified, &traditional)?;
    if wiktionary_attribution
        .entries
        .keys()
        .any(|lexical_id| archive.identities.get(lexical_id.digest()).is_none())
    {
        bail!("Wiktionary attribution contains an unknown lexical identity");
    }
    for (runtime_key, unit) in &data {
        let recomputed = LexicalId::new(&unit.simplified, &unit.traditional, &unit.pinyin.numbers)?;
        if recomputed != unit.id {
            bail!("lexical identity does not recompute for runtime key {runtime_key}");
        }
        if archive
            .identities
            .get(unit.id.digest())
            .map(|value| value.to_native())
            != Some(*runtime_key)
        {
            bail!("identity index is inconsistent for {}", unit.id);
        }
        if from_numbered(&unit.pinyin.numbers)? != unit.pinyin {
            bail!("noncanonical pinyin for {}", unit.id);
        }
        for alternative in unit.alternative_pronunciations.iter().chain(
            unit.english
                .iter()
                .flat_map(|definition| &definition.alternative_pronunciations),
        ) {
            if from_numbered(&alternative.value.pronunciation.numbers)?
                != alternative.value.pronunciation
            {
                bail!("noncanonical alternative pinyin for {}", unit.id);
            }
        }
        for lookup_key in pinyin_keys(unit) {
            if !pinyin
                .get(&lookup_key)
                .is_some_and(|runtime_keys| runtime_keys.binary_search(runtime_key).is_ok())
            {
                bail!(
                    "pinyin index omits lookup key {lookup_key:?} for {}",
                    unit.id
                );
            }
        }
        validate_sources(unit)?;
        let has_wiktionary = has_source(unit, crate::model::Source::Wiktionary);
        let attribution_urls = wiktionary_attribution.entries.get(&unit.id);
        if has_wiktionary != attribution_urls.is_some() {
            bail!("Wiktionary attribution is inconsistent for {}", unit.id);
        }
        if attribution_urls.is_some_and(|urls| {
            urls.is_empty()
                || urls.windows(2).any(|pair| pair[0] >= pair[1])
                || urls
                    .iter()
                    .any(|url| !url.starts_with("https://en.wiktionary.org/wiki/"))
        }) {
            bail!("invalid Wiktionary attribution URL for {}", unit.id);
        }
        for measure_word in unit.measure_words.iter().chain(
            unit.english
                .iter()
                .flat_map(|definition| &definition.measure_words),
        ) {
            if archive
                .identities
                .get(measure_word.value.digest())
                .is_none()
            {
                bail!(
                    "unresolved classifier {} in {}",
                    measure_word.value,
                    unit.id
                );
            }
        }
    }
    validate_index_contents(
        &chinese_fst,
        &archive.chinese_postings,
        archive.chinese_runtime_keys.as_slice(),
        &simplified,
        &traditional,
    )?;
    validate_single_index_contents(
        &pinyin_fst,
        &archive.pinyin_postings,
        archive.pinyin_runtime_keys.as_slice(),
        &pinyin,
    )?;
    Ok(())
}

fn validate_archived_index<I>(
    name: &str,
    fst_terms: usize,
    posting_records: usize,
    flat: &[rkyv::Archived<u32>],
    ranges: I,
    record_count: usize,
) -> Result<()>
where
    I: IntoIterator<Item = (u32, u32)>,
{
    if fst_terms != posting_records {
        bail!("{name} FST and posting metadata have different term counts");
    }
    for (start, len) in ranges {
        let start = usize::try_from(start)?;
        let end = start
            .checked_add(usize::try_from(len)?)
            .context("posting range overflow")?;
        let values = flat
            .get(start..end)
            .with_context(|| format!("{name} posting range is out of bounds"))?;
        let mut previous = None;
        for value in values {
            let value = usize::try_from(value.to_native())?;
            if value >= record_count {
                bail!("{name} posting references a missing runtime key");
            }
            if previous.is_some_and(|previous| previous >= value) {
                bail!("{name} posting values are not strictly sorted");
            }
            previous = Some(value);
        }
    }
    Ok(())
}

fn archived_postings(
    flat: &[rkyv::Archived<u32>],
    range: &ArchivedPostingRange,
) -> Result<Vec<u32>> {
    let start = usize::try_from(range.start.to_native())?;
    let end = start
        .checked_add(usize::try_from(range.len.to_native())?)
        .context("posting range overflow")?;
    Ok(flat
        .get(start..end)
        .context("posting range is out of bounds")?
        .iter()
        .map(|value| value.to_native())
        .collect())
}

fn validate_index_contents(
    fst: &Map<&[u8]>,
    postings: &[ArchivedChinesePostings],
    flat: &[rkyv::Archived<u32>],
    simplified: &SearchIndex,
    traditional: &SearchIndex,
) -> Result<()> {
    let terms = simplified
        .keys()
        .chain(traditional.keys())
        .collect::<BTreeSet<_>>();
    if fst.len() != terms.len() {
        bail!("Chinese FST term set is inconsistent with lexical records");
    }
    for term in terms {
        let ordinal = fst
            .get(term)
            .with_context(|| format!("Chinese FST omits {term:?}"))?;
        let entry = postings
            .get(usize::try_from(ordinal)?)
            .context("Chinese FST has invalid posting ordinal")?;
        if archived_postings(flat, &entry.simplified)?
            != simplified.get(term).cloned().unwrap_or_default()
            || archived_postings(flat, &entry.traditional)?
                != traditional.get(term).cloned().unwrap_or_default()
        {
            bail!("Chinese postings are inconsistent for {term:?}");
        }
    }
    Ok(())
}

fn validate_single_index_contents(
    fst: &Map<&[u8]>,
    postings: &[ArchivedPostingRange],
    flat: &[rkyv::Archived<u32>],
    expected: &SearchIndex,
) -> Result<()> {
    if fst.len() != expected.len() {
        bail!("Pinyin FST term set is inconsistent with lexical records");
    }
    for (term, values) in expected {
        let ordinal = fst
            .get(term)
            .with_context(|| format!("Pinyin FST omits {term:?}"))?;
        let range = postings
            .get(usize::try_from(ordinal)?)
            .context("Pinyin FST has invalid posting ordinal")?;
        if archived_postings(flat, range)? != *values {
            bail!("Pinyin postings are inconsistent for {term:?}");
        }
    }
    Ok(())
}

/// Validates the reviewed license policy copied into the bundle manifest.
fn validate_manifest_licensing(manifest: &Manifest) -> Result<()> {
    for source in &manifest.source_pins {
        let expected_license = match source.source {
            LockedArtifactSource::CcCedict | LockedArtifactSource::Wiktionary => "CC-BY-SA-4.0",
            LockedArtifactSource::ChineseNotes => "CC-BY-SA-3.0",
            LockedArtifactSource::PrincetonWordNet => "WordNet",
        };
        if source.license != expected_license {
            bail!("manifest contains an unreviewed source license");
        }
        if !source.license_url.starts_with("https://")
            || !source.license_evidence_url.starts_with("https://")
            || source.copyright_notice.trim().is_empty()
            || source.attribution.trim().is_empty()
            || source.modifications.trim().is_empty()
        {
            bail!("manifest contains incomplete source licensing metadata");
        }
    }
    Ok(())
}

/// Requires source attribution on every published value within an entity.
fn validate_sources(unit: &LexicalUnit) -> Result<()> {
    for definition in &unit.english {
        require_sources(&definition.gloss, "definition gloss")?;
        for value in &definition.context {
            require_sources(value, "definition context")?;
        }
        for value in &definition.examples {
            require_sources(value, "example")?;
        }
        for value in &definition.commentary {
            require_sources(value, "commentary")?;
        }
        for value in &definition.qualifiers {
            require_sources(value, "qualifier")?;
        }
        for value in &definition.lexical_kinds {
            require_sources(value, "lexical kind")?;
        }
        for value in &definition.parts_of_speech {
            require_sources(value, "part of speech")?;
        }
        for value in &definition.alternative_pronunciations {
            require_sources(value, "alternative pronunciation")?;
        }
        for value in &definition.measure_words {
            require_sources(value, "classifier")?;
        }
    }
    for value in &unit.alternative_pronunciations {
        require_sources(value, "alternative pronunciation")?;
    }
    for value in &unit.measure_words {
        require_sources(value, "classifier")?;
    }
    Ok(())
}

/// Rejects a shipped metadata value that lacks source attribution.
fn require_sources<T>(value: &Sourced<T>, kind: &str) -> Result<()> {
    if value.sources.is_empty() {
        bail!("{kind} has no source attribution");
    }
    Ok(())
}

/// Computes lowercase SHA-256 checksums for the named bundle files.
fn checksums(directory: &Path, names: &[&str]) -> Result<BTreeMap<String, String>> {
    names
        .iter()
        .map(|name| {
            let bytes = fs::read(directory.join(name))
                .with_context(|| format!("read {} for checksum", name))?;
            Ok(((*name).to_owned(), format!("{:x}", Sha256::digest(bytes))))
        })
        .collect()
}

/// Derives a process-scoped sibling path used for atomic bundle publication.
fn temporary_output_path(output_directory: &Path) -> PathBuf {
    let name = output_directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dictionary-output");
    output_directory.with_file_name(format!(".{name}.tmp-{}", std::process::id()))
}

/// Atomically swaps a validated temporary directory into its final location.
fn publish_directory(temporary_directory: &Path, output_directory: &Path) -> Result<()> {
    let backup_directory = output_directory.with_extension("dictionary-backup");
    if backup_directory.exists() {
        fs::remove_dir_all(&backup_directory).with_context(|| {
            format!("remove stale output backup {}", backup_directory.display())
        })?;
    }
    if output_directory.exists() {
        fs::rename(output_directory, &backup_directory).with_context(|| {
            format!("move existing output {} aside", output_directory.display())
        })?;
    }
    if let Err(error) = fs::rename(temporary_directory, output_directory) {
        if backup_directory.exists() {
            let _ = fs::rename(&backup_directory, output_directory);
        }
        return Err(error).context("atomically publish dictionary bundle");
    }
    if backup_directory.exists() {
        fs::remove_dir_all(&backup_directory).with_context(|| {
            format!(
                "remove replaced output backup {}",
                backup_directory.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AlternativePronunciation, Definition, HskLevels, Source};
    use tempfile::TempDir;

    fn unit() -> LexicalUnit {
        let pinyin = from_numbered("yan1 huo3").unwrap();
        LexicalUnit {
            id: LexicalId::new("烟火", "煙火", &pinyin.numbers).unwrap(),
            simplified: "烟火".to_owned(),
            traditional: "煙火".to_owned(),
            pinyin,
            alternative_pronunciations: Vec::new(),
            measure_words: Vec::new(),
            hsk: HskLevels::default(),
            english: vec![Definition::new(
                "to set off fireworks".to_owned(),
                Source::CcCedict,
            )],
        }
    }

    fn fixture_lock(directory: &Path) -> SourceLock {
        let path = directory.join("wordnet.tar.gz");
        let file = fs::File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for (name, bytes) in [
            (
                "dict/index.noun",
                b"  header\nshop n 1 0 1 1 00000001\n".as_slice(),
            ),
            (
                "dict/index.verb",
                b"  header\nrun v 1 0 1 1 00000001\nbe v 1 0 1 1 00000002\n".as_slice(),
            ),
            ("dict/noun.exc", b"men man\n".as_slice()),
            (
                "dict/verb.exc",
                b"ran run\nrunning run\nwas be\n".as_slice(),
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, name, bytes).unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();
        SourceLock {
            schema_version: SCHEMA_VERSION,
            artifacts: vec![crate::lock::SourcePin {
                source: LockedArtifactSource::PrincetonWordNet,
                role: "morphology".to_owned(),
                revision: "fixture".to_owned(),
                url: "https://example.com/wordnet".to_owned(),
                cache_file: "wordnet.tar.gz".to_owned(),
                download_sha256: "0".repeat(64),
                content_sha256: "0".repeat(64),
                preparation: crate::lock::Preparation::Plain,
                license: "WordNet".to_owned(),
                license_url: "https://wordnet.princeton.edu/license-and-commercial-use".to_owned(),
                license_evidence_url: "https://wordnet.princeton.edu/license-and-commercial-use"
                    .to_owned(),
                copyright_notice: "Copyright Princeton University.".to_owned(),
                attribution: "Princeton WordNet 3.1.".to_owned(),
                modifications: "Parsed noun and verb morphology.".to_owned(),
                parser_version: 1,
            }],
        }
    }

    #[test]
    fn complete_bundle_validates_and_repeats_byte_for_byte() {
        let temporary = TempDir::new().unwrap();
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        let lock = fixture_lock(temporary.path());
        write_bundle(
            &first,
            temporary.path(),
            &lock,
            vec![unit()],
            BuildReport::default(),
        )
        .unwrap();
        write_bundle(
            &second,
            temporary.path(),
            &lock,
            vec![unit()],
            BuildReport::default(),
        )
        .unwrap();
        validate_bundle(&first).unwrap();
        for name in LEGACY_DICTIONARY_FILES {
            assert!(!first.join(name).exists());
        }
        assert!(first.join("dictionary.rkyv.zst").is_file());
        for name in CHECKSUMMED_FILES
            .iter()
            .copied()
            .chain(["manifest.json", "build-report.json"])
        {
            assert_eq!(
                fs::read(first.join(name)).unwrap(),
                fs::read(second.join(name)).unwrap()
            );
        }
    }

    #[test]
    fn headword_indexes_require_exact_keys_associations_and_coverage() {
        let first = unit();
        let mut second = unit();
        second.simplified = "火".to_owned();
        second.traditional = "火".to_owned();
        second.pinyin = from_numbered("huo3").unwrap();
        second.id = LexicalId::new(
            &second.simplified,
            &second.traditional,
            &second.pinyin.numbers,
        )
        .unwrap();
        let data = DataMap::from([(0, first), (1, second)]);
        let (simplified, traditional) = headword_indexes(&data);

        validate_headword_indexes(&data, &simplified, &traditional).unwrap();
        assert!(validate_headword_indexes(&data, &traditional, &simplified).is_err());

        let mut wrong_association = simplified.clone();
        wrong_association.insert("烟火".to_owned(), vec![1]);
        assert!(validate_headword_indexes(&data, &wrong_association, &traditional).is_err());

        let mut omitted = simplified.clone();
        omitted.remove("烟火");
        assert!(validate_headword_indexes(&data, &omitted, &traditional).is_err());

        let mut stray_key = simplified.clone();
        stray_key.insert(String::new(), Vec::new());
        assert!(validate_headword_indexes(&data, &stray_key, &traditional).is_err());
    }

    #[test]
    fn bundle_rejects_corrupt_archive_with_updated_checksums() {
        let temporary = TempDir::new().unwrap();
        let output = temporary.path().join("bundle");
        let lock = fixture_lock(temporary.path());
        write_bundle(
            &output,
            temporary.path(),
            &lock,
            vec![unit()],
            BuildReport::default(),
        )
        .unwrap();

        let archive_path = output.join("dictionary.rkyv.zst");
        let mut archive_bytes = fs::read(&archive_path).unwrap();
        let middle = archive_bytes.len() / 2;
        archive_bytes[middle] ^= 0x80;
        fs::write(&archive_path, archive_bytes).unwrap();

        let updated_checksums = checksums(&output, CHECKSUMMED_FILES).unwrap();
        let mut manifest: Manifest =
            serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
        manifest.file_checksums = updated_checksums.clone();
        write_json(output.join("manifest.json"), &manifest).unwrap();
        let mut report: ReportFile =
            serde_json::from_slice(&fs::read(output.join("build-report.json")).unwrap()).unwrap();
        report.file_checksums = updated_checksums;
        write_json(output.join("build-report.json"), &report).unwrap();

        assert!(validate_bundle(&output).is_err());
    }

    #[test]
    fn bundle_carries_license_and_wiktionary_entry_attribution() {
        let temporary = TempDir::new().unwrap();
        let output = temporary.path().join("bundle");
        let repeated_output = temporary.path().join("repeated-bundle");
        let lock = fixture_lock(temporary.path());
        let mut wiktionary_unit = unit();
        wiktionary_unit.english[0].gloss.sources = vec![Source::Wiktionary];
        let lexical_id = wiktionary_unit.id.clone();
        write_bundle(
            &output,
            temporary.path(),
            &lock,
            vec![wiktionary_unit.clone()],
            BuildReport::default(),
        )
        .unwrap();
        write_bundle(
            &repeated_output,
            temporary.path(),
            &lock,
            vec![wiktionary_unit],
            BuildReport::default(),
        )
        .unwrap();

        let manifest: Manifest =
            serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.bundle_license, "CC-BY-SA-4.0");
        let attribution: WiktionaryAttribution =
            serde_json::from_slice(&fs::read(output.join("wiktionary-attribution.json")).unwrap())
                .unwrap();
        assert_eq!(
            attribution.entries.get(&lexical_id).unwrap(),
            &vec![
                "https://en.wiktionary.org/wiki/%E7%83%9F%E7%81%AB".to_owned(),
                "https://en.wiktionary.org/wiki/%E7%85%99%E7%81%AB".to_owned(),
            ]
        );
        assert!(output.join("LICENSE-DATA.txt").is_file());
        assert!(output.join("LICENSE-WORDNET.txt").is_file());
        assert!(output.join("NOTICE.md").is_file());
        assert_eq!(
            fs::read_to_string(output.join("LICENSE-WORDNET.txt")).unwrap(),
            WORDNET_LICENSE_TEXT
        );
        assert!(
            fs::read_to_string(output.join("NOTICE.md"))
                .unwrap()
                .contains("WordNet-derived morphology")
        );
        assert_eq!(
            fs::read(output.join("wiktionary-attribution.json")).unwrap(),
            fs::read(repeated_output.join("wiktionary-attribution.json")).unwrap()
        );
        validate_bundle(&output).unwrap();
    }

    #[test]
    fn pinyin_index_includes_all_legacy_lookup_forms() {
        let keys = pinyin_keys(&unit());
        assert!(keys.contains("yānhuǒ"));
        assert!(keys.contains("yan1huo3"));
        assert!(keys.contains("yanhuo"));
    }

    #[test]
    fn pinyin_dictionary_indexes_entity_and_definition_alternatives() {
        let temporary = TempDir::new().unwrap();
        let output = temporary.path().join("bundle");
        let lock = fixture_lock(temporary.path());
        let mut lexical_unit = unit();
        let entity_alternative = from_numbered("yan1 huo5").unwrap();
        let definition_alternative = from_numbered("yan1 huo4").unwrap();
        lexical_unit.alternative_pronunciations.push(Sourced::one(
            AlternativePronunciation {
                pronunciation: entity_alternative.clone(),
                label: "also pr.".to_owned(),
            },
            Source::CcCedict,
        ));
        lexical_unit.english[0]
            .alternative_pronunciations
            .push(Sourced::one(
                AlternativePronunciation {
                    pronunciation: definition_alternative.clone(),
                    label: "Taiwan pr.".to_owned(),
                },
                Source::CcCedict,
            ));
        write_bundle(
            &output,
            temporary.path(),
            &lock,
            vec![lexical_unit],
            BuildReport::default(),
        )
        .unwrap();

        validate_bundle(&output).unwrap();
    }
}
