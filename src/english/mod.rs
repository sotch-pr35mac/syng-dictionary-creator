//! Generator-owned compiler for the English lexical-search artifact.

mod morphology;

use crate::model::LexicalUnit;
use anyhow::{Context, Result, bail};
use fst::{Map, MapBuilder, Streamer};
use morphology::{Morphology, OVERRIDE_REVISION, WORDNET_REVISION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use syng_english_search_format::{
    ALL_DERIVATION_FLAGS, DERIVATION_APOSTROPHE_REMOVED, DERIVATION_GRAMMAR_REDUCED,
    DERIVATION_HYPHENS_JOINED, DERIVATION_HYPHENS_SEPARATED, DERIVATION_PARENTHETICAL_OMISSION,
    DERIVATION_SEMICOLON, EnglishSearchIndex, GRAMMAR_VERSION, MORPHOLOGY_VERSION,
    NORMALIZATION_VERSION, SEARCH_FORMAT_VERSION, Section, SectionCodec, SectionKind,
    normalize_text, read_uleb128, reduce_optional_grammar, write_container, write_uleb128,
};

pub(crate) const MAXIMUM_SIZE: u64 = 22 * 1024 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct EnglishSearchReport {
    pub(crate) compressed_bytes: u64,
    pub(crate) uncompressed_bytes: u64,
    pub(crate) compressed_sha256: String,
    pub(crate) uncompressed_sha256: String,
    pub(crate) tokens: u32,
    pub(crate) texts: u32,
    pub(crate) bindings: u32,
    pub(crate) occurrences: u32,
    pub(crate) direct_hits: u32,
    pub(crate) morphology_families: u32,
    pub(crate) morphology_surfaces: u32,
    pub(crate) maximum_text_tokens: u32,
    pub(crate) maximum_phrase_bytes: u32,
    pub(crate) section_raw_bytes: BTreeMap<String, u64>,
}

pub(crate) struct BuiltEnglishSearch {
    pub(crate) raw: Vec<u8>,
    pub(crate) compressed: Vec<u8>,
    pub(crate) report: EnglishSearchReport,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Binding {
    runtime_key: u32,
    flags: u16,
}

type ByteRange = (u64, u64);
type HitScore = (u8, u8, u16, u32, u16);
type HitPostings = BTreeMap<u32, HitScore>;
type EncodedOccurrences = (Vec<u8>, Vec<ByteRange>);
type EncodedHits = (Vec<u8>, Vec<ByteRange>, Vec<u32>, u32);

struct MetadataCounts {
    tokens: u32,
    texts: u32,
    bindings: u32,
    occurrences: usize,
    direct_hits: u32,
    maximum_text_tokens: u32,
    maximum_phrase_bytes: u32,
}

pub(crate) fn build(
    units: &BTreeMap<u32, LexicalUnit>,
    wordnet_path: &Path,
) -> Result<BuiltEnglishSearch> {
    let mut collected = BTreeMap::<String, BTreeMap<u32, Vec<u16>>>::new();
    for (&runtime_key, unit) in units {
        for definition in &unit.english {
            for (source, source_flags) in structural_views(&definition.gloss.value) {
                let normalized = normalize_text(&source);
                if normalized.is_empty() {
                    continue;
                }
                for (spelling, spelling_flags) in spelling_views(&normalized) {
                    add_evidence(
                        &mut collected,
                        spelling.clone(),
                        runtime_key,
                        source_flags | spelling_flags,
                    );
                    let words = spelling.split(' ').collect::<Vec<_>>();
                    if let Some(reduced) = reduce_optional_grammar(&words) {
                        add_evidence(
                            &mut collected,
                            reduced.join(" "),
                            runtime_key,
                            source_flags | spelling_flags | DERIVATION_GRAMMAR_REDUCED,
                        );
                    }
                }
            }
        }
    }

    let texts = collected
        .into_iter()
        .map(|(text, bindings)| {
            let bindings = bindings
                .into_iter()
                .flat_map(|(runtime_key, flags)| {
                    flags
                        .into_iter()
                        .map(move |flags| Binding { runtime_key, flags })
                })
                .collect::<Vec<_>>();
            (text, bindings)
        })
        .collect::<Vec<_>>();
    let vocabulary = texts
        .iter()
        .flat_map(|(text, _)| text.split(' ').map(str::to_owned))
        .collect::<BTreeSet<_>>();
    let token_ids = vocabulary
        .iter()
        .enumerate()
        .map(|(index, token)| Ok((token.clone(), u32::try_from(index)?)))
        .collect::<Result<BTreeMap<_, _>, std::num::TryFromIntError>>()?;
    let corpus = vocabulary.clone();
    let morphology = morphology::build(wordnet_path, &corpus)?;
    encode(units.len(), &texts, &vocabulary, &token_ids, &morphology)
}

fn structural_views(gloss: &str) -> Vec<(String, u16)> {
    let Some((alternatives, omitted)) = balanced_parts(gloss) else {
        return vec![(gloss.to_owned(), 0)];
    };
    let mut views = BTreeSet::from([(gloss.to_owned(), 0_u16)]);
    if omitted.trim() != gloss.trim() && !omitted.trim().is_empty() {
        views.insert((omitted, DERIVATION_PARENTHETICAL_OMISSION));
    }
    if alternatives.len() > 1 {
        for alternative in alternatives {
            if !alternative.trim().is_empty() {
                views.insert((alternative.clone(), DERIVATION_SEMICOLON));
                if let Some((_, omitted)) = balanced_parts(&alternative)
                    && omitted.trim() != alternative.trim()
                    && !omitted.trim().is_empty()
                {
                    views.insert((
                        omitted,
                        DERIVATION_SEMICOLON | DERIVATION_PARENTHETICAL_OMISSION,
                    ));
                }
            }
        }
    }
    views.into_iter().collect()
}

fn balanced_parts(value: &str) -> Option<(Vec<String>, String)> {
    let mut stack = Vec::new();
    let mut alternatives = Vec::new();
    let mut start = 0;
    let mut omitted = String::new();
    let mut omit_start = None;
    for (at, ch) in value.char_indices() {
        match ch {
            '(' | '[' => {
                if stack.is_empty() {
                    omitted.push_str(&value[omit_start.unwrap_or(0)..at]);
                    omit_start = Some(at + ch.len_utf8());
                }
                stack.push(ch);
            }
            ')' | ']' => {
                let expected = if ch == ')' { '(' } else { '[' };
                if stack.pop() != Some(expected) {
                    return None;
                }
                if stack.is_empty() {
                    omit_start = Some(at + ch.len_utf8());
                }
            }
            ';' if stack.is_empty() => {
                alternatives.push(value[start..at].to_owned());
                start = at + 1;
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return None;
    }
    alternatives.push(value[start..].to_owned());
    if let Some(start) = omit_start {
        omitted.push_str(&value[start..]);
    } else {
        omitted = value.to_owned();
    }
    Some((alternatives, omitted))
}

fn spelling_views(normalized: &str) -> Vec<(String, u16)> {
    let apostrophe = [
        (normalized.to_owned(), 0),
        (normalized.replace('\'', ""), DERIVATION_APOSTROPHE_REMOVED),
    ];
    let mut views = BTreeSet::new();
    for (form, flags) in apostrophe {
        views.insert((form.clone(), flags));
        if form.contains('-') {
            views.insert((form.replace('-', ""), flags | DERIVATION_HYPHENS_JOINED));
            views.insert((
                form.replace('-', " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                flags | DERIVATION_HYPHENS_SEPARATED,
            ));
        }
    }
    views
        .into_iter()
        .filter(|(text, _)| !text.is_empty())
        .collect()
}

fn add_evidence(
    catalog: &mut BTreeMap<String, BTreeMap<u32, Vec<u16>>>,
    text: String,
    runtime_key: u32,
    flags: u16,
) {
    let evidence = catalog
        .entry(text)
        .or_default()
        .entry(runtime_key)
        .or_default();
    if evidence
        .iter()
        .any(|existing| existing & flags == *existing)
    {
        return;
    }
    evidence.retain(|existing| flags & *existing != flags);
    evidence.push(flags);
    evidence.sort_unstable();
}

fn encode(
    unit_count: usize,
    texts: &[(String, Vec<Binding>)],
    vocabulary: &BTreeSet<String>,
    token_ids: &BTreeMap<String, u32>,
    morphology: &Morphology,
) -> Result<BuiltEnglishSearch> {
    let token_count = u32::try_from(vocabulary.len())?;
    let text_count = u32::try_from(texts.len())?;
    let mut phrase_builder = MapBuilder::memory();
    let mut token_builder = MapBuilder::memory();
    for (token, &id) in token_ids {
        token_builder.insert(token, u64::from(id))?;
    }
    for (index, (text, _)) in texts.iter().enumerate() {
        phrase_builder.insert(text, u64::try_from(index)?)?;
    }
    let token_fst = token_builder.into_inner()?;
    let phrase_fst = phrase_builder.into_inner()?;
    let token_strings = encode_strings(vocabulary.iter().map(String::as_str))?;

    let mut text_tokens = Vec::new();
    let mut bindings_bytes = Vec::new();
    let mut text_token_offsets = Vec::new();
    let mut text_binding_offsets = Vec::new();
    let mut text_token_counts = Vec::new();
    let mut text_best_flags = Vec::new();
    let mut occurrences = vec![Vec::<(u32, u32)>::new(); vocabulary.len()];
    let mut hit_candidates = vec![HitPostings::new(); vocabulary.len()];
    let mut binding_count = 0_u32;
    let mut maximum_text_tokens = 0_u32;
    let mut maximum_phrase_bytes = 0_u32;
    for (text_index, (text, bindings)) in texts.iter().enumerate() {
        let ids = text
            .split(' ')
            .map(|token| token_ids[token])
            .collect::<Vec<_>>();
        let text_key = u32::try_from(text_index)?;
        text_token_offsets.push(u64::try_from(text_tokens.len())?);
        for &id in &ids {
            write_uleb128(u64::from(id), &mut text_tokens);
        }
        let binding_offset = u64::try_from(bindings_bytes.len())?;
        text_binding_offsets.push(binding_offset);
        let best_flags = bindings
            .iter()
            .map(|binding| binding.flags)
            .min_by_key(|flags| evidence_key(*flags))
            .unwrap_or(0);
        let mut previous_runtime = 0_u32;
        for binding in bindings {
            write_uleb128(
                u64::from(binding.runtime_key - previous_runtime),
                &mut bindings_bytes,
            );
            write_uleb128(u64::from(binding.flags), &mut bindings_bytes);
            previous_runtime = binding.runtime_key;
            binding_count += 1;
        }
        text_token_counts.push(u32::try_from(ids.len())?);
        text_best_flags.push(best_flags);
        maximum_text_tokens = maximum_text_tokens.max(u32::try_from(ids.len())?);
        maximum_phrase_bytes = maximum_phrase_bytes.max(u32::try_from(text.len())?);
        for (position, &token_id) in ids.iter().enumerate() {
            occurrences[token_id as usize].push((text_key, u32::try_from(position)?));
            for binding in bindings {
                let score = (
                    u8::from(ids.len() != 1),
                    u8::from(
                        binding.flags
                            & (DERIVATION_APOSTROPHE_REMOVED
                                | DERIVATION_HYPHENS_JOINED
                                | DERIVATION_HYPHENS_SEPARATED
                                | DERIVATION_GRAMMAR_REDUCED)
                            != 0,
                    ),
                    binding.flags & DERIVATION_PARENTHETICAL_OMISSION,
                    u32::try_from(ids.len())?,
                    binding.flags.count_ones() as u16,
                );
                hit_candidates[token_id as usize]
                    .entry(binding.runtime_key)
                    .and_modify(|old| {
                        if score < *old {
                            *old = score;
                        }
                    })
                    .or_insert(score);
            }
        }
    }

    text_token_offsets.push(u64::try_from(text_tokens.len())?);
    text_binding_offsets.push(u64::try_from(bindings_bytes.len())?);
    let text_meta = encode_text_meta(
        &text_token_offsets,
        &text_binding_offsets,
        &text_token_counts,
        &text_best_flags,
    )?;

    let (occurrence_bytes, occurrence_ranges) = encode_occurrences(&occurrences)?;
    let (hit_bytes, hit_ranges, hit_counts, direct_hits) = encode_hits(&hit_candidates);
    let token_meta = encode_token_meta(&occurrences, &occurrence_ranges, &hit_ranges, &hit_counts)?;
    let (morph_fst, morph_analyses) = encode_morph_analyses(morphology)?;
    let morph_tokens = encode_morph_tokens(morphology, token_ids);
    let counts = MetadataCounts {
        tokens: token_count,
        texts: text_count,
        bindings: binding_count,
        occurrences: occurrences.iter().map(Vec::len).sum::<usize>(),
        direct_hits,
        maximum_text_tokens,
        maximum_phrase_bytes,
    };
    let mut metadata_section_sizes = BTreeMap::from([
        (SectionKind::TokenFst, u64::try_from(token_fst.len())?),
        (
            SectionKind::TokenStrings,
            u64::try_from(token_strings.len())?,
        ),
        (SectionKind::TokenMeta, u64::try_from(token_meta.len())?),
        (SectionKind::PhraseFst, u64::try_from(phrase_fst.len())?),
        (SectionKind::TextMeta, u64::try_from(text_meta.len())?),
        (SectionKind::TextTokens, u64::try_from(text_tokens.len())?),
        (
            SectionKind::TextBindings,
            u64::try_from(bindings_bytes.len())?,
        ),
        (
            SectionKind::TokenOccurrences,
            u64::try_from(occurrence_bytes.len())?,
        ),
        (SectionKind::TokenHits, u64::try_from(hit_bytes.len())?),
        (SectionKind::MorphologyFst, u64::try_from(morph_fst.len())?),
        (
            SectionKind::MorphologyAnalyses,
            u64::try_from(morph_analyses.len())?,
        ),
        (
            SectionKind::MorphologyTokens,
            u64::try_from(morph_tokens.len())?,
        ),
        (SectionKind::Metadata, 0),
    ]);
    let preliminary_metadata = encode_metadata(&counts, morphology, &metadata_section_sizes)?;
    metadata_section_sizes.insert(
        SectionKind::Metadata,
        u64::try_from(preliminary_metadata.len())?,
    );
    let metadata = encode_metadata(&counts, morphology, &metadata_section_sizes)?;

    let sections = vec![
        section(SectionKind::TokenFst, token_count, token_fst),
        section(SectionKind::TokenStrings, token_count, token_strings),
        section(SectionKind::TokenMeta, token_count, token_meta),
        section(SectionKind::PhraseFst, text_count, phrase_fst),
        section(SectionKind::TextMeta, text_count, text_meta),
        section(SectionKind::TextTokens, text_count, text_tokens),
        section(SectionKind::TextBindings, binding_count, bindings_bytes),
        section(SectionKind::TokenOccurrences, token_count, occurrence_bytes),
        section(SectionKind::TokenHits, token_count, hit_bytes),
        section(
            SectionKind::MorphologyFst,
            u32::try_from(morphology.surfaces.len())?,
            morph_fst,
        ),
        section(
            SectionKind::MorphologyAnalyses,
            u32::try_from(morphology.surfaces.len())?,
            morph_analyses,
        ),
        section(
            SectionKind::MorphologyTokens,
            u32::try_from(morphology.families.len())?,
            morph_tokens,
        ),
        section(SectionKind::Metadata, 1, metadata),
    ];
    let section_raw_bytes = sections
        .iter()
        .map(|section| (format!("{:?}", section.kind), section.bytes.len() as u64))
        .collect();
    let raw = write_container(u32::try_from(unit_count)?, sections)?;
    validate_raw(&raw, u32::try_from(unit_count)?)?;
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 19)?;
    encoder.include_checksum(true)?;
    encoder.include_contentsize(true)?;
    encoder.set_pledged_src_size(Some(raw.len() as u64))?;
    encoder.write_all(&raw)?;
    let compressed = encoder.finish()?;
    let report = EnglishSearchReport {
        compressed_bytes: compressed.len() as u64,
        uncompressed_bytes: raw.len() as u64,
        compressed_sha256: format!("{:x}", Sha256::digest(&compressed)),
        uncompressed_sha256: format!("{:x}", Sha256::digest(&raw)),
        tokens: token_count,
        texts: text_count,
        bindings: binding_count,
        occurrences: u32::try_from(occurrences.iter().map(Vec::len).sum::<usize>())?,
        direct_hits,
        morphology_families: u32::try_from(morphology.families.len())?,
        morphology_surfaces: u32::try_from(morphology.surfaces.len())?,
        maximum_text_tokens,
        maximum_phrase_bytes,
        section_raw_bytes,
    };
    Ok(BuiltEnglishSearch {
        raw,
        compressed,
        report,
    })
}

fn section(kind: SectionKind, item_count: u32, bytes: Vec<u8>) -> Section {
    Section {
        kind,
        codec: SectionCodec::RawV1,
        item_count,
        bytes,
    }
}

fn encode_strings<'a>(strings: impl ExactSizeIterator<Item = &'a str> + Clone) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut data = Vec::new();
    bytes.extend_from_slice(&u32::try_from(strings.len())?.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    let mut offsets = Vec::new();
    for value in strings.clone() {
        offsets.push(u64::try_from(data.len())?);
        data.extend_from_slice(value.as_bytes());
    }
    offsets.push(u64::try_from(data.len())?);
    let offset_table = encode_offset_table(&offsets)?;
    bytes.extend_from_slice(&u64::try_from(offset_table.len())?.to_le_bytes());
    bytes.extend_from_slice(&offset_table);
    bytes.extend_from_slice(&data);
    Ok(bytes)
}

fn encode_text_meta(
    token_offsets: &[u64],
    binding_offsets: &[u64],
    token_counts: &[u32],
    best_flags: &[u16],
) -> Result<Vec<u8>> {
    if token_offsets.len() != token_counts.len() + 1
        || binding_offsets.len() != token_counts.len() + 1
        || best_flags.len() != token_counts.len()
    {
        bail!("inconsistent text metadata columns");
    }
    let mut output = Vec::new();
    let token_table = encode_offset_table(token_offsets)?;
    let binding_table = encode_offset_table(binding_offsets)?;
    output.extend_from_slice(&u32::try_from(token_counts.len())?.to_le_bytes());
    output.extend_from_slice(&0_u32.to_le_bytes());
    output.extend_from_slice(&u64::try_from(token_table.len())?.to_le_bytes());
    output.extend_from_slice(&u64::try_from(binding_table.len())?.to_le_bytes());
    output.extend_from_slice(&token_table);
    output.extend_from_slice(&binding_table);
    for (&count, &flags) in token_counts.iter().zip(best_flags) {
        output.extend_from_slice(&count.to_le_bytes());
        output.extend_from_slice(&flags.to_le_bytes());
        output.extend_from_slice(&0_u16.to_le_bytes());
    }
    Ok(output)
}

fn encode_offset_table(offsets: &[u64]) -> Result<Vec<u8>> {
    let blocks = offsets.chunks(128).collect::<Vec<_>>();
    let mut output = Vec::new();
    output.extend_from_slice(&u32::try_from(offsets.len())?.to_le_bytes());
    output.extend_from_slice(&u32::try_from(blocks.len())?.to_le_bytes());
    let directory_at = output.len();
    output.resize(directory_at + blocks.len() * 24, 0);
    let mut payload = Vec::new();
    for (block_index, block) in blocks.into_iter().enumerate() {
        let at = directory_at + block_index * 24;
        output[at..at + 4].copy_from_slice(&u32::try_from(block_index * 128)?.to_le_bytes());
        output[at + 4..at + 8].copy_from_slice(&u32::try_from(block.len())?.to_le_bytes());
        output[at + 8..at + 16].copy_from_slice(&block[0].to_le_bytes());
        output[at + 16..at + 24].copy_from_slice(&u64::try_from(payload.len())?.to_le_bytes());
        let mut previous = block[0];
        for &offset in &block[1..] {
            write_uleb128(
                offset.checked_sub(previous).context("unsorted offsets")?,
                &mut payload,
            );
            previous = offset;
        }
    }
    output.extend_from_slice(&payload);
    Ok(output)
}

fn encode_occurrences(all: &[Vec<(u32, u32)>]) -> Result<EncodedOccurrences> {
    let mut output = Vec::new();
    let mut ranges = Vec::new();
    for records in all {
        let start = output.len();
        let blocks = records.chunks(128).collect::<Vec<_>>();
        output.extend_from_slice(&u32::try_from(blocks.len())?.to_le_bytes());
        let directory_at = output.len();
        output.resize(directory_at + blocks.len() * 16, 0);
        let mut encoded_blocks = Vec::new();
        for (index, block) in blocks.into_iter().enumerate() {
            let mut payload = Vec::new();
            let first = block.first().map_or(0, |record| record.0);
            let mut previous = first;
            for &(text, position) in block {
                write_uleb128(u64::from(text - previous), &mut payload);
                write_uleb128(u64::from(position), &mut payload);
                previous = text;
            }
            let at = directory_at + index * 16;
            output[at..at + 4].copy_from_slice(&first.to_le_bytes());
            output[at + 4..at + 8].copy_from_slice(&u32::try_from(block.len())?.to_le_bytes());
            output[at + 8..at + 16]
                .copy_from_slice(&u64::try_from(encoded_blocks.len())?.to_le_bytes());
            write_uleb128(u64::try_from(payload.len())?, &mut encoded_blocks);
            encoded_blocks.extend_from_slice(&payload);
        }
        output.extend_from_slice(&encoded_blocks);
        ranges.push((start as u64, (output.len() - start) as u64));
    }
    Ok((output, ranges))
}

fn encode_hits(all: &[HitPostings]) -> EncodedHits {
    let mut output = Vec::new();
    let mut ranges = Vec::new();
    let mut counts = Vec::new();
    let mut total = 0;
    for hits in all {
        let start = output.len();
        let mut buckets = BTreeMap::<HitScore, Vec<u32>>::new();
        for (&runtime, &score) in hits {
            buckets.entry(score).or_default().push(runtime);
        }
        write_uleb128(buckets.len() as u64, &mut output);
        for (score, mut keys) in buckets {
            keys.sort_unstable();
            output.push((score.0 << 2) | (score.1 << 1) | u8::from(score.2 != 0));
            write_uleb128(u64::from(score.3), &mut output);
            write_uleb128(u64::from(score.4), &mut output);
            write_uleb128(keys.len() as u64, &mut output);
            let mut previous = 0;
            for key in keys {
                write_uleb128(u64::from(key - previous), &mut output);
                previous = key;
                total += 1;
            }
        }
        ranges.push((start as u64, (output.len() - start) as u64));
        counts.push(hits.len() as u32);
    }
    (output, ranges, counts, total)
}

fn encode_token_meta(
    occurrences: &[Vec<(u32, u32)>],
    occ_ranges: &[(u64, u64)],
    hit_ranges: &[(u64, u64)],
    hit_counts: &[u32],
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    output.extend_from_slice(&u32::try_from(occurrences.len())?.to_le_bytes());
    output.extend_from_slice(&0_u32.to_le_bytes());
    for records in occurrences {
        output.extend_from_slice(&u32::try_from(records.len())?.to_le_bytes());
    }
    for records in occurrences {
        output.extend_from_slice(
            &u32::try_from(
                records
                    .iter()
                    .map(|record| record.0)
                    .collect::<BTreeSet<_>>()
                    .len(),
            )?
            .to_le_bytes(),
        );
    }
    let mut occurrence_offsets = occ_ranges.iter().map(|range| range.0).collect::<Vec<_>>();
    occurrence_offsets.push(occ_ranges.last().map_or(0, |range| range.0 + range.1));
    let mut hit_offsets = hit_ranges.iter().map(|range| range.0).collect::<Vec<_>>();
    hit_offsets.push(hit_ranges.last().map_or(0, |range| range.0 + range.1));
    let occurrence_table = encode_offset_table(&occurrence_offsets)?;
    let hit_table = encode_offset_table(&hit_offsets)?;
    output.extend_from_slice(&u64::try_from(occurrence_table.len())?.to_le_bytes());
    output.extend_from_slice(&u64::try_from(hit_table.len())?.to_le_bytes());
    output.extend_from_slice(&occurrence_table);
    output.extend_from_slice(&hit_table);
    for &count in hit_counts {
        output.extend_from_slice(&count.to_le_bytes());
    }
    Ok(output)
}

fn encode_morph_analyses(morphology: &Morphology) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut builder = MapBuilder::memory();
    let mut analyses = Vec::new();
    for (surface, families) in &morphology.surfaces {
        builder.insert(surface, u64::try_from(analyses.len())?)?;
        write_uleb128(families.len() as u64, &mut analyses);
        let mut previous = 0;
        for &family in families {
            write_uleb128(u64::from(family - previous), &mut analyses);
            previous = family;
        }
    }
    Ok((builder.into_inner()?, analyses))
}

fn encode_morph_tokens(morphology: &Morphology, token_ids: &BTreeMap<String, u32>) -> Vec<u8> {
    let mut output = Vec::new();
    for (family, forms) in &morphology.families {
        output.push(family.pos.code());
        write_uleb128(family.lemma.len() as u64, &mut output);
        output.extend_from_slice(family.lemma.as_bytes());
        let tokens = forms
            .iter()
            .filter_map(|form| token_ids.get(form).copied())
            .collect::<BTreeSet<_>>();
        write_uleb128(tokens.len() as u64, &mut output);
        let mut previous = 0;
        for token in tokens {
            write_uleb128(u64::from(token - previous), &mut output);
            previous = token;
        }
    }
    output
}

fn encode_metadata(
    counts: &MetadataCounts,
    morphology: &Morphology,
    section_sizes: &BTreeMap<SectionKind, u64>,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    for value in [
        SEARCH_FORMAT_VERSION,
        NORMALIZATION_VERSION,
        GRAMMAR_VERSION,
        MORPHOLOGY_VERSION,
    ] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    for value in [
        counts.tokens,
        counts.texts,
        counts.bindings,
        u32::try_from(counts.occurrences)?,
        counts.direct_hits,
        u32::try_from(morphology.families.len())?,
        u32::try_from(morphology.surfaces.len())?,
        counts.maximum_text_tokens,
        counts.maximum_phrase_bytes,
    ] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    for value in [WORDNET_REVISION, OVERRIDE_REVISION] {
        output.extend_from_slice(&u32::try_from(value.len())?.to_le_bytes());
        output.extend_from_slice(value.as_bytes());
    }
    output.extend_from_slice(&u32::try_from(section_sizes.len())?.to_le_bytes());
    for (&kind, &size) in section_sizes {
        output.extend_from_slice(&(kind as u32).to_le_bytes());
        output.extend_from_slice(&size.to_le_bytes());
    }
    Ok(output)
}

fn evidence_key(flags: u16) -> (u8, u8, u16) {
    (
        u8::from(
            flags
                & (DERIVATION_APOSTROPHE_REMOVED
                    | DERIVATION_HYPHENS_JOINED
                    | DERIVATION_HYPHENS_SEPARATED
                    | DERIVATION_GRAMMAR_REDUCED)
                != 0,
        ),
        u8::from(flags & DERIVATION_PARENTHETICAL_OMISSION != 0),
        flags.count_ones() as u16,
    )
}

pub(crate) fn validate_raw(raw: &[u8], expected_units: u32) -> Result<()> {
    let index = EnglishSearchIndex::parse(raw)?;
    if index.lexical_unit_count() != expected_units {
        bail!("English index lexical-unit count mismatch");
    }
    let token_count = index
        .sections()
        .iter()
        .find(|s| s.kind == SectionKind::TokenFst)
        .unwrap()
        .item_count;
    let text_count = index
        .sections()
        .iter()
        .find(|s| s.kind == SectionKind::PhraseFst)
        .unwrap()
        .item_count;
    let binding_count = section_item_count(&index, SectionKind::TextBindings);
    let morphology_family_count = section_item_count(&index, SectionKind::MorphologyTokens);
    let morphology_surface_count = section_item_count(&index, SectionKind::MorphologyFst);
    for (kind, expected) in [
        (SectionKind::TokenStrings, token_count),
        (SectionKind::TokenMeta, token_count),
        (SectionKind::TokenOccurrences, token_count),
        (SectionKind::TokenHits, token_count),
        (SectionKind::TextMeta, text_count),
        (SectionKind::TextTokens, text_count),
        (SectionKind::MorphologyAnalyses, morphology_surface_count),
    ] {
        if section_item_count(&index, kind) != expected {
            bail!("inconsistent section item count for {kind:?}");
        }
    }
    if section_item_count(&index, SectionKind::Metadata) != 1 {
        bail!("metadata section must contain one record");
    }
    validate_fst_values(index.section(SectionKind::TokenFst).unwrap(), token_count)?;
    validate_fst_values(index.section(SectionKind::PhraseFst).unwrap(), text_count)?;
    let maximum_phrase_bytes =
        maximum_fst_key_bytes(index.section(SectionKind::PhraseFst).unwrap())?;
    let maximum_text_tokens = validate_text_tokens(
        index.section(SectionKind::TextMeta).unwrap(),
        index.section(SectionKind::TextTokens).unwrap(),
        index.section(SectionKind::TextBindings).unwrap(),
        text_count,
        binding_count,
        token_count,
        expected_units,
    )?;
    let (occurrences, direct_hits) = validate_token_indexes(
        index.section(SectionKind::TokenMeta).unwrap(),
        index.section(SectionKind::TokenOccurrences).unwrap(),
        index.section(SectionKind::TokenHits).unwrap(),
        token_count,
        text_count,
        expected_units,
    )?;
    validate_morphology(&index)?;
    validate_metadata(
        &index,
        &[
            token_count,
            text_count,
            binding_count,
            occurrences,
            direct_hits,
            morphology_family_count,
            morphology_surface_count,
            maximum_text_tokens,
            maximum_phrase_bytes,
        ],
    )?;
    Ok(())
}

fn section_item_count(index: &EnglishSearchIndex<'_>, kind: SectionKind) -> u32 {
    index
        .sections()
        .iter()
        .find(|section| section.kind == kind)
        .unwrap()
        .item_count
}

fn validate_fst_values(bytes: &[u8], limit: u32) -> Result<()> {
    let map = Map::new(bytes)?;
    let mut stream = map.stream();
    let mut expected = 0_u64;
    while let Some((_key, value)) = stream.next() {
        if value != expected || value >= u64::from(limit) {
            bail!("FST values are not dense, ordered, and in range");
        }
        expected += 1;
    }
    if expected != u64::from(limit) {
        bail!("FST item count mismatch");
    }
    Ok(())
}

fn maximum_fst_key_bytes(bytes: &[u8]) -> Result<u32> {
    let map = Map::new(bytes)?;
    let mut stream = map.stream();
    let mut maximum = 0_u32;
    while let Some((key, _)) = stream.next() {
        maximum = maximum.max(u32::try_from(key.len())?);
    }
    Ok(maximum)
}

fn validate_text_tokens(
    meta: &[u8],
    bytes: &[u8],
    bindings: &[u8],
    expected_texts: u32,
    expected_bindings: u32,
    token_count: u32,
    units: u32,
) -> Result<u32> {
    if meta.len() < 24 {
        bail!("malformed text metadata");
    }
    let count = u32::from_le_bytes(meta[..4].try_into().unwrap()) as usize;
    if count != expected_texts as usize || meta[4..8] != [0; 4] {
        bail!("nonzero text metadata reserved field");
    }
    let token_table_len = usize::try_from(u64::from_le_bytes(meta[8..16].try_into().unwrap()))?;
    let binding_table_len = usize::try_from(u64::from_le_bytes(meta[16..24].try_into().unwrap()))?;
    let token_table_end = 24_usize
        .checked_add(token_table_len)
        .context("text metadata overflow")?;
    let binding_table_end = token_table_end
        .checked_add(binding_table_len)
        .context("text metadata overflow")?;
    let token_offsets = decode_offset_table(
        meta.get(24..token_table_end)
            .context("truncated token offset table")?,
    )?;
    let binding_offsets = decode_offset_table(
        meta.get(token_table_end..binding_table_end)
            .context("truncated binding offset table")?,
    )?;
    if token_offsets.len() != count + 1 || binding_offsets.len() != count + 1 {
        bail!("text offset table count mismatch");
    }
    let records_at = binding_table_end;
    if records_at.checked_add(count * 8) != Some(meta.len()) {
        bail!("malformed text metadata columns");
    }
    let mut decoded_bindings = 0_u32;
    let mut maximum_text_tokens = 0_u32;
    for index in 0..count {
        let mut cursor = usize::try_from(token_offsets[index])?;
        let token_end = usize::try_from(token_offsets[index + 1])?;
        let record_at = records_at + index * 8;
        let tokens_in_text = u32::from_le_bytes(meta[record_at..record_at + 4].try_into().unwrap());
        let best_flags = u16::from_le_bytes(meta[record_at + 4..record_at + 6].try_into().unwrap());
        if tokens_in_text == 0
            || best_flags & !ALL_DERIVATION_FLAGS != 0
            || meta[record_at + 6..record_at + 8] != [0; 2]
        {
            bail!("unknown best-evidence flag");
        }
        maximum_text_tokens = maximum_text_tokens.max(tokens_in_text);
        for _ in 0..tokens_in_text {
            if read_uleb128(bytes, &mut cursor)? >= u64::from(token_count) {
                bail!("out-of-range token ID");
            }
        }
        if cursor != token_end {
            bail!("text token offset does not end at its next record");
        }
        let mut binding_cursor = usize::try_from(binding_offsets[index])?;
        let binding_end = usize::try_from(binding_offsets[index + 1])?;
        let mut runtime = 0_u32;
        let mut previous = None;
        while binding_cursor < binding_end {
            runtime = runtime
                .checked_add(u32::try_from(read_uleb128(bindings, &mut binding_cursor)?)?)
                .context("binding delta overflow")?;
            let flags = u16::try_from(read_uleb128(bindings, &mut binding_cursor)?)?;
            if runtime >= units
                || flags & !ALL_DERIVATION_FLAGS != 0
                || previous.is_some_and(|value| value >= (runtime, flags))
            {
                bail!("invalid or unsorted text binding");
            }
            previous = Some((runtime, flags));
            decoded_bindings = decoded_bindings
                .checked_add(1)
                .context("binding count overflow")?;
        }
        if binding_cursor != binding_end || previous.is_none() {
            bail!("invalid text binding offset or empty binding list");
        }
    }
    if decoded_bindings != expected_bindings {
        bail!("text-binding item count mismatch");
    }
    Ok(maximum_text_tokens)
}

fn validate_token_indexes(
    meta: &[u8],
    occurrence_bytes: &[u8],
    hit_bytes: &[u8],
    token_count: u32,
    text_count: u32,
    unit_count: u32,
) -> Result<(u32, u32)> {
    if meta.len() < 24 {
        bail!("malformed token metadata");
    }
    let count = u32::from_le_bytes(meta[..4].try_into().unwrap());
    if count != token_count || meta[4..8] != [0; 4] {
        bail!("token metadata count or reserved field is invalid");
    }
    let count = count as usize;
    let occurrence_counts_at = 8;
    let document_counts_at = occurrence_counts_at + count * 4;
    let table_lengths_at = document_counts_at + count * 4;
    let occurrence_table_len = usize::try_from(u64::from_le_bytes(
        meta[table_lengths_at..table_lengths_at + 8]
            .try_into()
            .unwrap(),
    ))?;
    let hit_table_len = usize::try_from(u64::from_le_bytes(
        meta[table_lengths_at + 8..table_lengths_at + 16]
            .try_into()
            .unwrap(),
    ))?;
    let occurrence_table_at = table_lengths_at + 16;
    let hit_table_at = occurrence_table_at + occurrence_table_len;
    let hit_counts_at = hit_table_at + hit_table_len;
    if hit_counts_at.checked_add(count * 4) != Some(meta.len()) {
        bail!("malformed token metadata columns");
    }
    let occurrence_offsets = decode_offset_table(
        meta.get(occurrence_table_at..hit_table_at)
            .context("truncated occurrence offset table")?,
    )?;
    let hit_offsets = decode_offset_table(
        meta.get(hit_table_at..hit_counts_at)
            .context("truncated direct-hit offset table")?,
    )?;
    if occurrence_offsets.len() != count + 1 || hit_offsets.len() != count + 1 {
        bail!("token offset table count mismatch");
    }
    let mut total_occurrences = 0_u32;
    let mut total_hits = 0_u32;
    for token_index in 0..count {
        let occurrence_start = usize::try_from(occurrence_offsets[token_index])?;
        let occurrence_end = usize::try_from(occurrence_offsets[token_index + 1])?;
        let part = occurrence_bytes
            .get(occurrence_start..occurrence_end)
            .context("occurrence offset out of range")?;
        let mut decoded_occurrences = 0_u32;
        validate_one_occurrence_list(part, text_count, &mut decoded_occurrences)?;
        let expected_occurrences = read_column_u32(meta, occurrence_counts_at, token_index)?;
        if decoded_occurrences != expected_occurrences {
            bail!("token occurrence count mismatch");
        }
        total_occurrences = total_occurrences
            .checked_add(decoded_occurrences)
            .context("occurrence count overflow")?;

        let hit_start = usize::try_from(hit_offsets[token_index])?;
        let hit_end = usize::try_from(hit_offsets[token_index + 1])?;
        let part = hit_bytes
            .get(hit_start..hit_end)
            .context("direct-hit offset out of range")?;
        let decoded_hits = validate_one_hit_list(part, unit_count)?;
        if decoded_hits != read_column_u32(meta, hit_counts_at, token_index)? {
            bail!("direct-hit count mismatch");
        }
        total_hits = total_hits
            .checked_add(decoded_hits)
            .context("direct-hit count overflow")?;
    }
    Ok((total_occurrences, total_hits))
}

fn validate_one_occurrence_list(part: &[u8], text_count: u32, decoded: &mut u32) -> Result<()> {
    if part.len() < 4 {
        bail!("truncated occurrences");
    }
    let blocks = u32::from_le_bytes(part[..4].try_into().unwrap()) as usize;
    let directory_end = 4_usize
        .checked_add(
            blocks
                .checked_mul(16)
                .context("occurrence directory overflow")?,
        )
        .context("occurrence directory overflow")?;
    let payloads = part
        .get(directory_end..)
        .context("truncated occurrence directory")?;
    let mut prior_pair = None;
    let mut expected_payload_offset = 0_usize;
    for block_index in 0..blocks {
        let at = 4 + block_index * 16;
        let header = &part[at..at + 16];
        let first = u32::from_le_bytes(header[..4].try_into().unwrap());
        let count = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let payload_offset =
            usize::try_from(u64::from_le_bytes(header[8..16].try_into().unwrap()))?;
        if count == 0 || count > 128 || payload_offset != expected_payload_offset {
            bail!("invalid occurrence block directory");
        }
        let mut cursor = payload_offset;
        let payload_len = usize::try_from(read_uleb128(payloads, &mut cursor)?)?;
        let payload_end = cursor
            .checked_add(payload_len)
            .context("occurrence overflow")?;
        let payload = payloads
            .get(cursor..payload_end)
            .context("truncated occurrence payload")?;
        let mut cursor = 0_usize;
        let mut text = first;
        for _ in 0..count {
            text = text
                .checked_add(u32::try_from(read_uleb128(payload, &mut cursor)?)?)
                .context("occurrence delta overflow")?;
            let position = u32::try_from(read_uleb128(payload, &mut cursor)?)?;
            if text >= text_count || prior_pair.is_some_and(|pair| pair >= (text, position)) {
                bail!("unsorted or invalid occurrences");
            }
            prior_pair = Some((text, position));
            *decoded += 1;
        }
        if cursor != payload.len() {
            bail!("trailing occurrence bytes");
        }
        expected_payload_offset = payload_end;
    }
    if expected_payload_offset != payloads.len() {
        bail!("trailing occurrence-list bytes");
    }
    Ok(())
}

fn validate_one_hit_list(part: &[u8], unit_count: u32) -> Result<u32> {
    let mut cursor = 0_usize;
    let buckets = read_uleb128(part, &mut cursor)?;
    let mut prior_rank = None;
    let mut decoded = 0_u32;
    let mut seen = BTreeSet::new();
    for _ in 0..buckets {
        let packed = *part.get(cursor).context("truncated direct-hit bucket")?;
        cursor += 1;
        if packed & !0b111 != 0 {
            bail!("unknown direct-hit evidence bits");
        }
        let length = u32::try_from(read_uleb128(part, &mut cursor)?)?;
        let transformations = u16::try_from(read_uleb128(part, &mut cursor)?)?;
        let rank = (packed, length, transformations);
        if prior_rank.is_some_and(|previous| previous >= rank) {
            bail!("unsorted direct-hit buckets");
        }
        prior_rank = Some(rank);
        let keys = read_uleb128(part, &mut cursor)?;
        let mut runtime = 0_u32;
        let mut previous_runtime = None;
        for _ in 0..keys {
            runtime = runtime
                .checked_add(u32::try_from(read_uleb128(part, &mut cursor)?)?)
                .context("direct-hit delta overflow")?;
            if runtime >= unit_count
                || previous_runtime.is_some_and(|previous| previous >= runtime)
                || !seen.insert(runtime)
            {
                bail!("out-of-range, duplicate, or unsorted direct-hit runtime key");
            }
            previous_runtime = Some(runtime);
            decoded += 1;
        }
    }
    if cursor != part.len() {
        bail!("trailing direct-hit bytes");
    }
    Ok(decoded)
}

fn decode_offset_table(bytes: &[u8]) -> Result<Vec<u64>> {
    if bytes.len() < 8 {
        bail!("truncated offset table");
    }
    let count = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let block_count = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if block_count != count.div_ceil(128) {
        bail!("offset-table block count mismatch");
    }
    let directory_end = 8_usize
        .checked_add(
            block_count
                .checked_mul(24)
                .context("offset table overflow")?,
        )
        .context("offset table overflow")?;
    let payload = bytes
        .get(directory_end..)
        .context("truncated offset-table directory")?;
    let mut output = Vec::with_capacity(count);
    let mut expected_payload = 0_usize;
    for block_index in 0..block_count {
        let at = 8 + block_index * 24;
        let first_index = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let values = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let mut value = u64::from_le_bytes(bytes[at + 8..at + 16].try_into().unwrap());
        let payload_offset = usize::try_from(u64::from_le_bytes(
            bytes[at + 16..at + 24].try_into().unwrap(),
        ))?;
        if first_index != block_index * 128
            || values == 0
            || values > 128
            || payload_offset != expected_payload
        {
            bail!("invalid offset-table directory");
        }
        output.push(value);
        let mut cursor = payload_offset;
        for _ in 1..values {
            value = value
                .checked_add(read_uleb128(payload, &mut cursor)?)
                .context("offset-table delta overflow")?;
            output.push(value);
        }
        expected_payload = cursor;
    }
    if output.len() != count || expected_payload != payload.len() {
        bail!("offset-table length mismatch");
    }
    Ok(output)
}

fn read_column_u32(bytes: &[u8], start: usize, index: usize) -> Result<u32> {
    let at = start
        .checked_add(index.checked_mul(4).context("column offset overflow")?)
        .context("column offset overflow")?;
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .context("truncated count column")?
            .try_into()
            .unwrap(),
    ))
}

