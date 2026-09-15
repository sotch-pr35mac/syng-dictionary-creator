use crate::lock::SourceLock;
use crate::model::{BinaryEnvelope, LexicalId, LexicalUnit, SCHEMA_VERSION, Sourced};
use crate::pinyin::from_numbered;
use crate::sources::BuildReport;
use anyhow::{Context, Result, bail};
use fst::Set;
use regex::Regex;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const BINARY_FILES: &[&str] = &[
    "data.dictionary",
    "simplified.dictionary",
    "traditional.dictionary",
    "pinyin.dictionary",
    "english.dictionary",
    "identity.dictionary",
    "chinese.fst",
];

static DIGIT_OR_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d|\p{P}").expect("English lookup regex"));
static FIRST_PARENTHETICAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\([^)]*\)").expect("parenthetical regex"));

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
    creator_version: String,
    source_pins: Vec<ManifestSource>,
    record_counts: BTreeMap<String, u64>,
    file_checksums: BTreeMap<String, String>,
    attribution_requirement: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestSource {
    source: crate::model::Source,
    role: String,
    revision: String,
    url: String,
    content_sha256: String,
    license: String,
    attribution: String,
    parser_version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReportFile {
    schema_version: u32,
    report: BuildReport,
    file_checksums: BTreeMap<String, String>,
}

type RuntimeKey = u32;
type DataMap = BTreeMap<RuntimeKey, LexicalUnit>;
type SearchIndex = BTreeMap<String, Vec<RuntimeKey>>;
type IdentityIndex = BTreeMap<LexicalId, RuntimeKey>;

pub(crate) fn write_bundle(
    output_directory: &Path,
    source_lock: &SourceLock,
    lexical_units: Vec<LexicalUnit>,
    report: BuildReport,
) -> Result<()> {
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

    let bundle_result =
        write_temporary_bundle(&temporary_directory, source_lock, lexical_units, report)
            .and_then(|()| validate_bundle(&temporary_directory));
    if let Err(error) = bundle_result {
        let _ = fs::remove_dir_all(&temporary_directory);
        return Err(error);
    }

    publish_directory(&temporary_directory, output_directory)?;
    println!("published {}", output_directory.display());
    Ok(())
}

fn write_temporary_bundle(
    directory: &Path,
    source_lock: &SourceLock,
    lexical_units: Vec<LexicalUnit>,
    report: BuildReport,
) -> Result<()> {
    let mut data = DataMap::new();
    let mut identity = IdentityIndex::new();
    let mut simplified = SearchIndex::new();
    let mut traditional = SearchIndex::new();
    let mut pinyin = SearchIndex::new();
    let mut english = SearchIndex::new();
    let mut chinese_terms = BTreeSet::new();

    for (runtime_index, unit) in lexical_units.into_iter().enumerate() {
        let runtime_key =
            u32::try_from(runtime_index).context("more than u32::MAX lexical units")?;
        identity.insert(unit.id.clone(), runtime_key);
        insert_index(&mut simplified, unit.simplified.clone(), runtime_key);
        insert_index(&mut traditional, unit.traditional.clone(), runtime_key);
        chinese_terms.insert(unit.simplified.clone());
        chinese_terms.insert(unit.traditional.clone());
        for key in pinyin_keys(&unit) {
            insert_index(&mut pinyin, key, runtime_key);
        }
        for definition in &unit.english {
            for key in english_keys(&definition.gloss.value) {
                insert_index(&mut english, key, runtime_key);
            }
        }
        data.insert(runtime_key, unit);
    }
    sort_index_values(&mut simplified);
    sort_index_values(&mut traditional);
    sort_index_values(&mut pinyin);
    sort_index_values(&mut english);

    write_binary(directory, "data.dictionary", &data)?;
    write_binary(directory, "simplified.dictionary", &simplified)?;
    write_binary(directory, "traditional.dictionary", &traditional)?;
    write_binary(directory, "pinyin.dictionary", &pinyin)?;
    write_binary(directory, "english.dictionary", &english)?;
    write_binary(directory, "identity.dictionary", &identity)?;
    write_fst(directory, chinese_terms)?;

    let file_checksums = checksums(directory, BINARY_FILES)?;
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
            attribution: pin.attribution.clone(),
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
        source_pins,
        record_counts,
        file_checksums: file_checksums.clone(),
        attribution_requirement:
            "Redistributions must preserve every source's attribution and license notices."
                .to_owned(),
    };
    write_json(directory.join("manifest.json"), &manifest)?;
    write_json(
        directory.join("build-report.json"),
        &ReportFile {
            schema_version: SCHEMA_VERSION,
            report,
            file_checksums,
        },
    )?;
    Ok(())
}

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

