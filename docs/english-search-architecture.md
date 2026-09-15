# English lexical search architecture

Status: generator format finalized and implemented, 2026-09-15; consumer work remains future.

This document specifies the English search mechanism to implement in
`chinese_dictionary` and the supporting data to generate in
`syng-dictionary-creator`. The product name “full-text search” refers to this
dictionary-specific lexical search. The implementation returns `LexicalUnit`s.

The conversation's decisions supersede conflicting recommendations in the
original English search handoff, especially its treatment of common words and
its preference rules for longer spans. Implementation choices below are concrete
generator requirements; runtime performance budgets are initial tuning values,
not measured guarantees. This work changes the generator bundle but does not
implement the runtime.

## 1. Search contract

The search first identifies concepts in the query, then retrieves lexical units
for those concepts. A concept is a selected, contiguous range of query tokens.

- Prefer longer recognized spans. If `my name is` matches, suppress independent
  searches for `my`, `name`, and `is` inside that span.
- Recover multiple concepts: `hello my name is` can select `[hello] [my name is]`.
- If the longer phrase is unavailable, allow `[my] [name is]`, or further
  decomposition according to what the dictionary recognizes.
- Skip unrecognized tokens without abandoning the rest of the query:
  `pri design` can select `[design]`. Never join words across a skipped token
  into one phrase.
- Retain every common word. Searches for `is`, `to`, `a`, `the`, and `too` work.
- Treat grammatical wrappers as optional for discovery: `run` / `to run`,
  `store` / `a store` / `the store`, and `happy` / `be happy` / `to be happy` /
  `is happy` discover one another.
- Match inflections in both directions: `run`, `runs`, `running`, and `ran`
  discover one another, including when only an inflected form appears in the
  corpus. Preserve literal matches as stronger evidence.
- Allow completion of the final token: `des` can find `design`, and `print des`
  can find `print design`. Earlier tokens never receive prefix completion.
- Preserve digits: `3d` must discover `3-D`.
- Deduplicate results by lexical unit. Return all its original definitions;
  search does not reorder them.

This remains lexical discovery: `store` can find a unit whose definition contains
`store`, including a longer definition. A definition containing only `shop`
needs another lexical connection in the data to be found by `store`. Synonyms
and paraphrases are not implied by inflection matching.

## 2. Overall mechanism

```text
GENERATOR
Definition.gloss
  -> search text extraction + spelling/grammar aliases
  -> deduplicated searchable texts + lexical-unit bindings
  -> phrase FST + token occurrence index + ranked token hits
English morphology vocabulary
  -> surface-to-lemma lookup + lemma-to-corpus-token lookup

RUNTIME
raw query + completion mode
  -> tokens with original byte ranges
  -> literal tokens + morphology alternatives + grammatical views
  -> recognized spans (whole phrases, contained phrases, single tokens)
  -> choose non-overlapping concepts
  -> retrieve and rank lexical units within each concept
  -> deduplicate and fairly merge concept results
  -> resolve selected IDs to LexicalUnit references
```

An FST is a compact lookup structure for strings with shared prefixes. Use it
for exact phrase lookup and viable-prefix checks. Positional token postings
support discovering an ordered phrase inside a longer definition without
generating every possible substring at build time.

No database, network request, or model inference is involved at search time.
Load immutable index sections once and reuse them.

## 3. Shared normalization

Put normalization and the binary reader/writer contract in a small shared Rust
crate, provisionally `syng-english-search-format`. Initially it can live under
`crates/english-search-format` in this repository and be consumed by a pinned
revision from `chinese_dictionary`. It owns normalization, grammatical views,
format types, and decoding. The generator owns English linguistic data and
build logic; `chinese_dictionary` owns query planning and ranking.

Normalization version 1:

1. Apply Unicode NFKC and Unicode lowercase, then NFC. Do not strip accents.
2. Retain Unicode letters, combining marks attached to letters, and digits.
3. Convert whitespace and ordinary punctuation to token boundaries. Keep original
   byte ranges separately; normalized offsets cannot be used to slice raw input.