fn validate_metadata(index: &EnglishSearchIndex<'_>, expected_counts: &[u32; 9]) -> Result<()> {
    let bytes = index.section(SectionKind::Metadata).unwrap();
    let mut cursor = 0_usize;
    for expected in [
        SEARCH_FORMAT_VERSION,
        NORMALIZATION_VERSION,
        GRAMMAR_VERSION,
        MORPHOLOGY_VERSION,
    ] {
        let end = cursor.checked_add(4).context("metadata overflow")?;
        let actual = u32::from_le_bytes(
            bytes
                .get(cursor..end)
                .context("truncated metadata versions")?
                .try_into()
                .unwrap(),
        );
        if actual != expected {
            bail!("metadata version mismatch");
        }
        cursor = end;
    }
    for &expected in expected_counts {
        let end = cursor.checked_add(4).context("metadata overflow")?;
        let actual = u32::from_le_bytes(
            bytes
                .get(cursor..end)
                .context("truncated metadata counts")?
                .try_into()
                .unwrap(),
        );
        if actual != expected {
            bail!("metadata logical count mismatch");
        }
        cursor = end;
    }
    for expected in [WORDNET_REVISION, OVERRIDE_REVISION] {
        let length_end = cursor.checked_add(4).context("metadata overflow")?;
        let length = usize::try_from(u32::from_le_bytes(
            bytes
                .get(cursor..length_end)
                .context("truncated metadata revision length")?
                .try_into()
                .unwrap(),
        ))?;
        cursor = length_end;
        let revision_end = cursor.checked_add(length).context("metadata overflow")?;
        if bytes
            .get(cursor..revision_end)
            .context("truncated metadata revision")?
            != expected.as_bytes()
        {
            bail!("metadata revision mismatch");
        }
        cursor = revision_end;
    }
    let section_count_end = cursor.checked_add(4).context("metadata overflow")?;
    let section_count = usize::try_from(u32::from_le_bytes(
        bytes
            .get(cursor..section_count_end)
            .context("truncated metadata section count")?
            .try_into()
            .unwrap(),
    ))?;
    cursor = section_count_end;
    if section_count != index.sections().len() {
        bail!("metadata section-size count mismatch");
    }
    for expected in index.sections() {
        let end = cursor.checked_add(12).context("metadata overflow")?;
        let record = bytes
            .get(cursor..end)
            .context("truncated metadata section sizes")?;
        let kind = u32::from_le_bytes(record[..4].try_into().unwrap());
        let length = u64::from_le_bytes(record[4..12].try_into().unwrap());
        if kind != expected.kind as u32 || length != expected.length {
            bail!("metadata section size mismatch");
        }
        cursor = end;
    }
    if cursor != bytes.len() {
        bail!("trailing metadata bytes");
    }
    Ok(())
}

