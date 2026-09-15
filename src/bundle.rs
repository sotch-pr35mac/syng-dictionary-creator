//! Serialization, licensing notices, publication, and validation of output bundles.

use crate::english::{self, EnglishSearchReport};
use crate::lock::{LockedArtifactSource, SourceLock};
use crate::model::{BinaryEnvelope, LexicalId, LexicalUnit, SCHEMA_VERSION, Sourced};
use crate::pinyin::from_numbered;
use crate::sources::BuildReport;
use anyhow::{Context, Result, bail};
use fst::Set;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const CHECKSUMMED_FILES: &[&str] = &[
    "data.dictionary.zst",
    "simplified.dictionary.zst",
    "traditional.dictionary.zst",
    "pinyin.dictionary.zst",
    "english.search.zst",
    "identity.dictionary.zst",
    "chinese.fst",
    "LICENSE-DATA.txt",
    "NOTICE.md",
    "wiktionary-attribution.json",
];
const DICTIONARY_FILES: &[&str] = &[
    "data.dictionary",
    "simplified.dictionary",
    "traditional.dictionary",
    "pinyin.dictionary",
    "identity.dictionary",
];
const ZSTD_LEVEL: i32 = 19;
const BUNDLE_LICENSE: &str = "CC-BY-SA-4.0";
const BUNDLE_LICENSE_URL: &str = "https://creativecommons.org/licenses/by-sa/4.0/";
const BUNDLE_MODIFICATIONS: &str = "Syng Dictionary Creator parsed, normalized, filtered, structurally annotated, deduplicated exact matches, merged source material, assigned stable identities, and built search indexes. It excluded Wiktionary quotations.";
const WIKTIONARY_ATTRIBUTION_EXPLANATION: &str = "Each key identifies a LexicalUnit containing English Wiktionary material. The linked entry pages provide the contributor history used for author attribution.";
const LICENSE_DATA_TEXT: &str = include_str!("../LICENSE-DATA");

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
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
    let mut simplified = SearchIndex::new();
    let mut traditional = SearchIndex::new();
    let mut pinyin = SearchIndex::new();
    let mut chinese_terms = BTreeSet::new();
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
        insert_index(&mut simplified, unit.simplified.clone(), runtime_key);
        insert_index(&mut traditional, unit.traditional.clone(), runtime_key);
        chinese_terms.insert(unit.simplified.clone());
        chinese_terms.insert(unit.traditional.clone());
        for key in pinyin_keys(&unit) {
            insert_index(&mut pinyin, key, runtime_key);
        }
        data.insert(runtime_key, unit);
    }
    sort_index_values(&mut simplified);
    sort_index_values(&mut traditional);
    sort_index_values(&mut pinyin);

    write_binary(directory, "data.dictionary", &data)?;
    write_binary(directory, "simplified.dictionary", &simplified)?;
    write_binary(directory, "traditional.dictionary", &traditional)?;
    write_binary(directory, "pinyin.dictionary", &pinyin)?;
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
    write_binary(directory, "identity.dictionary", &identity)?;
    write_fst(directory, chinese_terms)?;
    fs::write(directory.join("LICENSE-DATA.txt"), LICENSE_DATA_TEXT)
        .context("write LICENSE-DATA.txt")?;
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
        creator_version: env!("CARGO_PKG_VERSION").to_owned(),
        bundle_license: BUNDLE_LICENSE.to_owned(),
        bundle_license_url: BUNDLE_LICENSE_URL.to_owned(),
        modification_notice: BUNDLE_MODIFICATIONS.to_owned(),
        source_pins,
        record_counts,
        file_checksums: file_checksums.clone(),
        english_search: english_search.report.clone(),
        attribution_requirement: "Redistributions must remain under CC-BY-SA-4.0 and include LICENSE-DATA.txt, NOTICE.md, manifest.json, and wiktionary-attribution.json. Do not imply endorsement by any upstream project or contributor."
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

