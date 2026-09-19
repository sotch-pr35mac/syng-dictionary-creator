//! WordNet-backed noun and verb inflection families.

use crate::english_search_format::normalize_text;
use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub(crate) const WORDNET_REVISION: &str = "Princeton WordNet 3.1";
pub(crate) const OVERRIDE_REVISION: &str = "1";
const OVERRIDES: &str = include_str!("../../data/english-morphology-overrides-v1.tsv");

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum EnglishPos {
    Noun,
    Verb,
}

impl EnglishPos {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Noun => 1,
            Self::Verb => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct FamilyKey {
    pub(crate) pos: EnglishPos,
    pub(crate) lemma: String,
}

#[derive(Debug)]
pub(crate) struct Morphology {
    pub(crate) families: Vec<(FamilyKey, Vec<String>)>,
    pub(crate) surfaces: BTreeMap<String, Vec<u32>>,
}

pub(crate) fn build(path: &Path, corpus: &BTreeSet<String>) -> Result<Morphology> {
    let members = read_members(path)?;
    let mut families = BTreeMap::<FamilyKey, BTreeSet<String>>::new();
    for (pos, index_name, exception_name) in [
        (EnglishPos::Noun, "dict/index.noun", "dict/noun.exc"),
        (EnglishPos::Verb, "dict/index.verb", "dict/verb.exc"),
    ] {
        let lemmas = parse_index(
            members
                .get(index_name)
                .context("WordNet archive omits index")?,
        );
        let exceptions = parse_exceptions(
            members
                .get(exception_name)
                .context("WordNet archive omits exceptions")?,
        );
        let mut by_lemma = BTreeMap::<String, Vec<String>>::new();
        for (surface, bases) in exceptions {
            for lemma in bases {
                by_lemma.entry(lemma).or_default().push(surface.clone());
            }
        }
        for lemma in lemmas {
            let key = FamilyKey {
                pos,
                lemma: lemma.clone(),
            };
            let values = families.entry(key).or_default();
            values.insert(lemma.clone());
            let exceptional = by_lemma.get(&lemma).cloned().unwrap_or_default();
            values.extend(exceptional.iter().cloned());
            match pos {
                EnglishPos::Noun => {
                    if exceptional.is_empty() {
                        values.insert(noun_plural(&lemma));
                    }
                }
                EnglishPos::Verb => add_regular_verb_forms(values, &lemma, &exceptional),
            }
        }
    }
    apply_overrides(&mut families)?;
    families.retain(|_, surfaces| surfaces.iter().any(|surface| corpus.contains(surface)));
    let mut ordered = Vec::with_capacity(families.len());
    let mut surfaces = BTreeMap::<String, Vec<u32>>::new();
    for (index, (family, forms)) in families.into_iter().enumerate() {
        let id = u32::try_from(index).context("more than u32::MAX morphology families")?;
        let forms = forms.into_iter().collect::<Vec<_>>();
        for surface in &forms {
            surfaces.entry(surface.clone()).or_default().push(id);
        }
        ordered.push((family, forms));
    }
    Ok(Morphology {
        families: ordered,
        surfaces,
    })
}

fn read_members(path: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let wanted = BTreeSet::from([
        "dict/index.noun",
        "dict/index.verb",
        "dict/noun.exc",
        "dict/verb.exc",
    ]);
    let mut result = BTreeMap::new();
    for entry in archive.entries().context("read WordNet archive")? {
        let mut entry = entry.context("read WordNet member")?;
        let name = entry.path()?.to_string_lossy().into_owned();
        if wanted.contains(name.as_str()) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            result.insert(name, bytes);
        }
    }
    Ok(result)
}

fn parse_index(bytes: &[u8]) -> BTreeSet<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.starts_with(' '))
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(one_token)
        .collect()
}

fn parse_exceptions(bytes: &[u8]) -> Vec<(String, Vec<String>)> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let surface = one_token(fields.next()?)?;
            let bases = fields.filter_map(one_token).collect::<Vec<_>>();
            (!bases.is_empty()).then_some((surface, bases))
        })
        .collect()
}

fn one_token(value: &str) -> Option<String> {
    if value.contains('_') {
        return None;
    }
    let normalized = normalize_text(value);
    (!normalized.is_empty() && !normalized.contains(' ')).then_some(normalized)
}

fn noun_plural(lemma: &str) -> String {
    if consonant_y(lemma) {
        format!("{}ies", &lemma[..lemma.len() - 1])
    } else if lemma.ends_with(['s', 'x', 'z']) || lemma.ends_with("ch") || lemma.ends_with("sh") {
        format!("{lemma}es")
    } else {
        format!("{lemma}s")
    }
}

