# Syng Dictionary Creator

This crate builds Syng's Chinese–English dictionary bundle from pinned snapshots of CC-CEDICT, Chinese Notes, English Wiktionary, and Princeton WordNet, plus the locally generated Syng word-frequency database. Version 4 publishes the lexical corpus as a validated zero-copy archive while retaining the specialized English search container.

The pipeline is written in Rust, is offline by default, keeps source attribution on every published assertion, and produces byte-for-byte deterministic artifacts from the same verified inputs.

## Commands

Fetch and verify the pinned inputs into the ignored local cache:

```console
cargo run -- fetch
```

Build offline from that cache and atomically publish a validated bundle to `out/`:

```console
cargo run -- build
```

Validate an existing bundle, including archive structure, checksums, identities, indexes, attribution, and classifier references:

```console
cargo run -- validate
```

All commands accept `--cache-dir PATH`, `--commonness-db PATH`, `--output-dir PATH`, and `--lock-file PATH`. `--commonness-db` defaults to `/Volumes/Passport/Projects/word_frequency/output/syng-commonness.bin`. `build` never downloads missing inputs; it reports the missing path and tells the operator to run `fetch` explicitly.

The creator consumes that database through the local `syng-word-frequency` crate. Each lexical identity receives its stored document-normalized commonness score, or `0.0` when the frequency corpora did not observe it. The database checksum, record count, and number of nonzero published scores are recorded in `build-report.json`.

`sources.lock.json` records source URLs, revisions, SHA-256 checksums, SPDX licenses, license and evidence URLs, copyright notices, attribution text, modification notices, and parser versions. The creator rejects an unreviewed source-license value. A changed input or an unknown structured grammar/POS value stops publication so its mapping can be reviewed.

