mod bundle;
mod combine;
mod lock;
pub mod model;
pub mod pinyin;
pub mod sources;

use anyhow::Result;
use std::path::PathBuf;

pub use bundle::validate_bundle as validate;

#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub cache_directory: PathBuf,
    pub output_directory: PathBuf,
    pub lock_file: PathBuf,
}

pub fn fetch(options: &BuildOptions) -> Result<()> {
    lock::fetch_all(options)
}

pub fn build(options: &BuildOptions) -> Result<()> {
    let source_lock = lock::load_and_verify(options, true)?;
    let (records, mut report) = sources::parse_all(&source_lock, &options.cache_directory)?;
    let lexical_units = combine::combine(records, &mut report)?;
    bundle::write_bundle(
        &options.output_directory,
        &source_lock,
        lexical_units,
        report,
    )
}