fn validate_morphology(index: &EnglishSearchIndex<'_>) -> Result<()> {
    let family_section = index
        .sections()
        .iter()
        .find(|s| s.kind == SectionKind::MorphologyTokens)
        .unwrap();
    let family_count = family_section.item_count;
    let surface_count = index
        .sections()
        .iter()
        .find(|s| s.kind == SectionKind::MorphologyFst)
        .unwrap()
        .item_count;
    let analyses = index.section(SectionKind::MorphologyAnalyses).unwrap();
    let map = Map::new(index.section(SectionKind::MorphologyFst).unwrap())?;
    let mut stream = map.stream();
    let mut entries = Vec::new();
    while let Some((key, offset)) = stream.next() {
        let surface = std::str::from_utf8(key).context("invalid morphology surface UTF-8")?;
        if normalize_text(surface) != surface || surface.contains(' ') {
            bail!("morphology surface is not one normalized token");
        }
        if entries.last().is_some_and(|&previous| previous >= offset) {
            bail!("unsorted morphology analysis offsets");
        }
        entries.push(offset);
    }
    if entries.len() != surface_count as usize {
        bail!("morphology surface count mismatch");
    }
    for (index, &offset) in entries.iter().enumerate() {
        let mut cursor = usize::try_from(offset)?;
        let end = entries.get(index + 1).map_or(analyses.len(), |&next| {
            usize::try_from(next).unwrap_or(usize::MAX)
        });
        let count = read_uleb128(analyses, &mut cursor)?;
        let mut family = 0_u32;
        let mut previous_family = None;
        for _ in 0..count {
            family = family
                .checked_add(u32::try_from(read_uleb128(analyses, &mut cursor)?)?)
                .context("family delta overflow")?;
            if family >= family_count || previous_family.is_some_and(|previous| previous >= family)
            {
                bail!("out-of-range or unsorted morphology family");
            }
            previous_family = Some(family);
        }
        if cursor != end || previous_family.is_none() {
            bail!("invalid or empty morphology analysis list");
        }
    }

    let family_bytes = index.section(SectionKind::MorphologyTokens).unwrap();
    let mut cursor = 0_usize;
    for _ in 0..family_count {
        let pos = *family_bytes
            .get(cursor)
            .context("truncated morphology family")?;
        cursor += 1;
        if !matches!(pos, 1 | 2) {
            bail!("unknown morphology part of speech");
        }
        let lemma_len = usize::try_from(read_uleb128(family_bytes, &mut cursor)?)?;
        let lemma_end = cursor
            .checked_add(lemma_len)
            .context("morphology lemma overflow")?;
        let lemma = std::str::from_utf8(
            family_bytes
                .get(cursor..lemma_end)
                .context("truncated morphology lemma")?,
        )?;
        if normalize_text(lemma) != lemma || lemma.contains(' ') {
            bail!("morphology lemma is not one normalized token");
        }
        cursor = lemma_end;
        let token_count = read_uleb128(family_bytes, &mut cursor)?;
        if token_count == 0 {
            bail!("morphology family does not intersect the corpus");
        }
        let mut token = 0_u32;
        let mut previous_token = None;
        for _ in 0..token_count {
            token = token
                .checked_add(u32::try_from(read_uleb128(family_bytes, &mut cursor)?)?)
                .context("morphology token delta overflow")?;
            if token
                >= index
                    .sections()
                    .iter()
                    .find(|section| section.kind == SectionKind::TokenFst)
                    .unwrap()
                    .item_count
                || previous_token.is_some_and(|previous| previous >= token)
            {
                bail!("out-of-range or unsorted morphology corpus token");
            }
            previous_token = Some(token);
        }
    }
    if cursor != family_bytes.len() {
        bail!("trailing morphology-family bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fst::IntoStreamer;

    #[test]
    fn extraction_obeys_balanced_grouping() {
        let views = structural_views("shop (medicine); pharmacy [formal]");
        assert!(views.contains(&("shop (medicine); pharmacy [formal]".to_owned(), 0)));
        assert!(views.contains(&(
            "shop ".to_owned(),
            DERIVATION_SEMICOLON | DERIVATION_PARENTHETICAL_OMISSION
        )));
        assert_eq!(
            structural_views("shop (medicine; pharmacy"),
            vec![("shop (medicine; pharmacy".to_owned(), 0)]
        );
    }

    #[test]
    fn evidence_removes_only_strictly_dominated_variants() {
        let mut catalog = BTreeMap::new();
        add_evidence(&mut catalog, "run".to_owned(), 1, 0);
        add_evidence(
            &mut catalog,
            "run".to_owned(),
            1,
            DERIVATION_GRAMMAR_REDUCED,
        );
        assert_eq!(catalog["run"][&1], vec![0]);
        let mut catalog = BTreeMap::new();
        add_evidence(&mut catalog, "x".to_owned(), 1, DERIVATION_SEMICOLON);
        add_evidence(
            &mut catalog,
            "x".to_owned(),
            1,
            DERIVATION_PARENTHETICAL_OMISSION,
        );
        assert_eq!(catalog["x"][&1].len(), 2);
    }

    #[test]
    fn logical_index_keeps_phrases_positions_prefixes_and_ranked_hits() {
        let source = BTreeMap::from([
            ("medicine".to_owned(), 0),
            ("medicine shop".to_owned(), 1),
            ("one two three four five six seven".to_owned(), 5),
            ("print design".to_owned(), 2),
            ("shop".to_owned(), 3),
            ("very very".to_owned(), 4),
        ]);
        let texts = source
            .iter()
            .map(|(text, &runtime_key)| {
                (
                    text.clone(),
                    vec![Binding {
                        runtime_key,
                        flags: 0,
                    }],
                )
            })
            .collect::<Vec<_>>();
        let vocabulary = texts
            .iter()
            .flat_map(|(text, _)| text.split(' ').map(str::to_owned))
            .collect::<BTreeSet<_>>();
        let token_ids = vocabulary
            .iter()
            .enumerate()
            .map(|(index, token)| (token.clone(), index as u32))
            .collect::<BTreeMap<_, _>>();
        let morphology = Morphology {
            families: Vec::new(),
            surfaces: BTreeMap::new(),
        };
        let first = encode(6, &texts, &vocabulary, &token_ids, &morphology).unwrap();
        let second = encode(6, &texts, &vocabulary, &token_ids, &morphology).unwrap();
        assert_eq!(first.raw, second.raw);
        assert_eq!(first.compressed, second.compressed);

        let index = EnglishSearchIndex::parse(&first.raw).unwrap();
        let phrases = Map::new(index.section(SectionKind::PhraseFst).unwrap()).unwrap();
        let tokens = Map::new(index.section(SectionKind::TokenFst).unwrap()).unwrap();
        let medicine_shop = phrases.get("medicine shop").unwrap() as usize;
        assert_eq!(
            decoded_text_tokens(&index, medicine_shop),
            [tokens.get("medicine").unwrap(), tokens.get("shop").unwrap(),]
        );
        let repeated = phrases.get("very very").unwrap() as usize;
        assert_eq!(
            decoded_text_tokens(&index, repeated),
            [tokens.get("very").unwrap(), tokens.get("very").unwrap(),]
        );
        let seven = phrases.get("one two three four five six seven").unwrap() as usize;
        assert_eq!(decoded_text_tokens(&index, seven).len(), 7);
        let mut token_prefix = tokens.range().ge("des").lt("det").into_stream();
        assert!(token_prefix.next().is_some_and(|(key, _)| key == b"design"));
        let mut phrase_prefix = phrases
            .range()
            .ge("print des")
            .lt("print det")
            .into_stream();
        assert!(
            phrase_prefix
                .next()
                .is_some_and(|(key, _)| key == b"print design")
        );
        assert_eq!(
            decoded_hit_keys(&index, tokens.get("medicine").unwrap() as usize),
            [0, 1]
        );

        let separate = vec![
            ("medicine".to_owned(), texts[0].1.clone()),
            ("shop".to_owned(), texts[4].1.clone()),
        ];
        let separate_vocabulary = BTreeSet::from(["medicine".to_owned(), "shop".to_owned()]);
        let separate_ids = BTreeMap::from([("medicine".to_owned(), 0), ("shop".to_owned(), 1)]);
        let separate_index = encode(
            6,
            &separate,
            &separate_vocabulary,
            &separate_ids,
            &morphology,
        )
        .unwrap();
        let separate_index = EnglishSearchIndex::parse(&separate_index.raw).unwrap();
        let separate_phrases =
            Map::new(separate_index.section(SectionKind::PhraseFst).unwrap()).unwrap();
        assert!(separate_phrases.get("medicine shop").is_none());
    }

    fn decoded_text_tokens(index: &EnglishSearchIndex<'_>, text: usize) -> Vec<u64> {
        let meta = index.section(SectionKind::TextMeta).unwrap();
        let token_table_len =
            usize::try_from(u64::from_le_bytes(meta[8..16].try_into().unwrap())).unwrap();
        let offsets = decode_offset_table(&meta[24..24 + token_table_len]).unwrap();
        let bytes = index.section(SectionKind::TextTokens).unwrap();
        let mut cursor = offsets[text] as usize;
        let end = offsets[text + 1] as usize;
        let mut result = Vec::new();
        while cursor < end {
            result.push(read_uleb128(bytes, &mut cursor).unwrap());
        }
        result
    }

    fn decoded_hit_keys(index: &EnglishSearchIndex<'_>, token: usize) -> Vec<u32> {
        let meta = index.section(SectionKind::TokenMeta).unwrap();
        let count = u32::from_le_bytes(meta[..4].try_into().unwrap()) as usize;
        let table_lengths_at = 8 + count * 8;
        let occurrence_table_len = usize::try_from(u64::from_le_bytes(
            meta[table_lengths_at..table_lengths_at + 8]
                .try_into()
                .unwrap(),
        ))
        .unwrap();
        let hit_table_len = usize::try_from(u64::from_le_bytes(
            meta[table_lengths_at + 8..table_lengths_at + 16]
                .try_into()
                .unwrap(),
        ))
        .unwrap();
        let hit_table_at = table_lengths_at + 16 + occurrence_table_len;
        let offsets =
            decode_offset_table(&meta[hit_table_at..hit_table_at + hit_table_len]).unwrap();
        let bytes = index.section(SectionKind::TokenHits).unwrap();
        let mut cursor = offsets[token] as usize;
        let end = offsets[token + 1] as usize;
        let buckets = read_uleb128(bytes, &mut cursor).unwrap();
        let mut result = Vec::new();
        for _ in 0..buckets {
            cursor += 1;
            read_uleb128(bytes, &mut cursor).unwrap();
            read_uleb128(bytes, &mut cursor).unwrap();
            let keys = read_uleb128(bytes, &mut cursor).unwrap();
            let mut runtime = 0_u32;
            for _ in 0..keys {
                runtime += u32::try_from(read_uleb128(bytes, &mut cursor).unwrap()).unwrap();
                result.push(runtime);
            }
        }
        assert_eq!(cursor, end);
        result
    }
}
