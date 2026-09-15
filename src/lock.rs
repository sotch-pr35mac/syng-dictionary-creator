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
    pub attribution: String,
    pub parser_version: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Preparation {
    Plain,
    Gzip,
    WiktionaryChinese,
}

pub(crate) fn load_and_verify(options: &BuildOptions, require_all: bool) -> Result<SourceLock> {
    let bytes = fs::read(&options.lock_file)
        .with_context(|| format!("read source lock {}", options.lock_file.display()))?;
    let source_lock: SourceLock = serde_json::from_slice(&bytes).context("parse source lock")?;
    if source_lock.schema_version != SCHEMA_VERSION {
        bail!(
            "source lock schema {} does not match creator schema {SCHEMA_VERSION}",
            source_lock.schema_version
        );
    }
    for pin in &source_lock.artifacts {
        let path = options.cache_directory.join(&pin.cache_file);
        if !path.exists() {
            if require_all {
                bail!(
                    "missing verified input {}; run `cargo run -- fetch --cache-dir {}` first",
                    path.display(),
                    options.cache_directory.display()
                );
            }
            continue;
        }
        verify_cached(pin, &path)?;
    }
    Ok(source_lock)
}

pub(crate) fn fetch_all(options: &BuildOptions) -> Result<()> {
    let source_lock = load_and_verify(options, false)?;
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

    for pin in &source_lock.artifacts {
        let path = options.cache_directory.join(&pin.cache_file);
        if path.exists() {
            verify_cached(pin, &path)?;
            println!("verified {}", path.display());
            continue;
        }
        println!("fetching {}", pin.url);
        match pin.preparation {
            Preparation::Plain | Preparation::Gzip => fetch_regular(&client, pin, &path)?,
            Preparation::WiktionaryChinese => fetch_wiktionary(&client, pin, &path)?,
        }
        verify_cached(pin, &path)?;
    }
    Ok(())
}

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

fn is_retained_chinese(language_code: &str, language: &str) -> bool {
    const CODES: &[&str] = &[
        "zh", "cmn", "yue", "nan", "hak", "wuu", "gan", "hsn", "cdo", "cjy", "cpx", "mnp", "zhx",
    ];
    CODES.contains(&language_code) || matches!(language, "Chinese" | "Mandarin" | "Cantonese")
}

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
    fn new(inner: R) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
        }
    }

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
