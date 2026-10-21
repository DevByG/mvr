use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use regex::{Captures, Regex};

#[derive(Debug, Clone)]
pub struct Rename {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    Literal(String),
    Group(usize),
    Index,
}

#[derive(Debug)]
pub enum PlanError {
    UnknownGroup {
        group: usize,
        available: usize,
    },
    SameTarget {
        target: String,
        sources: Vec<String>,
    },
    TakenTarget {
        target: String,
    },
    ChainedTarget {
        target: String,
    },
    InvalidTarget {
        target: String,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::UnknownGroup { group, available } => write!(
                f,
                "шаблон ссылается на ${group}, но regex даёт группы только $0…${}",
                available - 1
            ),
            PlanError::SameTarget { target, sources } => write!(
                f,
                "одно и то же новое имя «{target}» получается из: {}",
                sources.join(", ")
            ),
            PlanError::TakenTarget { target } => write!(
                f,
                "новое имя «{target}» уже занято файлом в этом каталоге"
            ),
            PlanError::ChainedTarget { target } => write!(
                f,
                "новое имя «{target}» совпадает с другим переименовываемым файлом — разбейте на два запуска"
            ),
            PlanError::InvalidTarget { target } => write!(
                f,
                "недопустимое новое имя «{target}»: ожидается непустое имя файла без «/»"
            ),
        }
    }
}

pub fn parse_pattern(pattern: &str) -> Vec<Segment> {
    fn flush(literal: &mut String, segments: &mut Vec<Segment>) {
        if !literal.is_empty() {
            segments.push(Segment::Literal(std::mem::take(literal)));
        }
    }

    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut chars = pattern.chars();

    while let Some(c) = chars.next() {
        if c != '$' {
            literal.push(c);
            continue;
        }
        match chars.next() {
            Some('$') => literal.push('$'),
            Some('i') => {
                flush(&mut literal, &mut segments);
                segments.push(Segment::Index);
            }
            Some(d) if d.is_ascii_digit() => {
                flush(&mut literal, &mut segments);
                segments.push(Segment::Group(d as usize - '0' as usize));
            }
            Some(other) => {
                literal.push('$');
                literal.push(other);
            }
            None => literal.push('$'),
        }
    }
    flush(&mut literal, &mut segments);
    segments
}

fn expand(segments: &[Segment], captures: &Captures<'_>, index: usize) -> String {
    segments
        .iter()
        .map(|segment| match segment {
            Segment::Literal(text) => text.clone(),
            Segment::Group(number) => captures
                .get(*number)
                .map(|m| m.as_str())
                .unwrap_or_default()
                .to_string(),
            Segment::Index => index.to_string(),
        })
        .collect()
}

pub fn plan(
    dir_names: &[String],
    regex: &Regex,
    segments: &[Segment],
) -> Result<Vec<Rename>, PlanError> {
    validate_groups(segments, regex.captures_len())?;

    let mut matched: Vec<&String> = dir_names
        .iter()
        .filter(|name| regex.is_match(name))
        .collect();
    matched.sort();

    let renames: Vec<Rename> = matched
        .iter()
        .enumerate()
        .filter_map(|(position, name)| {
            let captures = regex.captures(name)?;
            let to = expand(segments, &captures, position + 1);
            (to != **name).then(|| Rename {
                from: (*name).clone(),
                to,
            })
        })
        .collect();

    validate_targets(&renames, dir_names)?;
    Ok(renames)
}

fn validate_groups(segments: &[Segment], available: usize) -> Result<(), PlanError> {
    for segment in segments {
        if let Segment::Group(group) = segment {
            if *group >= available {
                return Err(PlanError::UnknownGroup {
                    group: *group,
                    available,
                });
            }
        }
    }
    Ok(())
}

fn validate_targets(renames: &[Rename], dir_names: &[String]) -> Result<(), PlanError> {
    let mut by_target: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for rename in renames {
        by_target
            .entry(rename.to.as_str())
            .or_default()
            .push(rename.from.as_str());
    }

    for (target, sources) in by_target {
        if target.is_empty() || target.contains('/') {
            return Err(PlanError::InvalidTarget {
                target: target.into(),
            });
        }
        if sources.len() > 1 {
            return Err(PlanError::SameTarget {
                target: target.into(),
                sources: sources.into_iter().map(String::from).collect(),
            });
        }
        if dir_names.iter().any(|name| name == target) {
            let chained = renames.iter().any(|rename| rename.from == target);
            return Err(if chained {
                PlanError::ChainedTarget {
                    target: target.into(),
                }
            } else {
                PlanError::TakenTarget {
                    target: target.into(),
                }
            });
        }
    }
    Ok(())
}