4. Normalize internal straight/curly apostrophes to `'` and retain them in the
   literal token. Add an apostrophe-free spelling alias for discovery, so
   `don't` can match `dont`, but literal `cant` outranks an alias of `can't`.
   Do not expand contractions in version 1.
5. Normalize internal hyphens to `-`. Preserve that literal spelling and create
   joined and space-separated search aliases: `3-d`, `3d`, `3 d`; `well-being`,
   `wellbeing`, `well being`. Apply the same rules to query views.
6. Serialize phrase keys as UTF-8 tokens separated by one ASCII space. Do not
   encode spaces as `%20`.

Generate uniform spelling views (literal, all internal hyphens joined, all
internal hyphens separated, apostrophe-free counterparts), not every independent
combination of punctuation rewrites. Deduplicate identical views. Partial mixed
spellings across several hyphenated compounds are not guaranteed in version 1.

Each query view carries a mapping back to the original token boundaries. A
recognized span may only start/end at those boundaries, so matching one half of
an expanded hyphenated token cannot accidentally consume the entire raw token.

Keep commas, semicolons, sentence punctuation, and newlines as hard phrase
boundaries in the query. They still permit separate concepts on either side.
Spaces, internal hyphens, and apostrophes follow the rules above. A phrase must
not cross a hard boundary.

## 4. What constitutes searchable text

Process each leaf `Definition.gloss` independently at build time.

For each gloss:

1. Index its full normalized text.
2. Extract nonempty alternatives separated by semicolons outside parentheses or
   brackets. Keep the full text too, with alternative-derived bindings marked.
3. For each resulting text, produce two parenthetical views: contents retained
   inline, and all balanced parenthetical groups omitted. Do not create all
   subsets of parentheticals. Unbalanced parentheses only undergo punctuation
   normalization. Parenthetical omission is weaker evidence than the full text.
4. Apply the spelling views from section 3 and grammatical views from section 5.
5. Tokenize the resulting texts and deduplicate equal token sequences globally.

Do not split commas, slashes, or `or` into alternative definitions by guessing.
Do not generate every contiguous n-gram. Positions supply substring matching.
Index leaf glosses initially; examples, commentary, and definition ancestors do
not contribute hits in this version. Ancestor retrieval can be added as an
explicit weaker field later.

Canonical definitions retain their existing text, boundaries, and metadata.
Search alternatives are derived index data only.

### Internal text references, without GlossId

Use a bundle-local `TextKey: u32` for each distinct normalized searchable text.
It is an array offset, like a token ID or posting-list offset. It does not name a
source definition, persist across builds, or appear on a `LexicalUnit`.

For example, every definition that yields `to run` can share the same search
text record. That record binds to all relevant lexical-unit runtime keys.
One definition can also contribute several different search texts.

This lets the runtime prove that `medicine` and `shop` occur together, in order,
in one searchable text. It must not invent a phrase from `medicine` in one
definition and `shop` in another definition of the same unit. After matching,
only lexical-unit IDs and match evidence matter; individual gloss identification
and gloss ordering are unnecessary.

## 5. Optional grammar

Retain literal text and add one reduced grammatical view. Apply the same
reduction to a candidate query span and a searchable text:

1. Remove article tokens `a`, `an`, and `the` within a multi-token span.
2. Remove one leading `to` when another token follows.
3. Remove one leading form of `be`: `be`, `am`, `is`, `are`, `was`, `were`,
   `been`, or `being`, when another token follows. This runs after step 2.
4. If the result would be empty, do not emit a reduced view.

These are lexical discovery rules, not a claim that every such deletion
preserves sentence meaning. For example, `to school` can discover `school` at a
weaker level. Literal matching and the ranking order below keep the distinction.
This explicit surface rule avoids depending on complete English POS annotation
of the Chinese definitions.

Preserve other prepositions and particles, including `up`, `after`, `of`, and
internal/trailing `to`. Thus `give up` is not equivalent to `give`, and
`look after` is not equivalent to `look`. Internal forms of `be` are retained.
The word `too` is never removed.

Articles may be omitted at individual positions because both sides compare
their fully reduced views. For example, `a picture of the store`,
`picture of the store`, and `picture of store` share the same reduced key.