fn english_keys(gloss: &str) -> BTreeSet<String> {
    let without_parenthetical = FIRST_PARENTHETICAL.replace(gloss, "");
    let normalized = DIGIT_OR_PUNCTUATION
        .replace_all(&without_parenthetical, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut keys = BTreeSet::new();
    if !normalized.is_empty() {
        keys.insert(normalized.replace(' ', "%20"));
        if let Some(verb) = normalized.strip_prefix("to ") {
            keys.insert(verb.replace(' ', "%20"));
        }
    }
    keys
}

fn insert_index(index: &mut SearchIndex, key: String, runtime_key: RuntimeKey) {
    index.entry(key).or_default().push(runtime_key);
}

fn sort_index_values(index: &mut SearchIndex) {
    for runtime_keys in index.values_mut() {
        runtime_keys.sort_unstable();
        runtime_keys.dedup();
    }
}

fn write_binary<T: Serialize>(directory: &Path, name: &str, payload: &T) -> Result<()> {
    let path = directory.join(name);
    let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    bincode::serialize_into(
        &mut writer,
        &BinaryEnvelope {
            schema_version: SCHEMA_VERSION,
            payload,
        },
    )
    .with_context(|| format!("serialize {}", path.display()))?;
    writer.flush()?;
    Ok(())
}

fn write_fst(directory: &Path, terms: BTreeSet<String>) -> Result<()> {
    let set = Set::from_iter(terms.iter().map(String::as_str)).context("build Chinese FST")?;
    let path = directory.join("chinese.fst");
    fs::write(&path, set.as_fst().as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn write_json(path: PathBuf, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn validate_bundle(directory: &Path) -> Result<()> {
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(directory.join("manifest.json")).context("read manifest.json")?,
    )
    .context("parse manifest.json")?;
    if manifest.schema_version != SCHEMA_VERSION {
        bail!("unsupported manifest schema {}", manifest.schema_version);
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
    let actual_checksums = checksums(directory, BINARY_FILES)?;
    if actual_checksums != manifest.file_checksums {
        bail!("bundle file checksums do not match manifest");
    }

    let data: DataMap = read_binary(directory, "data.dictionary")?;
    let identity: IdentityIndex = read_binary(directory, "identity.dictionary")?;
    let simplified: SearchIndex = read_binary(directory, "simplified.dictionary")?;
    let traditional: SearchIndex = read_binary(directory, "traditional.dictionary")?;
    let pinyin: SearchIndex = read_binary(directory, "pinyin.dictionary")?;
    let english: SearchIndex = read_binary(directory, "english.dictionary")?;
    let runtime_keys = data.keys().copied().collect::<BTreeSet<_>>();

    if identity.len() != data.len() {
        bail!("identity index and data have different record counts");
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
        validate_sources(unit)?;
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
        ("english", &english),
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

fn require_sources<T>(value: &Sourced<T>, kind: &str) -> Result<()> {
    if value.sources.is_empty() {
        bail!("{kind} has no source attribution");
    }
    Ok(())
}

fn read_binary<T: DeserializeOwned>(directory: &Path, name: &str) -> Result<T> {
    let path = directory.join(name);
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let envelope: BinaryEnvelope<T> = bincode::deserialize_from(file)
        .with_context(|| format!("deserialize {}", path.display()))?;
    if envelope.schema_version != SCHEMA_VERSION {
        bail!(
            "{} has unsupported schema {}",
            name,
            envelope.schema_version
        );
    }
    Ok(envelope.payload)
}

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

fn temporary_output_path(output_directory: &Path) -> PathBuf {
    let name = output_directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dictionary-output");
    output_directory.with_file_name(format!(".{name}.tmp-{}", std::process::id()))
}

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
    use crate::model::{Definition, HskLevels, Source};
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

    fn empty_lock() -> SourceLock {
        SourceLock {
            schema_version: SCHEMA_VERSION,
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn complete_bundle_validates_and_repeats_byte_for_byte() {
        let temporary = TempDir::new().unwrap();
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        write_bundle(&first, &empty_lock(), vec![unit()], BuildReport::default()).unwrap();
        write_bundle(&second, &empty_lock(), vec![unit()], BuildReport::default()).unwrap();
        validate_bundle(&first).unwrap();
        for name in BINARY_FILES
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
    fn pinyin_index_includes_all_legacy_lookup_forms() {
        let keys = pinyin_keys(&unit());
        assert!(keys.contains("yānhuǒ"));
        assert!(keys.contains("yan1huo3"));
        assert!(keys.contains("yanhuo"));
    }

    #[test]
    fn english_lookup_retains_non_full_text_behavior() {
        assert_eq!(
            english_keys("to set off fireworks (common)"),
            BTreeSet::from([
                "set%20off%20fireworks".to_owned(),
                "to%20set%20off%20fireworks".to_owned()
            ])
        );
    }
}
