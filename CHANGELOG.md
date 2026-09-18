# Changelog

## 4.0.0 - Unreleased

- Replace the five bincode dictionary artifacts and standalone Chinese FST
  with one validated `dictionary.rkyv.zst` archive.
- Store lexical identities as fixed SHA-256 digests while preserving their
  external versioned hexadecimal form.
- Package native Chinese and Pinyin FST maps with sorted flat postings.
- Retain the specialized English search encoding and deterministic publication
  and attribution checks.
- Preserve exact `(simplified, traditional, pinyin)` lexical identity while
  applying source-specific CC-CEDICT, Wiktionary, and Chinese Notes admission.
- Split CC-CEDICT semicolon text into ordered definitions and parse only
  reviewed parenthetical labels into structured metadata.
- Retain multi-pronunciation Wiktionary evidence, pair deterministic
  simplified/traditional examples, and generate a missing script counterpart
  as a display fallback without replacing source-attested text.
- Make Chinese Notes enrichment-only and report every suppressed match class.
- Embed one document-normalized commonness score in every lexical unit from the
  locally generated `syng-word-frequency` database, using zero for unseen
  identities without filtering rare vocabulary.
