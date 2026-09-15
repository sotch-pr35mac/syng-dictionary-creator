# Multi-source production decisions

The source-combination experiment was retired when schema 4 was implemented. These are the durable decisions retained from that work.

- Lexical equality is the exact normalized `(simplified, traditional, numbered Mandarin Pinyin)` tuple. Written-family membership, source grouping IDs, English text, and source priority never establish identity.
- Any source can seed an entity when it supplies one valid tuple. An incomplete foreign record may attach in the second pass only when explicit, noncontradictory headword and pronunciation evidence selects exactly one existing tuple.
- `復明` is the capitalization/scope regression case: a Wiktionary record spanning `fu4 Ming2` and `fu4 ming2` is rejected rather than enriching either. `煙火` fixes the distinct `yan1huo3` and `yan1huo5` boundary.
- Definition sharing uses only NFC plus outer trimming of the complete leaf gloss and equality of the ordered context chain. It does not use case folding, punctuation rewriting, stemming, or semantic similarity.
- Source attribution belongs to each assertion. Exact text can collect multiple sources, but POS, qualifiers, context, examples, notes, classifiers, and pronunciations retain independent attribution and scope.
- CC-CEDICT slash segments remain ordered. Semicolons stay inside their segment. `CL:` and `Taiwan pr.`/`also pr.` are extracted only through their reviewed grammars; malformed recognized annotations are hard diagnostics.
- Chinese Notes uses exactly 16 TSV fields and `\N` as the documented same-written-form traditional sentinel. English is not generically split. A bibliography-only note is not commentary. A cited, differing gloss against an existing CC-CEDICT identity is suppressed only when it has no publishable non-English enrichment.
- Wiktionary requires one explicit Mandarin Pinyin reading and enough script evidence for a new entity. Cumulative gloss ancestors remain ordered context. Explicit non-Mandarin senses are excluded. Only structured items whose type is `example` are published; quotations and unknown example types are counted and excluded.
- Closed enums are deliberately conservative. A new source POS or Chinese Notes grammar value blocks publication until the parser and schema mapping are reviewed. Unclassified prose is never guessed into a typed enum.
- Generated runtime keys are ephemeral and sorted by `LexicalId`; only `LexicalId` is persistent. Schema 4 intentionally breaks the legacy decoder.

The source revisions, checksums, license evidence, parser versions, copyright and modification notices, and attribution strings are maintained in `sources.lock.json`. The combined bundle uses CC-BY-SA-4.0 as its adapter's license, and every build emits human-readable notices plus per-identity Wiktionary entry links. The source loader rejects unreviewed license identifiers, while `deny.toml` guards the creator's GPL-compatible dependency-license allowlist. Representative regression cases live beside the Rust adapters and identity implementation as unit tests, so they cannot drift away from executable behavior.
