# Syng Dictionary Creator

This crate builds Syng's Chinese–English dictionary bundle from pinned snapshots of CC-CEDICT, Chinese Notes, and English Wiktionary. Version 4 is an intentionally incompatible replacement for the legacy CC-CEDICT-only format.

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

- `data.dictionary`: deterministic runtime `u32` keys to `LexicalUnit`
- `simplified.dictionary` and `traditional.dictionary`: normalized headword indexes
- `pinyin.dictionary`: primary and embedded-alternate pronunciation lookup forms
- `english.dictionary`: the existing non-full-text normalized gloss lookup
- `identity.dictionary`: persistent `LexicalId` to runtime key
- `chinese.fst`: deduplicated simplified/traditional tokenizer terms
- `manifest.json`: source pins, licenses, attribution, counts, schema, and checksums
- `build-report.json`: admitted, suppressed, rejected, and source-specific diagnostic counts
- `LICENSE-DATA.txt`: license grant for the combined dictionary bundle
- `NOTICE.md`: source copyrights, attribution, license evidence, and modification notices
- `wiktionary-attribution.json`: lexical identities to English Wiktionary entry pages and contributor histories

Every binary `.dictionary` file carries an explicit schema-version envelope. Ordered maps and sorted lists make runtime-key assignment and serialization deterministic.

The current `chinese_dictionary` decoder cannot read schema 4. That project and Syng must migrate separately; do not copy this bundle into an unmodified runtime.

## Source and license notices

The creator software is `GPL-3.0-only`; generated data is separate and is licensed as an adapted database under `CC-BY-SA-4.0`. CC-CEDICT and English Wiktionary material use CC-BY-SA-4.0. Chinese Notes uses CC-BY-SA-3.0, whose adapter-license clause permits a later BY-SA version with the same license elements. English Wiktionary also offers a GFDL option upstream, but this bundle uses its CC-BY-SA-4.0 option.

Redistributors must ship `LICENSE-DATA.txt`, `NOTICE.md`, `manifest.json`, and `wiktionary-attribution.json` with the data, retain the source and modification notices, license adaptations compatibly, and avoid implying upstream endorsement. See the checked-in [licensing and attribution notice](NOTICE.md) for the complete record.

The creator validates structure, pronunciation scope, attribution, deterministic combination, and cross-references. It does not independently certify the linguistic accuracy of source claims.
