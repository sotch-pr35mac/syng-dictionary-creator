# English search architecture

The generator emits `english.search.zst` as the existing versioned, sectioned
English search container. Schema 4 deliberately leaves this encoding and its
normalization versions unchanged; the consumer performs planning, ranking,
deduplication, limits, and completion over these sections.

Each definition gloss is indexed independently. The English artifact contains token and text tables, direct and positional
bindings, occurrence lists, and WordNet-derived morphology. All runtime keys
refer to positions in `DictionaryArchive::lexical_units`. Generation sorts and
deduplicates each association, validates every referenced runtime key, checks
the raw container after compression round-tripping, and enforces the 22 MiB
compressed publication gate.

The English artifact is separate from `dictionary.rkyv.zst` because its packed
posting-oriented representation already supports direct zero-copy traversal.
Wrapping it in rkyv would add packaging without removing meaningful startup
work. Native FST bytes in the dictionary archive follow the same boundary: rkyv
stores the bytes and posting metadata, while `fst::Map` reads those bytes in
place.

The generator and consumer keep matching private format modules. Any wire
change must update schema/version constants and shared fixtures together. Pure
runtime query optimizations do not require an English wire-format version bump.

Each archived lexical unit separately carries its document-normalized
`commonness` score from the local `syng-word-frequency` database. The English
container deliberately does not duplicate that value: its runtime keys already
address lexical units, so the consumer can apply commonness while ranking hits
within each selected query span or concept. A zero score means unseen in the
frequency corpora and must not filter an otherwise legitimate result.