fn add_regular_verb_forms(values: &mut BTreeSet<String>, lemma: &str, exceptions: &[String]) {
    let has_ing = exceptions.iter().any(|form| form.ends_with("ing"));
    let has_third = exceptions.iter().any(|form| form.ends_with('s'));
    let has_past = exceptions
        .iter()
        .any(|form| !form.ends_with("ing") && !form.ends_with('s'));
    if !has_third {
        values.insert(if consonant_y(lemma) {
            format!("{}ies", &lemma[..lemma.len() - 1])
        } else if lemma.ends_with(['s', 'x', 'z', 'o'])
            || lemma.ends_with("ch")
            || lemma.ends_with("sh")
        {
            format!("{lemma}es")
        } else {
            format!("{lemma}s")
        });
    }
    if !has_past {
        values.insert(if lemma.ends_with('e') {
            format!("{lemma}d")
        } else if consonant_y(lemma) {
            format!("{}ied", &lemma[..lemma.len() - 1])
        } else {
            format!("{lemma}ed")
        });
    }
    if !has_ing {
        values.insert(if let Some(stem) = lemma.strip_suffix("ie") {
            format!("{stem}ying")
        } else if lemma.ends_with('e') && !lemma.ends_with("ee") {
            format!("{}ing", &lemma[..lemma.len() - 1])
        } else {
            format!("{lemma}ing")
        });
    }
}

fn consonant_y(value: &str) -> bool {
    value.as_bytes().last() == Some(&b'y')
        && value
            .as_bytes()
            .get(value.len().saturating_sub(2))
            .is_some_and(|ch| !matches!(ch, b'a' | b'e' | b'i' | b'o' | b'u'))
}

fn apply_overrides(families: &mut BTreeMap<FamilyKey, BTreeSet<String>>) -> Result<()> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Op {
        Add,
        Remove,
    }
    let mut seen = BTreeMap::<(EnglishPos, String, String), Op>::new();
    let mut records = Vec::new();
    for (line_number, line) in OVERRIDES.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 {
            bail!("invalid morphology override line {}", line_number + 1);
        }
        let op = match fields[0] {
            "add" => Op::Add,
            "remove" => Op::Remove,
            _ => bail!("unknown morphology override operation"),
        };
        let pos = match fields[1] {
            "noun" => EnglishPos::Noun,
            "verb" => EnglishPos::Verb,
            _ => bail!("unknown morphology override POS"),
        };
        let lemma = one_token(fields[2]).context("override lemma must be one token")?;
        let surface = one_token(fields[3]).context("override surface must be one token")?;
        let identity = (pos, lemma.clone(), surface.clone());
        if let Some(previous) = seen.insert(identity, op) {
            if previous == op {
                bail!("duplicate morphology override record")
            } else {
                bail!("conflicting morphology override records")
            }
        }
        records.push((op, FamilyKey { pos, lemma }, surface));
    }
    for (_op, family, surface) in records.iter().filter(|(op, _, _)| *op == Op::Remove) {
        if let Some(values) = families.get_mut(family) {
            values.remove(surface);
        }
    }
    for (_, family, surface) in records.into_iter().filter(|(op, _, _)| *op == Op::Add) {
        families.entry(family).or_default().insert(surface);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    #[test]
    fn regular_spellings_are_conservative() {
        assert_eq!(noun_plural("city"), "cities");
        let mut forms = BTreeSet::new();
        add_regular_verb_forms(&mut forms, "dance", &[]);
        assert!(forms.contains("dances") && forms.contains("danced") && forms.contains("dancing"));
    }

    #[test]
    fn wordnet_families_retain_absent_surfaces_and_keep_pos_separate() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("wordnet.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            File::create(&path).unwrap(),
            flate2::Compression::default(),
        );
        let mut archive = tar::Builder::new(encoder);
        for (name, bytes) in [
            (
                "dict/index.noun",
                b"  header\nchild n 1 0 1 1 00000001\nsaw n 1 0 1 1 00000002\nshop n 1 0 1 1 00000003\n".as_slice(),
            ),
            (
                "dict/index.verb",
                b"  header\nrun v 1 0 1 1 00000001\nbe v 1 0 1 1 00000002\nsee v 1 0 1 1 00000003\n".as_slice(),
            ),
            ("dict/noun.exc", b"children child\n".as_slice()),
            (
                "dict/verb.exc",
                b"ran run\nrunning run\nsaw see\nwas be\n".as_slice(),
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, name, bytes).unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();

        let morphology = build(
            &path,
            &BTreeSet::from_iter(["children", "running", "is", "saw", "shop"].map(str::to_owned)),
        )
        .unwrap();
        let analyses = |surface: &str| {
            morphology.surfaces[surface]
                .iter()
                .map(|&id| &morphology.families[id as usize].0)
                .collect::<Vec<_>>()
        };
        let run = analyses("running");
        assert_eq!(run.len(), 1);
        assert!(morphology.surfaces.contains_key("ran"));
        assert!(!morphology.surfaces.contains_key("runed"));
        assert!(!morphology.surfaces.contains_key("runing"));
        for form in ["be", "am", "is", "are", "was", "were", "been", "being"] {
            assert_eq!(analyses(form).len(), 1);
        }
        let saw = analyses("saw");
        assert_eq!(saw.len(), 2);
        assert_ne!(saw[0].pos, saw[1].pos);
        assert_eq!(analyses("children")[0].lemma, "child");
        assert!(!morphology.surfaces.contains_key("childs"));
        assert!(morphology.surfaces.contains_key("shops"));
        assert!(!morphology.surfaces.contains_key("runner"));
    }
}