Queries always keep their literal path. A standalone `is` therefore retrieves
literal `is`, with further morphology/containment discovery below it.

### Span coverage

Reduction does not remove coverage from the original query. Matching `to run`
through reduced `run` covers both original tokens. Matching `is happy` through
`happy` covers both. Neither also emits independent component searches.

At runtime implement the reduction as a small token transducer: article
transitions and permitted leading-wrapper transitions can consume a token
without producing an index token. Require at least one produced token. Track
original start/end positions and whether any rewriting occurred.

An article-only prefix must not stop traversal merely because its reduced
output is currently empty: `[the]` can continue to `[the store]`. Do not emit a
match for the empty intermediate state.

## 6. Inflections

Use lemma relationships, preserving the original token. A lemma is the base
dictionary form, such as `run` for `running`.

Generate two compact mappings:

```text
normalized surface -> [(lemma ID, English part of speech), ...]
(lemma ID, English part of speech) -> [corpus TokenId, ...]
```

At runtime a token's alternatives are its exact corpus token, if present, plus
corpus tokens sharing one of its lemma/POS pairs. Exact equality remains literal
evidence; every other alternative is morphological evidence.

This handles both directions without storing every inflected variant of every
multiword phrase. Match token alternatives against phrase traversal or token
positions incrementally. Never materialize the Cartesian product of all query
token alternatives.

Preserve multiple analyses. `saw` may be a noun or a form of `see`. Keep
lemma/POS pairs separate and do not take transitive closure through ambiguous
words: an ambiguous surface spelling must not merge unrelated lemma families.
This remains discovery rather than full English word-sense disambiguation.

### Morphology source and build procedure

