use anyhow::{Context, Result, bail};
use std::env;
use std::path::PathBuf;
use syng_dictionary_creator::{BuildOptions, build, fetch, validate};

#[derive(Debug)]
struct Cli {
    command: String,
    cache_directory: PathBuf,
    output_directory: PathBuf,
    lock_file: PathBuf,
}

fn parse_cli() -> Result<Cli> {
    let mut arguments = env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "help".to_owned());
    let mut cache_directory = PathBuf::from(".cache/dictionary-sources");
    let mut output_directory = PathBuf::from("out");
    let mut lock_file = PathBuf::from("sources.lock.json");

    while let Some(argument) = arguments.next() {
        let target = match argument.as_str() {
            "--cache-dir" => &mut cache_directory,
            "--output-dir" => &mut output_directory,
            "--lock-file" => &mut lock_file,
            _ => bail!("unknown option {argument:?}"),
        };
        *target = PathBuf::from(
            arguments
                .next()
                .with_context(|| format!("{argument} requires a path"))?,
        );
    }

    Ok(Cli {
        command,
        cache_directory,
        output_directory,
        lock_file,
    })
}

fn run() -> Result<()> {
    let cli = parse_cli()?;
    let options = BuildOptions {
        cache_directory: cli.cache_directory,
        output_directory: cli.output_directory,
        lock_file: cli.lock_file,
    };

    match cli.command.as_str() {
        "fetch" => fetch(&options),
        "build" => build(&options),
        "validate" => validate(&options.output_directory),
        "help" | "--help" | "-h" => {
            println!(
                "syng-dictionary-creator <fetch|build|validate> [--cache-dir PATH] [--output-dir PATH] [--lock-file PATH]"
            );
            Ok(())
        }
        command => bail!("unknown command {command:?}; expected fetch, build, or validate"),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
