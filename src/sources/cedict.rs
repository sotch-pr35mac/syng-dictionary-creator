//! Typed adapter for CC-CEDICT V1 and V2 text records.

use super::{ParsedPronunciation, ParsedRecord, SourceReport, rejected_record};
use crate::model::{
    AlternativePronunciation, ChineseVariety, Definition, LexicalKind, MeasureWordReference,
    Qualifier, QualifierCategory, Source, Sourced, normalize_headword, normalize_text,
};
use crate::pinyin::from_numbered;
use anyhow::{Context, Result, bail};
use regex::Regex;
use std::io::BufRead;
use std::sync::LazyLock;

static ENTRY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\S+) (\S+) (\[\[.*\]\]|\[.*\]) /(.*)/$").expect("CC-CEDICT entry regex")
});
static CLASSIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(CL:([^()]*)\)").expect("classifier regex"));
static ALTERNATIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(Taiwan pr\.|also pr\.)\s*\[([^\]]+)\]").expect("pronunciation regex")
});
static PARENTHETICAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(([^()]*)\)").expect("parenthetical regex"));

/// Parses a complete CC-CEDICT stream while retaining an outcome for every entry.
pub(crate) fn parse(reader: impl BufRead, report: &mut SourceReport) -> Result<Vec<ParsedRecord>> {
    let mut records = Vec::new();
    for (line_index, line) in reader.lines().enumerate() {
        let line_number = u64::try_from(line_index + 1).expect("line number fits u64");
        let line = line.with_context(|| format!("read CC-CEDICT line {line_number}"))?;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        report.input_records += 1;
        match parse_line(&line, line_number) {
            Ok(record) => records.push(record),
            Err(error) => {
                records.push(rejected_record(
                    Source::CcCedict,
                    line_number,
                    format!("line:{line_number}"),
                    error.to_string(),
                ));
            }
        }
    }
    Ok(records)
}

/// Parses one V1 or V2 data line and scopes recognized annotations precisely.
pub(crate) fn parse_line(line: &str, line_number: u64) -> Result<ParsedRecord> {
    let captures = ENTRY
        .captures(line)
        .with_context(|| "malformed-entry-envelope".to_owned())?;
    let traditional = normalize_headword(&captures[1]).context("invalid-traditional-headword")?;
    let simplified = normalize_headword(&captures[2]).context("invalid-simplified-headword")?;
    let bracketed = &captures[3];
    let numbered = if bracketed.starts_with("[[") {
        &bracketed[2..bracketed.len() - 2]
    } else {
        &bracketed[1..bracketed.len() - 1]
    };
    let pinyin = from_numbered(numbered).context("invalid-primary-pinyin")?;
    let mut definitions = Vec::new();
    let mut alternative_pronunciations = Vec::new();
    let mut measure_words = Vec::new();

    for raw_definition in captures[4].split('/') {
        if raw_definition.is_empty() {
            continue;
        }
        if let Some(classifiers) = raw_definition.strip_prefix("CL:") {
            measure_words.extend(parse_classifiers(classifiers, Source::CcCedict)?);
            continue;
        }

        let alternatives = parse_alternatives(raw_definition)?;
        if !all_alternative_markers_matched(raw_definition) {
            bail!("malformed-pronunciation-annotation at line {line_number}");
        }
        let standalone_alternative = alternatives.len() == 1
            && ALTERNATIVE
                .find(raw_definition)
                .is_some_and(|found| found.start() == 0 && found.end() == raw_definition.len());
        if standalone_alternative {
            alternative_pronunciations.extend(alternatives);
            continue;
        }

        let mut saw_non_empty_segment = false;
        for raw_gloss in raw_definition.split(';') {
            if raw_gloss.trim().is_empty() {
                continue;
            }
            saw_non_empty_segment = true;
            let alternatives = parse_alternatives(raw_gloss)?;
            if !all_alternative_markers_matched(raw_gloss) {
                bail!("malformed-pronunciation-annotation at line {line_number}");
            }
            if !all_classifier_markers_matched(raw_gloss) {
                bail!("malformed-classifier-annotation at line {line_number}");
            }

            let mut gloss = CLASSIFIER.replace_all(raw_gloss, "").to_string();
            gloss = ALTERNATIVE.replace_all(&gloss, "").to_string();
            let (gloss, qualifiers, lexical_kinds) = extract_labels(&gloss);
            let gloss = normalize_text(&gloss);
            if gloss.is_empty() && !alternatives.is_empty() {
                alternative_pronunciations.extend(alternatives);
                continue;
            }
            if gloss.is_empty() {
                bail!("empty-definition-after-annotations at line {line_number}");
            }

            let mut scoped_measure_words = Vec::new();
            for capture in CLASSIFIER.captures_iter(raw_gloss) {
                scoped_measure_words.extend(parse_classifiers(&capture[1], Source::CcCedict)?);
            }
            let mut definition = Definition::new(gloss, Source::CcCedict);
            definition.qualifiers = qualifiers;
            definition.lexical_kinds = lexical_kinds;
            definition.alternative_pronunciations = alternatives;
            definition.measure_words = scoped_measure_words;
            definitions.push(definition);
        }
        if !saw_non_empty_segment {
            bail!("empty-definition-after-annotations at line {line_number}");
        }
    }

    Ok(ParsedRecord {
        source: Source::CcCedict,
        order: line_number,
        locator: format!("line:{line_number}"),
        simplified: Some(simplified.clone()),
        traditional: Some(traditional.clone()),
        explicit_headwords: vec![simplified, traditional],
        pronunciations: vec![ParsedPronunciation {
            pronunciation: pinyin,
            label: None,
        }],
        definitions,
        alternative_pronunciations,
        measure_words,
        rejection: None,
    })
}