pub fn apply(dir: &Path, renames: &[Rename]) -> Vec<(Rename, io::Error)> {
    renames
        .iter()
        .filter_map(|rename| {
            let from = dir.join(&rename.from);
            let to = dir.join(&rename.to);
            match fs::rename(&from, &to) {
                Ok(()) => None,
                Err(err) => Some((rename.clone(), err)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments_of(pattern: &str) -> Vec<Segment> {
        parse_pattern(pattern)
    }

    #[test]
    fn pattern_splits_literals_groups_and_index() {
        assert_eq!(
            segments_of("$1_файл_$i.$0$$"),
            vec![
                Segment::Group(1),
                Segment::Literal("_файл_".into()),
                Segment::Index,
                Segment::Literal(".".into()),
                Segment::Group(0),
                Segment::Literal("$".into()),
            ]
        );
    }

    #[test]
    fn pattern_keeps_stray_dollars_literal() {
        assert_eq!(segments_of("a$b"), vec![Segment::Literal("a$b".into())]);
        assert_eq!(segments_of("end$"), vec![Segment::Literal("end$".into())]);
    }

    #[test]
    fn pattern_groups_are_single_digit() {
        assert_eq!(
            segments_of("$10"),
            vec![Segment::Group(1), Segment::Literal("0".into())]
        );
    }

    #[test]
    fn expand_substitutes_groups_and_index() {
        let regex = Regex::new(r"(\w+)-(\d+)").unwrap();
        let captures = regex.captures("img-42").unwrap();
        assert_eq!(
            expand(&segments_of("$2_$1_$i"), &captures, 7),
            "42_img_7".to_string()
        );
        assert_eq!(
            expand(&segments_of("$0"), &captures, 1),
            "img-42".to_string()
        );
    }

    #[test]
    fn expand_missing_optional_group_is_empty() {
        let regex = Regex::new(r"(a)(x)?(b)").unwrap();
        let captures = regex.captures("ab").unwrap();
        assert_eq!(
            expand(&segments_of("$1[$2]$3"), &captures, 1),
            "a[]b".to_string()
        );
    }

    #[test]
    fn plan_numbers_matches_in_name_order() {
        let regex = Regex::new("b").unwrap();
        let names = vec!["2b.txt".to_string(), "1b.txt".to_string()];
        let renames = plan(&names, &regex, &segments_of("x$i")).unwrap();
        assert_eq!(
            renames
                .iter()
                .map(|r| (r.from.as_str(), r.to.as_str()))
                .collect::<Vec<_>>(),
            vec![("1b.txt", "x1"), ("2b.txt", "x2")]
        );
    }

    #[test]
    fn plan_skips_unchanged_names() {
        let regex = Regex::new("keep").unwrap();
        let names = vec!["keep".to_string(), "drop".to_string()];
        let renames = plan(&names, &regex, &segments_of("$0")).unwrap();
        assert!(renames.is_empty());
    }

    #[test]
    fn plan_rejects_unknown_group() {
        let regex = Regex::new("x").unwrap();
        let names = vec!["x".to_string()];
        let err = plan(&names, &regex, &segments_of("$1")).unwrap_err();
        assert!(matches!(
            err,
            PlanError::UnknownGroup {
                group: 1,
                available: 1
            }
        ));
    }

    #[test]
    fn plan_rejects_same_target_from_two_files() {
        let regex = Regex::new(r"^(a|b)\.txt$").unwrap();
        let names = vec!["a.txt".to_string(), "b.txt".to_string()];
        let err = plan(&names, &regex, &segments_of("same")).unwrap_err();
        assert!(matches!(err, PlanError::SameTarget { .. }));
    }

    #[test]
    fn plan_rejects_target_taken_by_bystander() {
        let regex = Regex::new("^a$").unwrap();
        let names = vec!["a".to_string(), "b".to_string()];
        let err = plan(&names, &regex, &segments_of("b")).unwrap_err();
        assert!(matches!(err, PlanError::TakenTarget { .. }));
    }

    #[test]
    fn plan_rejects_chained_target() {
        let regex = Regex::new(".").unwrap();
        let names = vec!["a".to_string(), "za".to_string()];
        let err = plan(&names, &regex, &segments_of("z$0")).unwrap_err();
        assert!(matches!(err, PlanError::ChainedTarget { .. }));
    }

    #[test]
    fn plan_rejects_target_with_path_inside() {
        let regex = Regex::new("^(a)$").unwrap();
        let names = vec!["a".to_string()];
        let err = plan(&names, &regex, &segments_of("sub/$1")).unwrap_err();
        assert!(matches!(err, PlanError::InvalidTarget { .. }));
    }

    #[test]
    fn apply_renames_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"").unwrap();
        fs::write(dir.path().join("b.txt"), b"").unwrap();
        let renames = vec![
            Rename {
                from: "a.txt".into(),
                to: "1.txt".into(),
            },
            Rename {
                from: "b.txt".into(),
                to: "2.txt".into(),
            },
        ];
        let failures = apply(dir.path(), &renames);
        assert!(failures.is_empty());
        assert!(dir.path().join("1.txt").exists());
        assert!(dir.path().join("2.txt").exists());
    }

    #[test]
    fn apply_reports_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let renames = vec![Rename {
            from: "нет".into(),
            to: "x".into(),
        }];
        let failures = apply(dir.path(), &renames);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0.from, "нет");
    }
}
