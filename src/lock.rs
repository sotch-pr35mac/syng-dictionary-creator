//! Source-lock loading, licensing gates, downloading, and checksum verification.

use crate::BuildOptions;
use crate::model::{SCHEMA_VERSION, Source};
use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SourceLock {
    pub schema_version: u32,
    pub artifacts: Vec<SourcePin>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SourcePin {
    pub source: Source,
    pub role: String,
    pub revision: String,
    pub url: String,
    pub cache_file: String,
    pub download_sha256: String,
    pub content_sha256: String,
    pub preparation: Preparation,
    pub license: String,
    pub license_url: String,
    pub license_evidence_url: String,
    pub copyright_notice: String,
    pub attribution: String,
    pub modifications: String,
    pub parser_version: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Preparation {
    Plain,
    Gzip,
    WiktionaryChinese,
}

/// Loads the source lock and validates its schema and legal metadata.
fn load(options: &BuildOptions) -> Result<SourceLock> {
    let bytes = fs::read(&options.lock_file)
        .with_context(|| format!("read source lock {}", options.lock_file.display()))?;
    let source_lock: SourceLock = serde_json::from_slice(&bytes).context("parse source lock")?;
    if source_lock.schema_version != SCHEMA_VERSION {
        bail!(
            "source lock schema {} does not match creator schema {SCHEMA_VERSION}",
            source_lock.schema_version
        );
    }
    validate_license_metadata(&source_lock)?;
    Ok(source_lock)
}

/// Loads the validated source lock and strictly verifies every cache entry.
pub(crate) fn load_and_verify(options: &BuildOptions) -> Result<SourceLock> {
    let source_lock = load(options)?;
    verify_all_cached(&source_lock, &options.cache_directory)?;
    Ok(source_lock)
}

/// Requires every pinned artifact to exist and match its content checksum.
fn verify_all_cached(source_lock: &SourceLock, cache_directory: &Path) -> Result<()> {
    for pin in &source_lock.artifacts {
        let path = cache_directory.join(&pin.cache_file);
        if !path.exists() {
            bail!(
                "missing verified input {}; run `cargo run -- fetch --cache-dir {}` first",
                path.display(),
                cache_directory.display()
            );
        }
        verify_cached(pin, &path)?;
    }
    Ok(())
}

/// Blocks publication unless every source carries the exact reviewed license metadata.
fn validate_license_metadata(source_lock: &SourceLock) -> Result<()> {
    for pin in &source_lock.artifacts {
        let expected_license = match pin.source {
            Source::CcCedict | Source::Wiktionary => "CC-BY-SA-4.0",
            Source::ChineseNotes => "CC-BY-SA-3.0",
        };
        if pin.license != expected_license {
            bail!(
                "unreviewed license {:?} for {:?}/{}; update the license policy deliberately before publication",
                pin.license,
                pin.source,
                pin.role
            );
        }
        for (field, value) in [
            ("license_url", pin.license_url.as_str()),
            ("license_evidence_url", pin.license_evidence_url.as_str()),
        ] {
            if !value.starts_with("https://") {
                bail!(
                    "{field} for {:?}/{} must be an HTTPS URL",
                    pin.source,
                    pin.role
                );
            }
        }
        for (field, value) in [
            ("copyright_notice", pin.copyright_notice.as_str()),
            ("attribution", pin.attribution.as_str()),
            ("modifications", pin.modifications.as_str()),
        ] {
            if value.trim().is_empty() {
                bail!("{field} is empty for {:?}/{}", pin.source, pin.role);
            }
        }
    }
    Ok(())
}

/// Fetches missing or invalid cache artifacts and verifies all final checksums.
pub(crate) fn fetch_all(options: &BuildOptions) -> Result<()> {
    let source_lock = load(options)?;
    fs::create_dir_all(&options.cache_directory).with_context(|| {
        format!(
            "create source cache directory {}",
            options.cache_directory.display()
        )
    })?;
    let client = Client::builder()
        .user_agent(format!(
            "syng-dictionary-creator/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .context("create HTTP client")?;

    synchronize_cache(
        &source_lock,
        &options.cache_directory,
        |pin, path| match pin.preparation {
            Preparation::Plain | Preparation::Gzip => fetch_regular(&client, pin, path),
            Preparation::WiktionaryChinese => fetch_wiktionary(&client, pin, path),
        },
    )
}

/// Synchronizes a cache using an injected downloader and verifies every replacement.
fn synchronize_cache(
    source_lock: &SourceLock,
    cache_directory: &Path,
    mut download: impl FnMut(&SourcePin, &Path) -> Result<()>,
) -> Result<()> {
    for pin in &source_lock.artifacts {
        let path = cache_directory.join(&pin.cache_file);
        if path.exists() {
            match verify_cached(pin, &path) {
                Ok(()) => {
                    println!("verified {}", path.display());
                    continue;
                }
                Err(error) => {
                    eprintln!(
                        "discarding invalid cached input {}: {error:#}",
                        path.display()
                    );
                    fs::remove_file(&path).with_context(|| {
                        format!("remove invalid cached input {}", path.display())
                    })?;
                }
            }
        }
        println!("fetching {}", pin.url);
        if let Err(error) = download(pin, &path) {
            if path.exists() {
                fs::remove_file(&path)
                    .with_context(|| format!("remove failed replacement {}", path.display()))?;
            }
            return Err(error);
        }
        if let Err(error) = verify_cached(pin, &path) {
            if path.exists() {
                fs::remove_file(&path)
                    .with_context(|| format!("remove invalid replacement {}", path.display()))?;
            }
            return Err(error).context(format!("verify replacement for {}", pin.url));
        }
    }
    Ok(())
}

/// Downloads one direct source artifact through a hashing temporary file.
fn fetch_regular(client: &Client, pin: &SourcePin, path: &Path) -> Result<()> {
    let response = client
        .get(&pin.url)
        .send()
        .with_context(|| format!("download {}", pin.url))?
        .error_for_status()
        .with_context(|| format!("download {}", pin.url))?;
    let temporary_path = partial_path(path);
    let mut source = HashingReader::new(response);
    let output = File::create(&temporary_path)
        .with_context(|| format!("create {}", temporary_path.display()))?;
    let mut output = std::io::BufWriter::new(output);
    match pin.preparation {
        Preparation::Plain => {
            std::io::copy(&mut source, &mut output).context("write downloaded source")?;
        }
        Preparation::Gzip => {
            let mut decoder = flate2::read::GzDecoder::new(&mut source);
            std::io::copy(&mut decoder, &mut output).context("decompress downloaded source")?;
        }
        Preparation::WiktionaryChinese => unreachable!(),
    }
    output.flush()?;
    let download_digest = source.finish();
    if download_digest != pin.download_sha256 {
        let _ = fs::remove_file(&temporary_path);
        bail!("download checksum mismatch for {}", pin.url);
    }
    fs::rename(&temporary_path, path)
        .with_context(|| format!("publish cached input {}", path.display()))?;
    Ok(())
}

/// Streams a Wiktionary dump into the retained Chinese-only JSONL cache artifact.
fn fetch_wiktionary(client: &Client, pin: &SourcePin, path: &Path) -> Result<()> {
    let response = client
        .get(&pin.url)
        .send()
        .with_context(|| format!("download {}", pin.url))?
        .error_for_status()
        .with_context(|| format!("download {}", pin.url))?;
    let temporary_path = partial_path(path);
    let hashing_reader = HashingReader::new(response);
    let decoder = flate2::read::GzDecoder::new(hashing_reader);
    let mut reader = BufReader::new(decoder);
    let output = File::create(&temporary_path)
        .with_context(|| format!("create {}", temporary_path.display()))?;
    let mut encoder = GzEncoder::new(output, Compression::default());
    let mut retained_digest = Sha256::new();
    let mut raw_line = String::new();
    let mut source_line = 0_u64;

    while reader.read_line(&mut raw_line)? != 0 {
        source_line += 1;
        let raw: Value = serde_json::from_str(raw_line.trim_end())
            .with_context(|| format!("parse Wiktionary source line {source_line}"))?;
        let language_code = raw.get("lang_code").and_then(Value::as_str).unwrap_or("");
        let language = raw.get("lang").and_then(Value::as_str).unwrap_or("");
        if is_retained_chinese(language_code, language) {
            let wrapper = serde_json::json!({"source_line": source_line, "raw": raw});
            let mut encoded = serde_json::to_vec(&wrapper)?;
            encoded.push(b'\n');
            retained_digest.update(&encoded);
            encoder.write_all(&encoded)?;
        }
        raw_line.clear();
    }
    encoder.finish()?.sync_all()?;
    let decoder = reader.into_inner();
    let download_digest = decoder.into_inner().finish();
    if download_digest != pin.download_sha256 {
        let _ = fs::remove_file(&temporary_path);
        bail!("download checksum mismatch for {}", pin.url);
    }
    let content_digest = format!("{:x}", retained_digest.finalize());
    if content_digest != pin.content_sha256 {
        let _ = fs::remove_file(&temporary_path);
        bail!("filtered Wiktionary checksum mismatch for {}", pin.url);
    }
    fs::rename(&temporary_path, path)
        .with_context(|| format!("publish cached input {}", path.display()))?;
    Ok(())
}

/// Selects Chinese-language dump records retained for typed parsing.
fn is_retained_chinese(language_code: &str, language: &str) -> bool {
    const CODES: &[&str] = &[
        "zh", "cmn", "yue", "nan", "hak", "wuu", "gan", "hsn", "cdo", "cjy", "cpx", "mnp", "zhx",
    ];
    CODES.contains(&language_code) || matches!(language, "Chinese" | "Mandarin" | "Cantonese")
}

/// Verifies one cached artifact against its pinned byte length and SHA-256 digest.
fn verify_cached(pin: &SourcePin, path: &Path) -> Result<()> {
    let file = File::open(path).with_context(|| format!("open cached input {}", path.display()))?;
    let mut reader: Box<dyn Read> = match pin.preparation {
        Preparation::WiktionaryChinese => Box::new(flate2::read::GzDecoder::new(file)),
        Preparation::Plain | Preparation::Gzip => Box::new(file),
    };
    let mut digest = Sha256::new();
    std::io::copy(&mut reader, &mut digest).with_context(|| format!("hash {}", path.display()))?;
    let actual = format!("{:x}", digest.finalize());
    if actual != pin.content_sha256 {
        bail!(
            "cached input checksum mismatch for {}: expected {}, got {}",
            path.display(),
            pin.content_sha256,
            actual
        );
    }
    Ok(())
}

/// Derives the recoverable partial-download sibling path for a cache artifact.
fn partial_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.partial",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("download")
    ))
}

struct HashingReader<R> {
    inner: R,
    digest: Sha256,
}

impl<R> HashingReader<R> {
    /// Wraps a reader with an initially empty SHA-256 digest.
    fn new(inner: R) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
        }
    }

    /// Finalizes and returns the lowercase digest of all consumed bytes.
    fn finish(self) -> String {
        format!("{:x}", self.digest.finalize())
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.digest.update(&buffer[..count]);
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn reviewed_pin() -> SourcePin {
        SourcePin {
            source: Source::CcCedict,
            role: "dictionary".to_owned(),
            revision: "fixture".to_owned(),
            url: "https://example.com/source".to_owned(),
            cache_file: "source.txt".to_owned(),
            download_sha256: "0".repeat(64),
            content_sha256: "0".repeat(64),
            preparation: Preparation::Plain,
            license: "CC-BY-SA-4.0".to_owned(),
            license_url: "https://creativecommons.org/licenses/by-sa/4.0/".to_owned(),
            license_evidence_url: "https://example.com/license".to_owned(),
            copyright_notice: "Copyright fixture contributors.".to_owned(),
            attribution: "Fixture contributors.".to_owned(),
            modifications: "Parsed for a test.".to_owned(),
            parser_version: 1,
        }
    }

    #[test]
    fn unreviewed_source_license_blocks_publication() {
        let mut pin = reviewed_pin();
        pin.license = "CC-BY-4.0".to_owned();
        let source_lock = SourceLock {
            schema_version: SCHEMA_VERSION,
            artifacts: vec![pin],
        };
        assert!(validate_license_metadata(&source_lock).is_err());
    }

    #[test]
    fn incomplete_license_metadata_blocks_publication() {
        let mut pin = reviewed_pin();
        pin.modifications.clear();
        let source_lock = SourceLock {
            schema_version: SCHEMA_VERSION,
            artifacts: vec![pin],
        };
        assert!(validate_license_metadata(&source_lock).is_err());
    }

    #[test]
    fn cache_synchronization_keeps_valid_and_replaces_missing_and_corrupt_artifacts() {
        let directory = tempdir().unwrap();
        let mut valid = reviewed_pin();
        valid.cache_file = "valid.txt".to_owned();
        valid.content_sha256 = digest(b"valid");
        let mut missing = reviewed_pin();
        missing.cache_file = "missing.txt".to_owned();
        missing.content_sha256 = digest(b"replacement for missing");
        let mut corrupt = reviewed_pin();
        corrupt.cache_file = "corrupt.txt".to_owned();
        corrupt.content_sha256 = digest(b"replacement for corrupt");
        fs::write(directory.path().join(&valid.cache_file), b"valid").unwrap();
        fs::write(directory.path().join(&corrupt.cache_file), b"corrupt").unwrap();
        let source_lock = SourceLock {
            schema_version: SCHEMA_VERSION,
            artifacts: vec![valid, missing, corrupt],
        };
        let mut downloaded = Vec::new();

        synchronize_cache(&source_lock, directory.path(), |pin, path| {
            downloaded.push(pin.cache_file.clone());
            let content = match pin.cache_file.as_str() {
                "missing.txt" => b"replacement for missing".as_slice(),
                "corrupt.txt" => b"replacement for corrupt".as_slice(),
                _ => unreachable!("valid cache entries are not downloaded"),
            };
            fs::write(path, content)?;
            Ok(())
        })
        .unwrap();

        assert_eq!(downloaded, vec!["missing.txt", "corrupt.txt"]);
        assert_eq!(
            fs::read(directory.path().join("corrupt.txt")).unwrap(),
            b"replacement for corrupt"
        );
        verify_all_cached(&source_lock, directory.path()).unwrap();
    }

    #[test]
    fn invalid_replacement_is_removed() {
        let directory = tempdir().unwrap();
        let mut pin = reviewed_pin();
        pin.content_sha256 = digest(b"expected");
        fs::write(directory.path().join(&pin.cache_file), b"old corrupt data").unwrap();
        let source_lock = SourceLock {
            schema_version: SCHEMA_VERSION,
            artifacts: vec![pin.clone()],
        };

        let result = synchronize_cache(&source_lock, directory.path(), |_pin, path| {
            fs::write(path, b"still corrupt")?;
            Ok(())
        });

        assert!(result.is_err());
        assert!(!directory.path().join(pin.cache_file).exists());
    }

    #[test]
    fn strict_cache_verification_rejects_missing_and_invalid_inputs() {
        let directory = tempdir().unwrap();
        let mut pin = reviewed_pin();
        pin.content_sha256 = digest(b"expected");
        let source_lock = SourceLock {
            schema_version: SCHEMA_VERSION,
            artifacts: vec![pin.clone()],
        };

        assert!(verify_all_cached(&source_lock, directory.path()).is_err());
        fs::write(directory.path().join(&pin.cache_file), b"invalid").unwrap();
        assert!(verify_all_cached(&source_lock, directory.path()).is_err());
    }
}