Audit the complete Rust dependency graph with [cargo-deny](https://github.com/EmbarkStudios/cargo-deny):

```console
cargo deny check licenses
```

`deny.toml` contains the reviewed GPL-compatible license allowlist. Run this check whenever `Cargo.lock` changes.

## English search design

[English search architecture](docs/english-search-architecture.md) specifies the finalized generator-side normalization, extraction, morphology, and binary format, plus the implemented consumer responsibilities. This repository generates the supporting data but does not implement production query planning or ranking.

## Canonical schema

The central serialized model is:

```rust
struct LexicalId([u8; 32]);

struct Pinyin {
    marks: String,
    numbers: String,
    tones: Vec<u8>,
}

struct Sourced<T> {
    value: T,
    sources: Vec<Source>,
}

struct AlternativePronunciation {
    pronunciation: Pinyin,
    label: String,
}

struct Example {
    simplified: Option<String>,
    traditional: Option<String>,
    english: Option<String>,
}

struct Definition {
    gloss: Sourced<String>,
    context: Vec<Sourced<String>>,
    examples: Vec<Sourced<Example>>,
    commentary: Vec<Sourced<String>>,
    qualifiers: Vec<Sourced<Qualifier>>,
    lexical_kinds: Vec<Sourced<LexicalKind>>,
    parts_of_speech: Vec<Sourced<PartOfSpeech>>,
    alternative_pronunciations: Vec<Sourced<AlternativePronunciation>>,
    measure_words: Vec<Sourced<LexicalId>>,
}

struct LexicalUnit {
    id: LexicalId,
    simplified: String,
    traditional: String,
    pinyin: Pinyin,
    commonness: f32,
    alternative_pronunciations: Vec<Sourced<AlternativePronunciation>>,
    measure_words: Vec<Sourced<LexicalId>>,
    hsk: HskLevels,
    english: Vec<Definition>,
}
```

Vectors are always present, including when empty. `commonness` is a finite, nonnegative ranking prior rather than a vocabulary filter; zero means the identity was unseen in the configured frequency corpora. Source-native IDs, parser records, raw rows, and unbounded source payloads are build-time data and are not serialized into a `LexicalUnit`.

Standalone classifiers and alternate pronunciations stay at lexical-unit scope. Inline/sense-specific values stay on their definition. Alternative-pronunciation evidence is retained even when another lexical identity uses the same pronunciation; the two assertions have different learner-facing purposes.

CC-CEDICT establishes the initial identity set. Slash and semicolon separators produce ordered definition entries. Only closed, reviewed parenthetical labels such as `(idiom)` are converted to structured metadata; unrecognized parentheticals remain literal gloss text.

Wiktionary may establish an exact single-pronunciation identity. A multi-pronunciation Wiktionary record enriches one CC-CEDICT identity only when the same written forms and CC-CEDICT primary/alternative pronunciation evidence identify one owner. Distinct single-pronunciation records remain distinct identities. Simplified and traditional examples are paired within one sense using exact English text and converter-derived script keys; ambiguous conversion classes remain separate. Every accepted example publishes both script forms: source-attested text is preserved, and a missing counterpart is generated with the pinned character converter for display fallback.

Chinese Notes is enrichment-only. It cannot establish lexical identities or publish glosses. A row must exactly match a complete identity and an existing gloss before its whitelisted part-of-speech, lexical-kind, and domain metadata can be attached. The `\N` traditional sentinel remains incomplete for matching and is not inferred.

## Stable identity

Simplified and traditional forms are normalized to NFC, trimmed only at their outer Unicode whitespace, and rejected if empty or containing controls. Numbered Pinyin is case-preserving, uses tones 1–5, normalizes `u:` and `v` to `ü`, and is concatenated without separators. Spaces, hyphens, apostrophes, commas, and middle dots are accepted as pronunciation boundaries and omitted from the canonical form.

The version-1 identity is:

```text
"1:" + lowercase_hex(
  SHA256(
    UTF8(simplified) || 0x00 ||
    UTF8(traditional) || 0x00 ||
    UTF8(canonical_concatenated_numbered_pinyin)
  )
)
```

Capitalization and tones remain significant. For example, `fu4Ming2` and `fu4ming2` are distinct, as are `yan1huo3` and `yan1huo5`.

Marked Pinyin is converted only when the reviewed syllable vocabulary yields one segmentation, constrained by the Han-character count when applicable. Combining tone marks are accepted, including on the reviewed syllabic nasals `m`, `n`, `ng`, `hm`, and `hng`; their tones are retained in both numbered and display forms. Ambiguous readings, `xx5`, malformed input, and non-Mandarin readings cannot define an identity.

## Output

The published bundle contains:

- `dictionary.rkyv.zst`: dense lexical records, fixed-digest identity lookup, and native Chinese/Pinyin FST maps with flat postings
- `english.search.zst`: the complete versioned English lexical-search container, compressed as one deterministic Zstandard frame
- `manifest.json`: source pins, licenses, attribution, counts, schema, and checksums
- `build-report.json`: admitted, suppressed, rejected, source-specific diagnostic counts, and commonness input/coverage metadata
- `LICENSE-DATA.txt`: license grant for the combined dictionary bundle
- `LICENSE-WORDNET.txt`: Princeton WordNet 3.1 license notice and disclaimer
- `NOTICE.md`: source copyrights, attribution, license evidence, and modification notices
- `wiktionary-attribution.json`: lexical identities to English Wiktionary entry pages and contributor histories

`dictionary.rkyv.zst` uses rkyv 0.8.18 with little-endian, aligned, 32-bit-pointer formatting and deterministic Zstandard level-20 compression. Lexical units are assigned dense runtime keys in stable identity order. Identity entries are supplied to rkyv's portable archived hash map in digest order at a fixed load factor; FST terms and postings are sorted and postings are deduplicated. The consumer validates and embeds the decompressed aligned bytes without constructing an owned corpus.

The compressed English index has a 22 MiB publication ceiling. A larger build prints its section sizes and fails before the atomic bundle swap.

The canonical schema-4 archive and English format implementations remain local
to this generator. `chinese_dictionary` keeps synchronized consumer-local
definitions and shared fixtures, so its crates.io package has no unpublished
path dependencies. Changes to the still-unreleased schema 4 contract require
regenerating its fixtures and updating the consumer-local definitions before
using a newly generated bundle downstream.

## Source and license notices

The creator software is `GPL-3.0-only`; generated data is separate. Syng's selection, arrangement, schema, metadata, and compatible adapted material use `CC-BY-SA-4.0`. CC-CEDICT and English Wiktionary material use CC-BY-SA-4.0. Chinese Notes uses CC-BY-SA-3.0, whose adapter-license clause permits a later BY-SA version with the same license elements. WordNet-derived English morphology remains under the separate Princeton WordNet License; see `LICENSE-WORDNET.txt`. English Wiktionary also offers a GFDL option upstream, but this bundle uses its CC-BY-SA-4.0 option.

Redistributors must ship `LICENSE-DATA.txt`, `LICENSE-WORDNET.txt`, `NOTICE.md`, `manifest.json`, and `wiktionary-attribution.json` with the data, retain the source and modification notices, license adaptations compatibly, and avoid implying upstream endorsement. See the checked-in [licensing and attribution notice](NOTICE.md) for the complete record.

The creator validates structure, pronunciation scope, attribution, deterministic combination, and cross-references. It does not independently certify the linguistic accuracy of source claims.
