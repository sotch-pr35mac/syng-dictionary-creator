# Syng Dictionary Creator

This crate builds Syng's Chinese–English dictionary bundle from pinned snapshots of CC-CEDICT, Chinese Notes, English Wiktionary, and Princeton WordNet. Version 5 adds the supporting index for English lexical search and is intentionally incompatible with earlier bundles.

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

Validate an existing bundle, including schema wrappers, checksums, identities, indexes, attribution, and classifier references:

```console
cargo run -- validate
```

All commands accept `--cache-dir PATH`, `--output-dir PATH`, and `--lock-file PATH`. `build` never downloads missing inputs; it reports the missing path and tells the operator to run `fetch` explicitly.

`sources.lock.json` records source URLs, revisions, SHA-256 checksums, SPDX licenses, license and evidence URLs, copyright notices, attribution text, modification notices, and parser versions. The creator rejects an unreviewed source-license value. A changed input or an unknown structured grammar/POS value stops publication so its mapping can be reviewed.

Audit the complete Rust dependency graph with [cargo-deny](https://github.com/EmbarkStudios/cargo-deny):

```console
cargo deny check licenses
```

`deny.toml` contains the reviewed GPL-compatible license allowlist. Run this check whenever `Cargo.lock` changes.

## English search design

[English search architecture](docs/english-search-architecture.md) specifies the finalized generator-side normalization, extraction, morphology, and binary format, plus the future consumer responsibilities. This repository generates the supporting data but does not implement production query planning or ranking.

## Canonical schema

The central serialized model is:

```rust
#[serde(transparent)]
struct LexicalId(String);

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
    chinese: String,
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
    alternative_pronunciations: Vec<Sourced<AlternativePronunciation>>,
    measure_words: Vec<Sourced<LexicalId>>,
    hsk: HskLevels,
    english: Vec<Definition>,
}
```

Vectors are always present, including when empty. Source-native IDs, parser records, raw rows, and unbounded source payloads are build-time data and are not serialized into a `LexicalUnit`.

Standalone classifiers and alternate pronunciations stay at lexical-unit scope. Inline/sense-specific values stay on their definition. An alternative pronunciation is embedded only when the alternate tuple does not already have its own lexical entity.

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

- `data.dictionary.zst`: deterministic runtime `u32` keys to `LexicalUnit`
- `simplified.dictionary.zst` and `traditional.dictionary.zst`: normalized headword indexes
- `pinyin.dictionary.zst`: primary and embedded-alternate pronunciation lookup forms
- `english.search.zst`: the complete versioned English lexical-search container, compressed as one deterministic Zstandard frame
- `identity.dictionary.zst`: persistent `LexicalId` to runtime key
- `chinese.fst`: deduplicated simplified/traditional tokenizer terms
- `manifest.json`: source pins, licenses, attribution, counts, schema, and checksums
- `build-report.json`: admitted, suppressed, rejected, and source-specific diagnostic counts
- `LICENSE-DATA.txt`: license grant for the combined dictionary bundle
- `LICENSE-WORDNET.txt`: Princeton WordNet 3.1 license notice and disclaimer
- `NOTICE.md`: source copyrights, attribution, license evidence, and modification notices
- `wiktionary-attribution.json`: lexical identities to English Wiktionary entry pages and contributor histories

Every `.dictionary.zst` file contains a schema-versioned bincode envelope compressed as a deterministic Zstandard level-19 frame with content size and checksum enabled. Ordered maps and sorted lists make runtime-key assignment and serialization deterministic. The future `chinese_dictionary` build script should decompress the dictionary archives to their corresponding `.dictionary` names and `english.search.zst` to `OUT_DIR/english.search` before compiling the consumer.

The compressed English index has a 22 MiB publication ceiling. A larger build prints its section sizes and fails before the atomic bundle swap.

With the pinned 2026-09-15 corpus, the complete version-1 English design encodes to 56,226,298 bytes raw and 22,056,532 bytes compressed (SHA-256 `08ad9f60dd3703446f090682aab557f848262c57277d1aab12f28ec0120eea9e`), within that ceiling.

The same build compresses the five schema-enveloped dictionary files from 146,687,816 bytes to 34,150,044 bytes, a 76.7% reduction. The complete validated bundle is 73,270,590 bytes, down from 182,699,416 bytes for the preceding uncompressed schema-4 bundle despite the larger English search index.

The current `chinese_dictionary` decoder cannot read schema 5. That project and Syng must migrate separately; do not copy this bundle into an unmodified runtime.

## Source and license notices

The creator software is `GPL-3.0-only`; generated data is separate. Syng's selection, arrangement, schema, metadata, and compatible adapted material use `CC-BY-SA-4.0`. CC-CEDICT and English Wiktionary material use CC-BY-SA-4.0. Chinese Notes uses CC-BY-SA-3.0, whose adapter-license clause permits a later BY-SA version with the same license elements. WordNet-derived English morphology remains under the separate Princeton WordNet License; see `LICENSE-WORDNET.txt`. English Wiktionary also offers a GFDL option upstream, but this bundle uses its CC-BY-SA-4.0 option.

Redistributors must ship `LICENSE-DATA.txt`, `LICENSE-WORDNET.txt`, `NOTICE.md`, `manifest.json`, and `wiktionary-attribution.json` with the data, retain the source and modification notices, license adaptations compatibly, and avoid implying upstream endorsement. See the checked-in [licensing and attribution notice](NOTICE.md) for the complete record.

The creator validates structure, pronunciation scope, attribution, deterministic combination, and cross-references. It does not independently certify the linguistic accuracy of source claims.