/// Requires every classifier marker to start immediately inside a complete annotation match.
fn all_classifier_markers_matched(value: &str) -> bool {
    value.match_indices("CL:").all(|(marker_start, _)| {
        CLASSIFIER
            .find_iter(value)
            .any(|annotation| annotation.start() + 1 == marker_start)
    })
}

/// Requires every alternative-pronunciation marker to start a complete annotation match.
fn all_alternative_markers_matched(value: &str) -> bool {
    ["Taiwan pr.", "also pr."].into_iter().all(|marker| {
        value.match_indices(marker).all(|(marker_start, _)| {
            ALTERNATIVE
                .find_iter(value)
                .any(|annotation| annotation.start() == marker_start)
        })
    })
}

/// Parses a comma-delimited `CL:` annotation into classifier display references.
///
/// CC-CEDICT gives a Mandarin Pinyin reading, which is validated here, but a
/// classifier only receives an identity after all source records are admitted.
/// This prevents a Pinyin-specific guess when multiple current lexical units
/// share the same traditional and simplified forms.
fn parse_classifiers(value: &str, source: Source) -> Result<Vec<Sourced<MeasureWordReference>>> {
    let mut results = Vec::new();
    for raw_classifier in value.split(',') {
        let raw_classifier = raw_classifier.trim();
        let open = raw_classifier
            .rfind('[')
            .with_context(|| format!("malformed-classifier:{raw_classifier}"))?;
        let close = raw_classifier
            .strip_suffix(']')
            .map(|without_close| without_close.len())
            .with_context(|| format!("malformed-classifier:{raw_classifier}"))?;
        if close <= open + 1 {
            bail!("malformed-classifier:{raw_classifier}");
        }
        let forms = &raw_classifier[..open];
        let _pronunciation = from_numbered(&raw_classifier[open + 1..close])
            .with_context(|| format!("malformed-classifier-pinyin:{raw_classifier}"))?;
        let (traditional, simplified) = forms
            .split_once('|')
            .map_or((forms, forms), |(traditional, simplified)| {
                (traditional, simplified)
            });
        let traditional = normalize_headword(traditional)
            .with_context(|| format!("malformed-classifier-headword:{raw_classifier}"))?;
        let simplified = normalize_headword(simplified)
            .with_context(|| format!("malformed-classifier-headword:{raw_classifier}"))?;
        results.push(Sourced::one(
            MeasureWordReference {
                traditional,
                simplified,
                lexical_id: None,
                varieties: vec![ChineseVariety::Mandarin],
            },
            source,
        ));
    }
    Ok(results)
}

/// Parses one reviewed `Taiwan pr.` or `also pr.` annotation.
fn parse_alternatives(value: &str) -> Result<Vec<Sourced<AlternativePronunciation>>> {
    let mut results = Vec::new();
    for capture in ALTERNATIVE.captures_iter(value) {
        let pronunciation = from_numbered(&capture[2])
            .with_context(|| format!("malformed-alternative-pinyin: {}", &capture[2]))?;
        results.push(Sourced::one(
            AlternativePronunciation {
                pronunciation,
                label: capture[1].to_owned(),
            },
            Source::CcCedict,
        ));
    }
    Ok(results)
}

