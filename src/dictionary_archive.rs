//! Schema-4 zero-copy dictionary archive contract.
//!
//! This module is intentionally mirrored in the consumer crate. The shared
//! fixture tests protect the contract without making the published consumer
//! depend on the generator.

use crate::model::{IDENTITY_VERSION, LexicalUnit, SCHEMA_VERSION};
use rkyv::{
    Archive, Place, Serialize,
    collections::swiss_table::{ArchivedHashMap, HashMapResolver},
    rancor::{Fallible, Source},
    ser::{Allocator, Writer},
};

/// Load factor used by the portable archived identity hash table.
pub(crate) const IDENTITY_LOAD_FACTOR: (usize, usize) = (7, 8);
/// Exact rkyv format controls used by both generator and consumer.
pub(crate) const ARCHIVE_CONTRACT: &[u8; 34] = b"syng-dictionary-rkyv-v4-le-a16-p32";

/// A half-open range in one flat runtime-key postings vector.
#[derive(Clone, Copy, Debug, Eq, PartialEq, rkyv::Archive, rkyv::Serialize)]
pub(crate) struct PostingRange {
    pub(crate) start: u32,
    pub(crate) len: u32,
}

/// Separate exact associations for one shared Chinese FST term.
#[derive(Clone, Copy, Debug, Eq, PartialEq, rkyv::Archive, rkyv::Serialize)]
pub(crate) struct ChinesePostings {
    pub(crate) simplified: PostingRange,
    pub(crate) traditional: PostingRange,
}

/// Digest-sorted identities serialized directly into a portable SwissTable.
pub(crate) struct IdentityMap {
    entries: Vec<([u8; 32], u32)>,
}

impl IdentityMap {
    pub(crate) fn new(entries: Vec<([u8; 32], u32)>) -> Self {
        debug_assert!(entries.windows(2).all(|pair| pair[0].0 < pair[1].0));
        Self { entries }
    }
}

impl Archive for IdentityMap {
    type Archived = ArchivedHashMap<[u8; 32], rkyv::Archived<u32>>;
    type Resolver = HashMapResolver;

    fn resolve(&self, resolver: Self::Resolver, out: Place<Self::Archived>) {
        ArchivedHashMap::resolve_from_len(self.entries.len(), IDENTITY_LOAD_FACTOR, resolver, out);
    }
}

impl<S> Serialize<S> for IdentityMap
where
    S: Fallible + Writer + Allocator + ?Sized,
    S::Error: Source,
{
    fn serialize(&self, serializer: &mut S) -> Result<Self::Resolver, S::Error> {
        ArchivedHashMap::<[u8; 32], rkyv::Archived<u32>>::serialize_from_iter::<
            _,
            _,
            _,
            [u8; 32],
            u32,
            S,
        >(
            self.entries
                .iter()
                .map(|(digest, runtime_key)| (digest, runtime_key)),
            IDENTITY_LOAD_FACTOR,
            serializer,
        )
    }
}

/// Complete archived dictionary data. FST values index the matching postings
/// metadata vectors; postings contain dense lexical-unit vector positions.
#[derive(rkyv::Archive, rkyv::Serialize)]
pub(crate) struct DictionaryArchive {
    pub(crate) archive_contract: [u8; 34],
    pub(crate) schema_version: u32,
    pub(crate) identity_version: u8,
    pub(crate) lexical_units: Vec<LexicalUnit>,
    pub(crate) identities: IdentityMap,
    pub(crate) chinese_fst: Vec<u8>,
    pub(crate) chinese_postings: Vec<ChinesePostings>,
    pub(crate) chinese_runtime_keys: Vec<u32>,
    pub(crate) pinyin_fst: Vec<u8>,
    pub(crate) pinyin_postings: Vec<PostingRange>,
    pub(crate) pinyin_runtime_keys: Vec<u32>,
}

impl DictionaryArchive {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        lexical_units: Vec<LexicalUnit>,
        identities: IdentityMap,
        chinese_fst: Vec<u8>,
        chinese_postings: Vec<ChinesePostings>,
        chinese_runtime_keys: Vec<u32>,
        pinyin_fst: Vec<u8>,
        pinyin_postings: Vec<PostingRange>,
        pinyin_runtime_keys: Vec<u32>,
    ) -> Self {
        Self {
            archive_contract: *ARCHIVE_CONTRACT,
            schema_version: SCHEMA_VERSION,
            identity_version: IDENTITY_VERSION,
            lexical_units,
            identities,
            chinese_fst,
            chinese_postings,
            chinese_runtime_keys,
            pinyin_fst,
            pinyin_postings,
            pinyin_runtime_keys,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn empty_contract_fixture_matches_the_consumer() {
        let archive = DictionaryArchive::new(
            Vec::new(),
            IdentityMap::new(Vec::new()),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&archive).unwrap();
        assert_eq!(bytes.len(), 112);
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "fb10c68ad48c35314b06dc816ec72ffe96d694b23d53deb8e3849c65a73200ca"
        );
    }
}