/// Serializes and deterministically compresses one schema-enveloped bincode payload.
fn write_binary<T: Serialize>(directory: &Path, name: &str, payload: &T) -> Result<()> {
    if !name.ends_with(".dictionary") {
        bail!("binary artifact name must end in .dictionary");
    }
    let raw = bincode::serialize(&BinaryEnvelope {
        schema_version: SCHEMA_VERSION,
        payload,
    })
    .with_context(|| format!("serialize {name}"))?;
    let compressed = compress_zstd(&raw)?;
    let archive_name = format!("{name}.zst");
    fs::write(directory.join(&archive_name), &compressed)
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

/// Writes the sorted Chinese-term finite-state set.
fn write_fst(directory: &Path, terms: BTreeSet<String>) -> Result<()> {
    let set = Set::from_iter(terms.iter().map(String::as_str)).context("build Chinese FST")?;
    let path = directory.join("chinese.fst");
    fs::write(&path, set.as_fst().as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
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
    for name in DICTIONARY_FILES {
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

    let data: DataMap = read_binary(directory, "data.dictionary")?;
    let identity: IdentityIndex = read_binary(directory, "identity.dictionary")?;
    let simplified: SearchIndex = read_binary(directory, "simplified.dictionary")?;
    let traditional: SearchIndex = read_binary(directory, "traditional.dictionary")?;
    let pinyin: SearchIndex = read_binary(directory, "pinyin.dictionary")?;
    if directory.join("english.dictionary").exists() {
        bail!("legacy english.dictionary must not be published");
    }
    let compressed_english =
        fs::read(directory.join("english.search.zst")).context("read english.search.zst")?;
    let raw_english = zstd::stream::decode_all(compressed_english.as_slice())
        .context("decompress english.search.zst")?;
    english::validate_raw(&raw_english, u32::try_from(data.len())?)?;
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
    let runtime_keys = data.keys().copied().collect::<BTreeSet<_>>();

    if identity.len() != data.len() {
        bail!("identity index and data have different record counts");
    }
    if wiktionary_attribution
        .entries
        .keys()
        .any(|lexical_id| !identity.contains_key(lexical_id))
    {
        bail!("Wiktionary attribution contains an unknown lexical identity");
    }
    for (runtime_key, unit) in &data {
        let recomputed = LexicalId::new(&unit.simplified, &unit.traditional, &unit.pinyin.numbers)?;
        if recomputed != unit.id {
            bail!("lexical identity does not recompute for runtime key {runtime_key}");
        }
        if identity.get(&unit.id) != Some(runtime_key) {
            bail!("identity index is inconsistent for {}", unit.id);
        }
        if from_numbered(&unit.pinyin.numbers)? != unit.pinyin {
            bail!("noncanonical pinyin for {}", unit.id);
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
            if !identity.contains_key(&measure_word.value) {
                bail!(
                    "unresolved classifier {} in {}",
                    measure_word.value,
                    unit.id
                );
            }
        }
    }
    for (name, index) in [
        ("simplified", &simplified),
        ("traditional", &traditional),
        ("pinyin", &pinyin),
    ] {
        for values in index.values() {
            if values.windows(2).any(|pair| pair[0] >= pair[1]) {
                bail!("{name} index values are not strictly sorted");
            }
            if values
                .iter()
                .any(|runtime_key| !runtime_keys.contains(runtime_key))
            {
                bail!("{name} index contains a missing runtime key");
            }
        }
    }
    Set::new(fs::read(directory.join("chinese.fst"))?).context("validate Chinese FST")?;
    Ok(())
}

/// Validates the reviewed license policy copied into the bundle manifest.
fn validate_manifest_licensing(manifest: &Manifest) -> Result<()> {
    for source in &manifest.source_pins {
        let expected_license = match source.source {
            LockedArtifactSource::CcCedict | LockedArtifactSource::Wiktionary => "CC-BY-SA-4.0",
            LockedArtifactSource::ChineseNotes => "CC-BY-SA-3.0",
            LockedArtifactSource::PrincetonWordNet => "WordNet-3.0",
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

/// Reads one binary payload and enforces its schema envelope.
fn read_binary<T: DeserializeOwned>(directory: &Path, name: &str) -> Result<T> {
    let archive_name = format!("{name}.zst");
    let path = directory.join(&archive_name);
    let compressed = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let raw = zstd::stream::decode_all(compressed.as_slice())
        .with_context(|| format!("decompress {}", path.display()))?;
    let envelope: BinaryEnvelope<T> =
        bincode::deserialize(&raw).with_context(|| format!("deserialize {}", path.display()))?;
    if envelope.schema_version != SCHEMA_VERSION {
        bail!(
            "{} has unsupported schema {}",
            name,
            envelope.schema_version
        );
    }
    Ok(envelope.payload)
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
                license: "WordNet-3.0".to_owned(),
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
        for name in DICTIONARY_FILES {
            assert!(!first.join(name).exists());
            assert!(first.join(format!("{name}.zst")).is_file());
        }
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
        assert!(output.join("NOTICE.md").is_file());
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
        let lexical_id = lexical_unit.id.clone();

        write_bundle(
            &output,
            temporary.path(),
            &lock,
            vec![lexical_unit],
            BuildReport::default(),
        )
        .unwrap();

        let identity: IdentityIndex = read_binary(&output, "identity.dictionary").unwrap();
        let pinyin: SearchIndex = read_binary(&output, "pinyin.dictionary").unwrap();
        let runtime_key = identity[&lexical_id];
        for pronunciation in [entity_alternative, definition_alternative] {
            for lookup_key in [
                pronunciation
                    .marks
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect::<String>()
                    .to_lowercase(),
                pronunciation.numbers.to_lowercase(),
                pronunciation
                    .numbers
                    .chars()
                    .filter(|character| !character.is_ascii_digit())
                    .collect::<String>()
                    .to_lowercase(),
            ] {
                assert_eq!(pinyin.get(&lookup_key), Some(&vec![runtime_key]));
            }
        }
    }
}