#[derive(Clone, Copy)]
enum CedictLabel {
    Qualifier(QualifierCategory, &'static str),
    LexicalKind(LexicalKind),
}

/// Removes only reviewed complete parenthetical labels, wherever they occur.
fn extract_labels(value: &str) -> (String, Vec<Sourced<Qualifier>>, Vec<Sourced<LexicalKind>>) {
    let mut remainder = String::with_capacity(value.len());
    let mut qualifiers = Vec::new();
    let mut lexical_kinds = Vec::new();
    let mut copied_through = 0;
    for capture in PARENTHETICAL.captures_iter(value) {
        let raw = &capture[1];
        let mapped = match raw {
            "idiom" => Some(CedictLabel::LexicalKind(LexicalKind::Idiom)),
            "coll." => Some(CedictLabel::Qualifier(
                QualifierCategory::Register,
                "colloquial",
            )),
            "slang" => Some(CedictLabel::Qualifier(QualifierCategory::Register, "slang")),
            "Internet slang" => Some(CedictLabel::Qualifier(
                QualifierCategory::Register,
                "Internet slang",
            )),
            "literary" => Some(CedictLabel::Qualifier(QualifierCategory::Usage, "literary")),
            "archaic" => Some(CedictLabel::Qualifier(QualifierCategory::Usage, "archaic")),
            "Tw" | "Taiwan" => Some(CedictLabel::Qualifier(QualifierCategory::Region, "Taiwan")),
            "dialect" => Some(CedictLabel::Qualifier(QualifierCategory::Region, "dialect")),
            "computing" | "medicine" | "math." | "mathematics" | "botany" | "chemistry" => {
                Some(CedictLabel::Qualifier(
                    QualifierCategory::Domain,
                    match raw {
                        "computing" => "computing",
                        "medicine" => "medicine",
                        "math." => "math.",
                        "mathematics" => "mathematics",
                        "botany" => "botany",
                        "chemistry" => "chemistry",
                        _ => unreachable!(),
                    },
                ))
            }
            _ => None,
        };
        let Some(mapped) = mapped else {
            continue;
        };
        let whole = capture.get(0).expect("whole label");
        remainder.push_str(&value[copied_through..whole.start()]);
        copied_through = whole.end();
        match mapped {
            CedictLabel::Qualifier(category, mapped_value) => qualifiers.push(Sourced::one(
                Qualifier {
                    category,
                    value: mapped_value.to_owned(),
                },
                Source::CcCedict,
            )),
            CedictLabel::LexicalKind(value) => {
                lexical_kinds.push(Sourced::one(value, Source::CcCedict));
            }
        }
    }
    remainder.push_str(&value[copied_through..]);
    (
        remainder.split_whitespace().collect::<Vec<_>>().join(" "),
        qualifiers,
        lexical_kinds,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_semicolon_glosses_and_extracts_inline_classifiers() {
        let record = parse_line(
            "經歷 经历 [jing1 li4] /experience (CL:段[duan4],次[ci4])/to experience/",
            1,
        )
        .unwrap();
        assert_eq!(record.definitions[0].gloss.value, "experience");
        assert_eq!(record.definitions[0].measure_words.len(), 2);
        let classifier = &record.definitions[0].measure_words[0].value;
        assert_eq!(classifier.traditional, "段");
        assert_eq!(classifier.simplified, "段");
        assert_eq!(classifier.lexical_id, None);
        assert_eq!(classifier.varieties, vec![ChineseVariety::Mandarin]);
        assert_eq!(record.definitions[1].gloss.value, "to experience");

        let record = parse_line("和 和 [[he2]] /and; together with/", 2).unwrap();
        assert_eq!(record.definitions.len(), 2);
        assert_eq!(record.definitions[0].gloss.value, "and");
        assert_eq!(record.definitions[1].gloss.value, "together with");
    }

    #[test]
    fn ignores_empty_semicolon_gloss_segments() {
        for (glosses, expected) in [
            ("and;", vec!["and"]),
            (";and", vec!["and"]),
            ("and;;together with", vec!["and", "together with"]),
            ("and;  ", vec!["and"]),
        ] {
            let line = format!("和 和 [[he2]] /{glosses}/");
            let record = parse_line(&line, 1).unwrap();
            let values = record
                .definitions
                .iter()
                .map(|definition| definition.gloss.value.as_str())
                .collect::<Vec<_>>();
            assert_eq!(values, expected);
        }

        assert!(parse_line("和 和 [[he2]] /;  ;/", 1).is_err());
    }

    #[test]
    fn scopes_inline_annotations_to_their_semicolon_gloss() {
        let record = parse_line(
            "保存 保存 [[bao3cun2]] /to conserve; to preserve; to keep; to store; (computing) to save (a file etc)/",
            1,
        )
        .unwrap();
        assert_eq!(record.definitions.len(), 5);
        assert!(
            record
                .definitions
                .iter()
                .take(4)
                .all(|definition| definition.qualifiers.is_empty())
        );
        assert_eq!(record.definitions[4].qualifiers.len(), 1);
        assert_eq!(record.definitions[4].qualifiers[0].value.value, "computing");

        let record = parse_line("光 光 [guang1] /light (CL:道[dao4]); ray/", 2).unwrap();
        assert_eq!(record.definitions[0].measure_words.len(), 1);
        assert!(record.definitions[1].measure_words.is_empty());
    }

    #[test]
    fn keeps_multiple_classifier_annotations_scoped_to_the_definition() {
        let record = parse_line(
            "望遠鏡 望远镜 [wang4 yuan3 jing4] /telescope (CL:架[jia4]) (CL:個|个[ge4])/",
            1,
        )
        .unwrap();
        assert_eq!(record.definitions[0].measure_words.len(), 2);
        assert!(record.measure_words.is_empty());
    }

    #[test]
    fn standalone_annotations_stay_at_lexical_unit_scope() {
        let record = parse_line(
            "丈夫 丈夫 [zhang4 fu5] /husband/CL:個|个[ge4],位[wei4]/Taiwan pr. [zhang4 fu1]/",
            1,
        )
        .unwrap();
        assert_eq!(record.measure_words.len(), 2);
        assert_eq!(record.alternative_pronunciations.len(), 1);
        assert!(record.definitions[0].measure_words.is_empty());
    }

    #[test]
    fn only_reviewed_parentheticals_become_qualifiers() {
        let record = parse_line("甲 甲 [jia3] /(coll.) first (explanation)/", 1).unwrap();
        assert_eq!(record.definitions[0].gloss.value, "first (explanation)");
        assert_eq!(record.definitions[0].qualifiers.len(), 1);

        let idiom = parse_line(
            "天作之合 天作之合 [tian1 zuo4 zhi1 he2] /a match made in heaven (idiom)/",
            2,
        )
        .unwrap();
        assert_eq!(idiom.definitions[0].gloss.value, "a match made in heaven");
        assert_eq!(
            idiom.definitions[0].lexical_kinds[0].value,
            LexicalKind::Idiom
        );
    }

    #[test]
    fn admits_reviewed_punctuation_and_syllabic_nasal_readings() {
        let idiom = parse_line(
            "一不做，二不休 一不做，二不休 [yi1 bu4 zuo4, er4 bu4 xiu1] /in for a penny, in for a pound/",
            1,
        )
        .unwrap();
        assert_eq!(
            idiom.pronunciations[0].pronunciation.numbers,
            "yi1bu4zuo4er4bu4xiu1"
        );

        let name = parse_line("亞當·斯密 亚当·斯密 [Ya4 dang1 · Si1 mi4] /Adam Smith/", 2).unwrap();
        assert_eq!(
            name.pronunciations[0].pronunciation.numbers,
            "Ya4dang1Si1mi4"
        );

        for (line_number, tone) in [(3, 1), (4, 2), (5, 4)] {
            let record =
                parse_line(&format!("呣 呣 [m{tone}] /syllabic nasal/"), line_number).unwrap();
            assert_eq!(
                record.pronunciations[0].pronunciation.numbers,
                format!("m{tone}")
            );
        }
    }

    #[test]
    fn rejects_malformed_classifier_after_valid_inline_annotation() {
        assert!(
            parse_line(
                "甲 甲 [jia3] /first (CL:個|个[ge4]) and then (CL:broken/",
                1,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_malformed_pronunciation_after_valid_inline_annotation() {
        for marker in ["Taiwan pr.", "also pr."] {
            let line = format!("甲 甲 [jia3] /first ({marker} [jia2]) and {marker} malformed/");
            assert!(parse_line(&line, 1).is_err());
        }
    }
}
