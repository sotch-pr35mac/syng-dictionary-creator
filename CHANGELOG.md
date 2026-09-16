# Changelog

## 4.0.0 - Unreleased

- Replace the five bincode dictionary artifacts and standalone Chinese FST
  with one validated `dictionary.rkyv.zst` archive.
- Store lexical identities as fixed SHA-256 digests while preserving their
  external versioned hexadecimal form.
- Package native Chinese and Pinyin FST maps with sorted flat postings.
- Retain the specialized English search encoding and deterministic publication
  and attribution checks.