Use a pinned WordNet vocabulary and exception lists as the initial lemma/POS
source, together with explicit English inflection generation rules and a small
override file. WordNet's Morphy documentation describes exception lookup,
POS-specific suffix rules, and vocabulary checks; it also documents limitations
and possible nonword analyses. It is a useful source/model, not a complete
bidirectional inflection generator.
Source: [Princeton WordNet Morphy documentation](https://wordnet.princeton.edu/documentation/morphy7wn).

The generator should:

1. Analyze corpus tokens against the pinned lemma vocabulary and exceptions.
2. Keep every relevant lemma/POS pair, including lemmas whose base spelling does
   not itself occur in the corpus.
3. Generate supported forms for those lemmas, including forms absent from the
   Chinese dictionary, and include recorded irregular forms.
4. Resolve spelling rules such as doubled consonants and silent `e` explicitly;
   blindly reversing suffix-stripping rules would admit misspellings.
5. Emit the surface map and the reverse map to actual corpus tokens.

Version 1 includes noun plurals and verb base, third-person singular, past,
past-participle, and present-participle forms. Include irregular `be` forms.
Proposed initial boundary: adjective comparison and derivation (`runner`,
`runnable`) are outside automatic equivalence. Literal/token matches for those
words still work. The agreed `happy` behavior comes from optional grammar.

Unknown words retain literal and prefix lookup. The runtime does not invent
new suffix transformations. Pin the morphology data and rule version in the
search metadata; ship only the relevant compact mappings, not all of WordNet.

## 7. Generated index layout

Produce one uncompressed logical `english.search` container with a small fixed
header, a section directory, and independently readable sections. Compress the
complete container as deterministic, single-threaded Zstandard level 19 with
content size and checksum enabled, and publish only `english.search.zst`. This
replaces the legacy `english.dictionary`.

Header fields: magic bytes, search-format version, normalization version,
grammar version, morphology version, lexical-unit count, and section directory
offset/count. Record the artifact under the existing bundle manifest/checksum
mechanism and increment the bundle schema when switching formats.

All integers in fixed records are explicitly little-endian; use `u64` byte
offsets and `u32` IDs/counts. Variable records use offset arrays. Do not serialize
native Rust struct memory, `usize`, or pointers.

### Sections

**Token vocabulary**

- `token.fst`: normalized token string -> `TokenId`.
- `token.strings`: offsets plus UTF-8 token bytes, for reverse lookup and prefix
  scoring.
- `token.meta`: occurrence count, lexical-unit document frequency, and offsets
  into the occurrence and ranked-hit sections.

**Search texts and exact phrases**

- `phrase.fst`: space-joined normalized text -> `TextKey`.
- `text.meta`: token offset/count and binding-list offset/count.
- Include the best available binding evidence in `text.meta`, so span recognition
  can establish match strength without walking a potentially large binding list.
- `text.tokens`: concatenated `TokenId` sequences.
- `text.bindings`: `(RuntimeKey, derivation_flags)` records.

Derivation flags describe spelling aliases, semicolon extraction, parenthetical
omission, and grammar reduction. Store flags per binding: the same searchable
text can be literal for one unit and derived for another. Retain nondominated
evidence variants if one combined record would lose ranking information. Do not
OR unrelated derivations into a falsely strong or falsely weak match.

**Token occurrences**

- `token.occurrences`: token -> sorted `(TextKey, token_position)` postings.
- Include repeated occurrences; `very very` must require two adjacent positions.
- Use blocks of 128 records, delta-encoded text IDs and positions, with a block
  directory supporting `seek(TextKey)` and decoding only relevant blocks.

These postings prove adjacency inside a text. They never point into display
gloss arrays. Original gloss lengths/IDs are unnecessary.

**Ranked direct token hits**

- `token.hits`: token -> lexical-unit postings, ordered in evidence/length
  buckets, then by runtime key.
- Each entry stores the best applicable field class and containing-text length.
- Within a token, deduplicate by runtime key and retain its best evidence.

This deliberately duplicates a small part of the text bindings. It allows a
common query such as `is` to obtain its best first page without scanning every
text containing `is`. The direct path must use the same ranking tuple as the
positional path so it is an optimization with equivalent ordering.

**Morphology**

- `morphology.fst`: normalized surface spelling -> analysis-list offset.
- `morphology.analyses`: lemma/POS pairs.
- `morphology.tokens`: lemma/POS -> sorted corpus token IDs.

**Metadata**

- Counts, maximum searchable-text token count, morphology source/rule revision,
  and the maximum UTF-8 byte length of a whole phrase after normalization.
- Derivation flag definitions and section codec versions.

Phrase FST entries and the corresponding binding lists also supply exact
single-token results. Keep exact lists separately addressable so broad token
postings cannot displace exact hits before ranking.

### Why these structures

Phrase lookup finds whole concepts quickly. Token positions add phrases inside
longer definitions with storage proportional to text length, rather than all
possible substrings. Ranked token hits make common-word lookup inexpensive.
Morphology mappings keep English rule processing in the generator.

`TextKey`, `TokenId`, and `RuntimeKey` are all bundle-local. Only `LexicalId`
retains the existing persistent identity role.

## 8. Runtime span recognition

Represent each candidate as:

```rust
struct SpanCandidate {
    start_token: u32,              // inclusive, original query tokens
    end_token: u32,                // exclusive
    best_evidence: Evidence,
    retrieval: Vec<RetrievalPlan>, // index references, not loaded LexicalUnits
}
```

Coalesce equal start/end ranges, retain their retrieval plans, and deduplicate
plans. Do not load thousands of lexical units to decide whether a span exists.

### Pass A: whole searchable phrases

From each token boundary, walk the phrase FST with the literal query view.
Record terminal matches and stop when the prefix is impossible. Run grammatical
and spelling views as additional paths. Match completed-token morphology
alternatives incrementally; deduplicate equivalent traversal states, including
their accumulated FST output, query position, and transformation state.

Literal traversal always runs independently of the budget for expanded paths.
Phrase length is bounded by the corpus metadata and query length, never an
arbitrary four-word ceiling.

### Pass B: phrases contained in longer texts

Recognize contiguous, ordered matches within search texts. A query
`print design` can match a text such as `a method of print design`.

For each viable multi-token query span/view:

1. Look up the occurrence counts of its completed-token alternatives.
2. Use the token position with the smallest combined occurrence count as an
   anchor. If any required token has no alternatives, reject that span.
3. For an anchor occurrence `(text, p)` at query offset `k`, test the candidate
   text start `p - k`. Verify the full token sequence in `text.tokens`, using
   each query token's alternative set.
4. Reject out-of-bounds, reordered, nonadjacent, or cross-text matches.
5. Deduplicate verified text/start pairs and emit the span with weaker evidence
   than a whole-text match. Resolve bindings only during retrieval.

Generate spans within each hard-boundary segment up to the corpus-derived
maximum text length. Enumerating query spans costs O(Q²) in the worst case;
it does not enumerate all dictionary phrases. Use token-existence pruning,
anchor-count ordering, cached alternative sets, and a bounded discovery pass.
The exact FST pass does not depend on this enumeration.

Grammar views are matched against both literal and reduced stored texts, and
their coverage maps preserve the original query span. An unmatched word cannot
be removed by this process unless it is an explicitly optional wrapper/article.

### Pass C: single-token fallback

Every token with exact, morphological, or eligible prefix hits can create a
one-token span. Include common words. Use direct ranked token postings rather
than occurrence scans. These candidates are discarded if a selected longer
span covers them.

## 9. Prefix completion

Expose `completion: Disabled | FinalToken`. `FinalToken` permits completion only
when the raw input ends in a token, with no trailing whitespace or hard boundary.
The compatibility search wrapper uses `FinalToken`; submitted/exact-only callers
can choose `Disabled`. This preserves the distinction between `des` and `des `.

Initial minimum: three normalized characters in the unfinished token. Exact
one- and two-character words remain searchable. Prefix expansion extends the
literal unfinished spelling; do not lemmatize a fragment such as `runn`.
Completed preceding tokens still get full morphology and grammar behavior.

Run complete-token lookup first even when completion is enabled. A real word
such as `run` retains its exact results and may have lower-ranked completions.

Use contextual traversal for whole phrases: after `print `, traverse the phrase
index under `des`, rather than expanding a globally truncated `des*` list first.
Completion extends exactly one token: `print design method` is not a whole-text
match for `print des`. It can qualify through contained-phrase matching instead.
For contained phrases, anchor on a completed token and check whether the final
text token starts with the typed prefix. For standalone prefixes, enumerate the
token vocabulary with a bounded scan.

Recommended initial discovery budgets per query:

- At most 256 standalone vocabulary completions inspected; prefer fewer added
  characters, then stronger lexical evidence, then stable lexical order among
  inspected completions.
- At most 4,096 expanded phrase traversal states, including morphology paths.
- At most 8,192 occurrence records decoded during contained-phrase discovery.
- At most 4,096 query-span/view plans considered for contained-phrase discovery;
  inspect longer spans first, then rarer anchors and leftmost starts. This also
  bounds planning when most words have no useful occurrence matches.
- At most 2,048 distinct lexical-unit candidates scored per selected concept.

These are deterministic work limits, not latency claims. Process whole-phrase
plans first, then rarer anchors and stronger evidence. Never consume these
budgets on behalf of the literal FST pass. Budget exhaustion sets
`discovery_truncated`; it may reduce broader recall or change segmentation when
only a discovery match proves a longer span. Do not report “no matches” as an
exhaustive conclusion in that case.

Do not persist only a capped list of prefix matches in the bundle. Keeping the
full vocabulary lets runtime budgets change without rebuilding dictionary data.
If prefix costs fail the targets, disable completion while retaining exact,
grammatical, and inflectional matching.

Bound request size separately from phrase length: initially reject input longer
than `max(4096, max_indexed_phrase_utf8_bytes)` UTF-8 bytes with `InputTooLong`,
rather than silently trimming it. This is a request resource limit, not the old
four-token phrase ceiling. Include byte/transition work in traversal budgets;
counting only emitted matches would not bound expensive failed searches.

## 10. Choosing concepts

Construct a directed acyclic graph over original query boundaries `0..Q`:
each recognized span is an edge; a skip edge advances one token with no result.
Do not create recognized edges across hard phrase boundaries.

Choose a path using dynamic programming. Compare paths lexicographically by:

1. Most covered original tokens.
2. Largest sum of squared recognized-span lengths.
3. Best evidence: compare counts of covered tokens in evidence classes, strongest
   class first, preferring more coverage at the first class that differs.
4. Earliest longer span, then a stable index-key tie-break.

Example: a three-token phrase contributes 9 to criterion 2; three one-token
concepts contribute 3. Thus a recognized `my name is` suppresses its components,
including when recognition uses optional grammar or morphology.

The first criterion means better total coverage can beat one overlapping longer
phrase. For example, `[a b] [c d]` beats `[a b c]` plus an unmatched `d`.
This is the proposed resolution for overlapping phrases; among equally covered
alternatives, longer spans win. Prefix and contained-phrase matches count as
recognized spans, so a longer discovery span may beat shorter literal spans.
This is intentional under the requested longer-span preference.

After the path is chosen, retrieve only its selected spans. Do not append
results for any contained component span. A lexical unit may still appear
because it independently matches the selected full span; that is not a
component fallback.

## 11. Ranking and returning results

Ranking is within each selected concept. Use a deterministic tuple, not a sum
of opaque boosts. In order:

1. Whole searchable-text match before proper substring/token containment.
2. Complete match before final-token completion.
3. Literal before spelling/grammar alias, before morphology, before a combination
   of alias/grammar and morphology.
4. Full gloss or semicolon alternative before parenthetical-omission-derived text.
5. Fewer unmatched surrounding text tokens for containment.
6. Fewer added characters for prefix completion.
7. Fewer transformations, then stable `RuntimeKey`.

This ordering deliberately favors a whole lexical expression over a word
buried in descriptive prose. For `run`, an entire `to run` or `running`
definition precedes an incidental literal `run` inside a long explanation.
Within whole matches, literal `run` comes first. Whole prefix completions can
precede contained matches; they never precede whole complete matches.

Take the best evidence for a lexical unit. Ten weak matches across its many
definitions must not outrank another unit's one strong match merely by count.
No English corpus-frequency prior is assumed to represent Chinese learner
usefulness; do not invent a frequency score from source repetition. The final
stable tie-break is reproducibility, not a claim about lexical importance.

Use lazy merging of ranked posting streams. Initial response defaults:
20 hits per concept and 100 unique lexical units globally. Fill the flat result
list round-robin across concepts in query order, preserving each concept's
internal order. Skip duplicates and refill from that concept. This prevents a
broad `hello` result set from hiding every `my name is` result.

Keep per-concept hit lists as well as the unique flat list. A unit that matches
two concepts appears once in the flat list and remains associated with both
concepts. With more concepts than the global limit, return all concept metadata
but mark the limited result set; some concepts necessarily lack a displayed hit.

Use per-concept continuation cursors for remaining ranked hits. Bind cursors to
the bundle fingerprint, query, options, selected-plan fingerprint, and the
per-concept result offset. A simple first implementation reruns deterministic
retrieval and slices the same ranked candidate pool at that offset; caching the
pool is an optional later optimization. Pagination continues that pool;
increasing discovery budgets is a separate search and may change segmentation.
`has_more` means that the known pool has more hits. `discovery_truncated` means
additional matches may exist beyond it, even if `has_more` is false. Exact
posting lists remain separately addressable for a caller requesting exhaustive
exact lookup; the default candidate limit does not imply exhaustive results.

Resolve runtime keys to `LexicalUnit` only after limiting results. The caller
receives every original gloss on the selected unit without search-driven
reordering.

## 12. Consumer API

Suggested public shape (illustrative Rust, not a committed ABI):

```rust
pub struct EnglishSearchOptions {
    pub completion: CompletionMode,
    pub limit: usize,                 // default 100 unique units
    pub per_concept_limit: usize,     // default 20
}

pub struct EnglishSearchResult<'a> {
    pub concepts: Vec<EnglishConcept>,
    pub entries: Vec<&'a LexicalUnit>, // unique, fairly merged
    pub discovery_truncated: bool,
    pub has_more: bool,
}

pub struct EnglishConcept {
    pub query_bytes: std::ops::Range<usize>,
    pub hits: Vec<EnglishHit>,
    pub next_cursor: Option<EnglishCursor>,
}

pub struct EnglishHit {
    pub runtime_key: u32,
    pub evidence: EnglishMatchEvidence,
}

pub fn search_english(
    raw: &str,
    options: EnglishSearchOptions,
) -> Result<EnglishSearchResult<'static>, EnglishSearchError>;
```

Keep `query_by_english(raw)` as a convenience wrapper returning the flattened
entries with documented default limits. This introduces bounded results where
the old API returned all hits; callers needing more should use the structured
API and continuation. Preserve raw trailing whitespace until the English path
has made its completion decision. A general search dispatcher must not trim
that information away first.
The structured API reports oversized input and invalid/stale cursors explicitly.
The convenience wrapper returns an empty vector for oversized input; document
that limitation and use the structured API for application-facing error handling.

Inside Syng, invoke the structured API and serialize unique entries once.
Concept hit references can point into that returned entry list at the application
boundary. Discard stale per-keystroke responses using a query generation number.
Measure serialization/UI work separately from core Rust retrieval.

## 13. Performance and size decisions

Initial targets from the handoff: warm core Rust search p50 below 1 ms, p95 below
2 ms, and p99 below 5 ms on named representative hardware. Include difficult
common-word and prefix queries, not just exact lookups. Measure mobile hardware
before making a mobile latency claim. Initialization and full application
response time are separate measurements.

Keep sections in immutable bytes, using `include_bytes!` or a loaded byte buffer
owned for the index lifetime. FST readers and fixed arrays borrow those bytes;
decode posting blocks on demand. Avoid reconstructing all indexes as heap
HashMaps. The canonical data store can follow its existing loading strategy.

Earlier exploratory measurements on the current generated corpus found roughly
301,187 normalized phrases, 105,542 tokens, 408,505 phrase-to-unit postings, and
1,358,283 token-to-unit postings. A simplified phrase FST plus compressed unit
postings was about 9.45 MiB; a token FST plus compressed unit postings was about
3.26 MiB. These figures used a provisional normalizer and omitted the text
catalog, token positions, grammatical/spelling aliases, ranking metadata, and
morphology proposed here. They are feasibility context, not a 12.7 MiB estimate
for this architecture.

The same exploration found `des` covered 116 vocabulary tokens and approximately
1,831 units, while `d` covered 4,250 tokens and approximately 40,616 units. That
supports a three-character initial prefix threshold and bounded retrieval.

The compressed artifact has a 22 MiB publication ceiling. Larger builds print
all raw section sizes and fail before atomic publication, preserving the prior
output. The generator records compressed and uncompressed lengths and SHA-256
digests in the manifest, and logical counts plus raw section sizes in
`build-report.json`.

The pinned 2026-09-15 corpus currently produces 123,912 tokens, 664,011 texts,
987,173 bindings, 3,860,398 positioned occurrences, and 1,393,563 ranked direct
hits. The complete container is 56,226,298 bytes raw and 22,056,532 bytes after
the specified compression (SHA-256
`08ad9f60dd3703446f090682aab557f848262c57277d1aab12f28ec0120eea9e`), within
the publication ceiling.

The future consumer build script should decompress `english.search.zst` into
`OUT_DIR/english.search` and each `.dictionary.zst` archive to its corresponding
`.dictionary` filename. Runtime resident memory, initialization, ranking, and
query latency are measured when `chinese_dictionary` adopts the shared reader.

All five schema-enveloped bincode artifacts are also published only as
deterministic level-19 Zstandard frames. On the pinned corpus their combined
size falls from 146,687,816 bytes to 34,150,044 bytes. Together with the English
index and unchanged supporting files, the complete validated bundle is
73,270,590 bytes.

## 14. Behavior cases for implementation

These are fixture expectations, conditional on the indicated searchable texts
being present. Match quality in the real corpus is a separate measurement.

- `hello my name is`, with both phrases available: `[hello] [my name is]`;
  no independent `my`, `name`, or `is` results.
- Same query without `my name is`, but with `my` and `name is`:
  `[hello] [my] [name is]`.
- `pri design`, with no match for `pri`: skip `pri`, select `[design]`.
- `print des`, with `print design` available and completion enabled:
  one two-token concept matching `print design`.
- `des ` or completion disabled: no `design` hit solely due to prefix matching.
- `is`, `too`, `a`: exact single-word definitions remain first-class results.
- `run`, `runs`, `running`, `ran`: all discover a fixture unit with `to run`,
  and all discover another unit whose only searchable text is `running`.
- `to run`: one two-token concept; no separate `to` concept.
- `happy`, `be happy`, `to be happy`, `is happy`: discover one another.
- `store`, `a store`, `the store`: discover one another; literal form first
  among otherwise comparable whole matches.
- `3d`: discover a fixture gloss `3-D` without losing the digit.
- `give up`: prefer the two-token span if available; do not reduce it to `give`.
- `medicine shop`: match adjacent occurrences inside one text; reject a
  fabricated two-token match assembled from two separate definitions.
- `very very`: require two token occurrences, not one reused occurrence.
- A seven-token full phrase: recognize it as one span if indexed.
- Two units with equal match quality: deterministic order across repeated runs
  of the same bundle.
- A unit matching two selected concepts: one flat result, two concept associations.
- Discovery budget exhausted: retain literal matches and expose truncation.

## 15. Work split and implementation sequence

### In this generator repository

1. Introduce the shared normalization/format crate and versioned search types.
2. Add `src/english/` modules for text extraction, morphology compilation,
   token/phrase indexes, postings, and writing `english.search`.
3. Build indexes after assigning the existing sorted lexical-unit runtime keys.
4. Add the pinned morphology input and inflection rule/override version.
5. Wire the search artifact into `src/bundle.rs` and the existing manifest.
6. Measure each section and the narrow retrieval slice before freezing the codec.
7. Switch bundle schema/output to the new artifact. Consumer migration remains separate.

### In chinese_dictionary

1. Adopt the current `LexicalUnit` bundle model as part of its already-required
   migration from the legacy `WordEntry` decoder.
2. Add the immutable English index reader and shared normalization dependency.
3. Implement literal/grammatical whole-phrase retrieval and single-token lookup.
4. Add morphology, positional containment, and concept dynamic programming.
5. Add contextual final-token completion with independent work budgets.
6. Add ranking, bounded grouped results, cursors, and the compatibility wrapper.
7. Measure per-keystroke behavior with Syng's actual call/serialization pattern.

Implement each layer against the same format contract. Generator fixtures use
the shared reader to exercise generated data before the production consumer
changes. No query-planning or search CLI is included here; production search
policy belongs in `chinese_dictionary`.

### What can be decided now

The generator needs searchable text extraction, preserved digits, grammar
aliases, morphology data, phrase lookup, token lookup, and compact ranked
postings. The proposed positional text catalog provides richer contained-phrase
discovery without adding domain gloss identity. These requirements follow from
the runtime behavior above.

The byte codecs, morphology coverage, and contained-phrase supporting data are
fixed for search-format version 1. Candidate budgets, concept selection,
pagination, prefix policy, behavioral ranking, and latency targets remain future
`chinese_dictionary` decisions.

## 16. Repository basis

- Generator `src/english/`: compiles complete glosses, deterministic aliases,
  positional occurrences, ranked direct postings, and morphology into the
  versioned search container.
- Generator `src/model.rs`: `LexicalUnit.english` contains structured
  `Definition` values, including separate leaf gloss and contextual metadata.
- Generator `docs/source-decisions.md`: source definition boundaries are
  deliberately preserved; derived search views must respect that distinction.
- Generator bundle schema 5 intentionally replaces the legacy exact-key English
  representation.
- Consumer `src/chinese_dictionary.rs`: current English lookup uses a maximum
  four-token, greedy exact-key scan and returns `WordEntry` references.
- Original handoff: `/Users/preston/Downloads/chinese_dictionary_english_search_handoff.md`.
  Its general direction informed this design; later conversation decisions
  override its stop-word exclusions and gloss-identity recommendations.
