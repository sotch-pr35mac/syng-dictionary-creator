#![warn(missing_docs)]

//! Deterministic builder for Syng's multi-source Chinese dictionary bundle.

mod bundle;
mod combine;
mod dictionary_archive;
mod english;
mod english_search_format;
mod lock;
/// Serializable dictionary types and stable lexical identities.
pub mod model;
/// Canonical conversion and validation for numbered and marked Hanyu Pinyin.
pub mod pinyin;
/// Typed adapters for the pinned upstream dictionary sources.
pub mod sources;

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Filesystem locations used by fetch and build operations.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// Directory containing checksum-verified source artifacts.
    pub cache_directory: PathBuf,
    /// Directory where the completed dictionary bundle is published.
    pub output_directory: PathBuf,
    /// Checked-in source lock file containing pins and licensing metadata.
    pub lock_file: PathBuf,
}

/// Fetches every pinned source artifact and verifies its checksum.
pub fn fetch(options: &BuildOptions) -> Result<()> {
    let started = Instant::now();
    eprintln!("Fetching pinned dictionary sources...");
    lock::fetch_all(options)?;
    eprintln!("Fetch complete in {:.1?}.", started.elapsed());
    Ok(())
}

/// Builds and atomically publishes a dictionary bundle from the verified cache.
pub fn build(options: &BuildOptions) -> Result<()> {
    let started = Instant::now();
    eprintln!("Building dictionary from verified cached sources...");

    let stage_started = Instant::now();
    let source_lock = lock::load_and_verify(options)?;
    eprintln!(
        "Verified {} pinned artifacts in {:.1?}.",
        source_lock.artifacts.len(),
        stage_started.elapsed()
    );

    let (records, mut report) = sources::parse_all(&source_lock, &options.cache_directory)?;
    let record_count = records.len();
    let stage_started = Instant::now();
    eprintln!("Combining {record_count} parsed source records...");
    let lexical_units = combine::combine(records, &mut report)?;
    eprintln!(
        "Combined records into {} lexical units and {} definitions in {:.1?}.",
        report.lexical_units,
        report.definitions,
        stage_started.elapsed()
    );

    bundle::write_bundle(
        &options.output_directory,
        &options.cache_directory,
        &source_lock,
        lexical_units,
        report,
    )?;
    eprintln!("Build complete in {:.1?}.", started.elapsed());
    Ok(())
}

/// Validates a generated bundle's schema, checksums, indexes, and attribution.
pub fn validate(output_directory: &Path) -> Result<()> {
    let started = Instant::now();
    eprintln!("Validating bundle {}...", output_directory.display());
    bundle::validate_bundle(output_directory)?;
    eprintln!("Validation complete in {:.1?}.", started.elapsed());
    Ok(())
}
