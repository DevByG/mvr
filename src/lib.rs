use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use regex::{Regex, RegexBuilder};

pub fn scan_paths(depth: usize) -> io::Result<Vec<String>> {
    if depth > 1 {
        walk_from(Path::new("."), depth)
    } else {
        top_level_names()
    }
}

fn top_level_names() -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(".")? {
        if let Ok(name) = entry?.file_name().into_string() {
            names.push(name);
        }
    }
    Ok(names)
}

fn walk_from(root: &Path, depth: usize) -> io::Result<Vec<String>> {
    // walk depth equals pattern depth: an exact-depth match has nothing
    // to find deeper than the pattern, so we never go there
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>, remaining: usize) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            out.push(path.clone());
            if remaining > 1 && entry.file_type()?.is_dir() {
                walk(&entry.path(), &path, out, remaining - 1)?;
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    walk(root, "", &mut paths, depth)?;
    Ok(paths)
}

#[derive(Debug, Clone)]
pub struct Rename {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Literal(String),
    Group(usize),
    Index,
}

pub struct MatchedFile {
    pub path: String,
    pub groups: Vec<Option<String>>,
}

#[derive(Debug)]
pub enum PlanError {
    InvalidRegex {
        segment: usize,
        error: String,
    },
    EmptyPatternSegment {
        segment: usize,
    },
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
    TargetInsideSource {
        from: String,
        target: String,
    },
    EmptyTarget,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::InvalidRegex { segment, error } => {
                write!(f, "invalid regex (segment {segment}): {error}")
            }
            PlanError::EmptyPatternSegment { segment } => write!(
                f,
                "empty segment {segment} in REGEX (" //" or a trailing "/")"
            ),
            PlanError::UnknownGroup { group, available } => write!(
                f,
                "the pattern refers to ${group}, but the regex only provides $0-${}",
                available - 1
            ),
            PlanError::SameTarget { target, sources } => write!(
                f,
                "the same new name \"{target}\" is produced from: {}",
                sources.join(", ")
            ),
            PlanError::TakenTarget { target } => {
                write!(
                    f,
                    "new name \"{target}\" is already taken by an existing file"
                )
            }
            PlanError::ChainedTarget { target } => write!(
                f,
                "new name \"{target}\" collides with another renamed file - split into two runs"
            ),
            PlanError::TargetInsideSource { from, target } => {
                write!(f, "cannot move \"{from}\" inside itself (\"{target}\")")
            }
            PlanError::EmptyTarget => {
                write!(f, "empty new name - check the PATTERN")
            }
        }
    }
}

pub fn parse_replacement(pattern: &str) -> Vec<Part> {
    fn flush(literal: &mut String, parts: &mut Vec<Part>) {
        if !literal.is_empty() {
            parts.push(Part::Literal(std::mem::take(literal)));
        }
    }

    let mut parts = Vec::new();
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
                flush(&mut literal, &mut parts);
                parts.push(Part::Index);
            }
            Some(d) if d.is_ascii_digit() => {
                flush(&mut literal, &mut parts);
                parts.push(Part::Group(d as usize - '0' as usize));
            }
            Some(other) => {
                literal.push('$');
                literal.push(other);
            }
            None => literal.push('$'),
        }
    }
    flush(&mut literal, &mut parts);
    parts
}

pub fn compile_matchers(pattern: &str, case_insensitive: bool) -> Result<Vec<Regex>, PlanError> {
    pattern
        .split('/')
        .enumerate()
        .map(|(segment, source)| {
            if source.is_empty() {
                Err(PlanError::EmptyPatternSegment { segment })
            } else {
                RegexBuilder::new(source)
                    .case_insensitive(case_insensitive)
                    .build()
                    .map_err(|err| PlanError::InvalidRegex {
                        segment,
                        error: err.to_string(),
                    })
            }
        })
        .collect()
}

pub fn match_paths(paths: &[String], matchers: &[Regex]) -> Vec<MatchedFile> {
    let mut matched = Vec::new();
    for path in paths {
        let components: Vec<&str> = path.split('/').collect();
        if components.len() != matchers.len() {
            continue;
        }
        let mut groups = vec![Some(path.clone())];
        let mut all_matched = true;
        for (component, matcher) in components.iter().zip(matchers) {
            let Some(captures) = matcher.captures(component) else {
                all_matched = false;
                break;
            };
            for group in 1..captures.len() {
                groups.push(captures.get(group).map(|m| m.as_str().to_string()));
            }
        }
        if all_matched {
            matched.push(MatchedFile {
                path: path.clone(),
                groups,
            });
        }
    }
    matched.sort_by(|a, b| a.path.cmp(&b.path));
    matched
}

fn expand(parts: &[Part], groups: &[Option<String>], index: usize) -> String {
    parts
        .iter()
        .map(|part| match part {
            Part::Literal(text) => text.clone(),
            Part::Group(number) => groups.get(*number).cloned().flatten().unwrap_or_default(),
            Part::Index => index.to_string(),
        })
        .collect()
}

pub struct PlanOutcome {
    pub renames: Vec<Rename>,
    pub error: Option<PlanError>,
}

pub fn plan(
    paths: &[String],
    matchers: &[Regex],
    parts: &[Part],
    exists: &dyn Fn(&str) -> bool,
) -> Result<Vec<Rename>, PlanError> {
    let PlanOutcome { renames, error } = plan_outcome(paths, matchers, parts, exists);
    error.map_or(Ok(renames), Err)
}

pub fn plan_outcome(
    paths: &[String],
    matchers: &[Regex],
    parts: &[Part],
    exists: &dyn Fn(&str) -> bool,
) -> PlanOutcome {
    let available = 1 + matchers
        .iter()
        .map(|matcher| matcher.captures_len() - 1)
        .sum::<usize>();
    if let Err(err) = validate_groups(parts, available) {
        return PlanOutcome {
            renames: Vec::new(),
            error: Some(err),
        };
    }

    let renames: Vec<Rename> = match_paths(paths, matchers)
        .iter()
        .enumerate()
        .filter_map(|(position, file)| {
            let to = expand(parts, &file.groups, position + 1);
            (to != file.path).then(|| Rename {
                from: file.path.clone(),
                to,
            })
        })
        .collect();

    match validate_targets(&renames, exists) {
        Ok(()) => PlanOutcome {
            renames,
            error: None,
        },
        Err(err) => PlanOutcome {
            renames,
            error: Some(err),
        },
    }
}

fn validate_groups(parts: &[Part], available: usize) -> Result<(), PlanError> {
    for part in parts {
        if let Part::Group(group) = part {
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

fn validate_targets(renames: &[Rename], exists: &dyn Fn(&str) -> bool) -> Result<(), PlanError> {
    let mut by_target: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for rename in renames {
        by_target
            .entry(rename.to.as_str())
            .or_default()
            .push(rename.from.as_str());
    }

    for (target, sources) in by_target {
        if target.is_empty() {
            return Err(PlanError::EmptyTarget);
        }
        if sources.len() > 1 {
            return Err(PlanError::SameTarget {
                target: target.into(),
                sources: sources.into_iter().map(String::from).collect(),
            });
        }
        let inside_source = renames
            .iter()
            .find(|rename| target.starts_with(&format!("{}/", rename.from)))
            .map(|rename| rename.from.as_str());
        if let Some(from) = inside_source {
            return Err(PlanError::TargetInsideSource {
                from: from.into(),
                target: target.into(),
            });
        }
        if exists(target) {
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

pub struct ApplyOutcome {
    pub failures: Vec<(Rename, io::Error)>,
    pub pruned_dirs: Vec<String>,
}

pub fn apply(root: &Path, renames: &[Rename]) -> ApplyOutcome {
    let mut ordered: Vec<&Rename> = renames.iter().collect();
    ordered.sort_by(|a, b| {
        a.from
            .matches('/')
            .count()
            .cmp(&b.from.matches('/').count())
            .then_with(|| a.from.cmp(&b.from))
    });

    let mut moved_dirs: Vec<Rename> = Vec::new();
    let mut failures = Vec::new();

    for rename in ordered {
        let from = translate(&rename.from, &moved_dirs);
        let to = translate(&rename.to, &moved_dirs);
        let from_path = root.join(&from);
        let to_path = root.join(&to);

        let was_dir = from_path.is_dir();
        let outcome = (|| -> io::Result<()> {
            if let Some(parent) = to_path.parent() {
                fs::create_dir_all(parent)?;
            }
            if to_path.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("target already exists: {to}"),
                ));
            }
            fs::rename(&from_path, &to_path)
        })();
        match outcome {
            Ok(()) => {
                if was_dir {
                    moved_dirs.push(rename.clone());
                }
            }
            Err(err) => failures.push((rename.clone(), err)),
        }
    }

    let pruned_dirs = prune_emptied_dirs(root, renames);
    ApplyOutcome {
        failures,
        pruned_dirs,
    }
}

pub fn planned_prunes(paths: &[String], renames: &[Rename]) -> Vec<String> {
    let froms: HashSet<&str> = renames.iter().map(|rename| rename.from.as_str()).collect();
    let mut queue: Vec<String> = renames
        .iter()
        .filter_map(|rename| parent_dir(&rename.from))
        .collect();
    queue.sort();
    queue.dedup();
    queue.sort_by_key(|dir| Reverse(dir.matches('/').count()));

    let mut pruned: Vec<String> = Vec::new();
    let mut index = 0;
    while index < queue.len() {
        let dir = queue[index].clone();
        let children_leave = paths
            .iter()
            .filter(|path| path.starts_with(&format!("{dir}/")))
            .all(|path| is_gone(path, &froms, &pruned));
        let no_target_inside = renames
            .iter()
            .all(|rename| rename.to != dir && !rename.to.starts_with(&format!("{dir}/")));
        if children_leave && no_target_inside {
            pruned.push(dir.clone());
            if let Some(parent) = parent_dir(&dir) {
                if !queue.contains(&parent) {
                    queue.push(parent);
                }
            }
        }
        index += 1;
    }
    pruned.sort();
    pruned
}

fn is_gone(path: &str, froms: &HashSet<&str>, pruned: &[String]) -> bool {
    froms.contains(path)
        || pruned
            .iter()
            .any(|dir| path.starts_with(&format!("{dir}/")))
}

fn prune_emptied_dirs(root: &Path, renames: &[Rename]) -> Vec<String> {
    let mut queue: Vec<String> = renames
        .iter()
        .flat_map(|rename| [&rename.from, &rename.to])
        .filter_map(|path| parent_dir(path))
        .collect();
    queue.sort();
    queue.dedup();
    queue.sort_by_key(|dir| Reverse(dir.matches('/').count()));

    let mut pruned = Vec::new();
    let mut index = 0;
    while index < queue.len() {
        let dir = queue[index].clone();
        let path = root.join(&dir);
        if dir_is_empty(&path) && fs::remove_dir(&path).is_ok() {
            pruned.push(dir.clone());
            if let Some(parent) = parent_dir(&dir) {
                if !queue.contains(&parent) {
                    queue.push(parent);
                }
            }
        }
        index += 1;
    }
    pruned.sort();
    pruned
}

fn dir_is_empty(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

fn parent_dir(path: &str) -> Option<String> {
    let cut = path.rfind('/')?;
    let parent = &path[..cut];
    if parent.is_empty()
        || parent.starts_with('/')
        || parent.split('/').any(|part| part == "." || part == "..")
    {
        return None;
    }
    Some(parent.to_string())
}

fn translate(path: &str, moves: &[Rename]) -> String {
    let mut current = path.to_string();
    for _ in 0..=moves.len() {
        let Some(best) = moves
            .iter()
            .filter(|m| current == m.from || current.starts_with(&format!("{}/", m.from)))
            .max_by_key(|m| m.from.len())
        else {
            return current;
        };
        current = format!("{}{}", best.to, &current[best.from.len()..]);
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts_of(pattern: &str) -> Vec<Part> {
        parse_replacement(pattern)
    }

    fn matchers_of(pattern: &str) -> Vec<Regex> {
        compile_matchers(pattern, true).unwrap()
    }

    #[test]
    fn matching_is_case_insensitive_by_default() {
        let matched = match_paths(&["Img_1.JPG".into()], &matchers_of(r"img_\d\.jpg"));
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].path, "Img_1.JPG");
    }

    #[test]
    fn inline_flag_restores_case_sensitivity() {
        let strict = compile_matchers(r"(?-i)img", true).unwrap();
        let matched = match_paths(&["img".into(), "IMG".into()], &strict);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].path, "img");
    }

    fn exists_among<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |path| names.contains(&path)
    }

    #[test]
    fn walk_is_bounded_by_depth() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        fs::write(dir.path().join("a/b/c/deep.txt"), b"").unwrap();

        let two = walk_from(dir.path(), 2).unwrap();
        assert!(two.contains(&"a/b".to_string()));
        assert!(two.iter().all(|p| p.split('/').count() <= 2));

        let four = walk_from(dir.path(), 4).unwrap();
        assert!(four.contains(&"a/b/c/deep.txt".to_string()));
    }

    #[test]
    fn replacement_splits_literals_groups_and_index() {
        assert_eq!(
            parts_of("$1_файл_$i.$0$$"),
            vec![
                Part::Group(1),
                Part::Literal("_файл_".into()),
                Part::Index,
                Part::Literal(".".into()),
                Part::Group(0),
                Part::Literal("$".into()),
            ]
        );
    }

    #[test]
    fn replacement_keeps_stray_dollars_literal() {
        assert_eq!(parts_of("a$b"), vec![Part::Literal("a$b".into())]);
        assert_eq!(parts_of("end$"), vec![Part::Literal("end$".into())]);
    }

    #[test]
    fn replacement_groups_are_single_digit() {
        assert_eq!(
            parts_of("$10"),
            vec![Part::Group(1), Part::Literal("0".into())]
        );
    }

    #[test]
    fn expand_substitutes_groups_and_index() {
        let groups = vec![Some("img-42".into()), Some("img".into()), Some("42".into())];
        assert_eq!(expand(&parts_of("$2_$1_$i"), &groups, 7), "42_img_7");
        assert_eq!(expand(&parts_of("$0"), &groups, 1), "img-42");
    }

    #[test]
    fn expand_missing_optional_group_is_empty() {
        let groups = vec![Some("ab".into()), Some("a".into()), None, Some("b".into())];
        assert_eq!(expand(&parts_of("$1[$2]$3"), &groups, 1), "a[]b");
    }

    #[test]
    fn matchers_reject_empty_segment() {
        assert!(matches!(
            compile_matchers("a//b", true).unwrap_err(),
            PlanError::EmptyPatternSegment { .. }
        ));
        assert!(matches!(
            compile_matchers("a/", true).unwrap_err(),
            PlanError::EmptyPatternSegment { .. }
        ));
    }

    #[test]
    fn depth_of_pattern_must_equal_depth_of_path() {
        let paths = vec!["01".to_string(), "01/01.mp3".to_string()];
        let matched = match_paths(&paths, &matchers_of(r"\d+"));
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].path, "01");

        let deep = match_paths(
            &["01/01/01.mp3".to_string(), "01/01.mp3".to_string()],
            &matchers_of(r"\d+/\d+\.mp3"),
        );
        assert_eq!(deep.len(), 1);
        assert_eq!(deep[0].path, "01/01.mp3");
    }

    #[test]
    fn groups_are_stitched_across_segments() {
        let matched = match_paths(
            &["s01/ep 02.mkv".to_string()],
            &matchers_of(r"s(\d+)/ep (\d+)\.mkv"),
        );
        assert_eq!(matched.len(), 1);
        assert_eq!(
            matched[0].groups,
            vec![
                Some("s01/ep 02.mkv".into()),
                Some("01".into()),
                Some("02".into()),
            ]
        );
    }

    #[test]
    fn plan_numbers_matches_in_path_order() {
        let paths = vec!["b/2.txt".to_string(), "a/1.txt".to_string()];
        let renames = plan(
            &paths,
            &matchers_of(r"(\w+)/\d\.txt"),
            &parts_of("x$i"),
            &exists_among(&[]),
        )
        .unwrap();
        assert_eq!(
            renames
                .iter()
                .map(|r| (r.from.as_str(), r.to.as_str()))
                .collect::<Vec<_>>(),
            vec![("a/1.txt", "x1"), ("b/2.txt", "x2")]
        );
    }

    #[test]
    fn plan_skips_unchanged_paths() {
        let renames = plan(
            &["keep".to_string()],
            &matchers_of("keep"),
            &parts_of("$0"),
            &exists_among(&[]),
        )
        .unwrap();
        assert!(renames.is_empty());
    }

    #[test]
    fn plan_outcome_keeps_renames_on_validation_error() {
        let outcome = plan_outcome(
            &["a.txt".to_string(), "b.txt".to_string()],
            &matchers_of(r"^(a|b)\.txt$"),
            &parts_of("same"),
            &exists_among(&[]),
        );
        assert!(matches!(outcome.error, Some(PlanError::SameTarget { .. })));
        assert_eq!(outcome.renames.len(), 2);
    }

    #[test]
    fn plan_rejects_unknown_group() {
        let err = plan(
            &["x".to_string()],
            &matchers_of("x"),
            &parts_of("$1"),
            &exists_among(&[]),
        )
        .unwrap_err();
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
        let err = plan(
            &["a.txt".to_string(), "b.txt".to_string()],
            &matchers_of(r"^(a|b)\.txt$"),
            &parts_of("same"),
            &exists_among(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::SameTarget { .. }));
    }

    #[test]
    fn plan_rejects_target_taken_by_bystander() {
        let err = plan(
            &["a".to_string(), "b".to_string()],
            &matchers_of("^a$"),
            &parts_of("b"),
            &exists_among(&["a", "b"]),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::TakenTarget { .. }));
    }

    #[test]
    fn plan_rejects_chained_target() {
        let err = plan(
            &["a".to_string(), "za".to_string()],
            &matchers_of("."),
            &parts_of("z$0"),
            &exists_among(&["a", "za"]),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::ChainedTarget { .. }));
    }

    #[test]
    fn plan_rejects_moving_directory_into_itself() {
        let err = plan(
            &["d/x".to_string()],
            &matchers_of(r"d/x"),
            &parts_of("d/x/y"),
            &exists_among(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::TargetInsideSource { .. }));
    }

    #[test]
    fn plan_allows_dots_and_folder_targets() {
        let renames = plan(
            &["01 x.mp3".to_string()],
            &matchers_of(r"^(\d+) x\.mp3$"),
            &parts_of("../sorted/$1/x.mp3"),
            &exists_among(&[]),
        )
        .unwrap();
        assert_eq!(renames[0].to, "../sorted/01/x.mp3");
    }

    #[test]
    fn apply_creates_target_folders() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ep 01.mkv"), b"").unwrap();
        let renames = vec![Rename {
            from: "ep 01.mkv".into(),
            to: "01/mov.ts".into(),
        }];
        assert!(apply(dir.path(), &renames).failures.is_empty());
        assert!(dir.path().join("01/mov.ts").is_file());
    }

    #[test]
    fn apply_renames_parent_before_children_and_rewrites_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("s01")).unwrap();
        fs::write(dir.path().join("s01/ep1.mkv"), b"").unwrap();

        let renames = vec![
            Rename {
                from: "s01/ep1.mkv".into(),
                to: "s01/x/ep1.mkv".into(),
            },
            Rename {
                from: "s01".into(),
                to: "сезон 1".into(),
            },
        ];
        assert!(apply(dir.path(), &renames).failures.is_empty());
        assert!(dir.path().join("сезон 1/x/ep1.mkv").is_file());
        assert!(!dir.path().join("s01").exists());
    }

    #[test]
    fn apply_refuses_to_overwrite_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"").unwrap();
        fs::write(dir.path().join("b.txt"), b"occupied").unwrap();
        let renames = vec![Rename {
            from: "a.txt".into(),
            to: "b.txt".into(),
        }];
        let failures = apply(dir.path(), &renames).failures;
        assert_eq!(failures.len(), 1);
        assert_eq!(
            fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "occupied"
        );
    }

    #[test]
    fn planned_prunes_predicts_emptied_source_dirs() {
        let paths: Vec<String> = vec!["s01/ep1".into(), "s01/ep2".into(), "keep.txt".into()];
        let renames = vec![
            Rename {
                from: "s01/ep1".into(),
                to: "a/ep1".into(),
            },
            Rename {
                from: "s01/ep2".into(),
                to: "a/ep2".into(),
            },
        ];
        assert_eq!(planned_prunes(&paths, &renames), vec!["s01".to_string()]);
    }

    #[test]
    fn planned_prunes_keep_dirs_with_staying_files_and_targets() {
        let paths: Vec<String> = vec!["s/ep1".into(), "s/stay".into(), "t/ep".into()];
        let staying = vec![Rename {
            from: "s/ep1".into(),
            to: "out".into(),
        }];
        assert!(planned_prunes(&paths, &staying).is_empty());

        let receiving = vec![Rename {
            from: "t/ep".into(),
            to: "s/ep".into(),
        }];
        assert_eq!(planned_prunes(&paths, &receiving), vec!["t".to_string()]);
    }

    #[test]
    fn planned_prunes_chain_upward_through_emptied_ancestors() {
        let paths: Vec<String> = vec!["a/b/f".into()];
        let renames = vec![Rename {
            from: "a/b/f".into(),
            to: "c".into(),
        }];
        assert_eq!(
            planned_prunes(&paths, &renames),
            vec!["a".to_string(), "a/b".to_string()]
        );
    }

    #[test]
    fn apply_removes_emptied_husks() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("s01")).unwrap();
        fs::write(dir.path().join("s01/ep1.mkv"), b"").unwrap();
        let renames = vec![Rename {
            from: "s01/ep1.mkv".into(),
            to: "01/mov.ts".into(),
        }];
        let outcome = apply(dir.path(), &renames);
        assert!(outcome.failures.is_empty());
        assert_eq!(outcome.pruned_dirs, vec!["s01".to_string()]);
        assert!(!dir.path().join("s01").exists());
        assert!(dir.path().join("01/mov.ts").is_file());
    }

    #[test]
    fn apply_keeps_dirs_that_still_have_content() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("s01")).unwrap();
        fs::write(dir.path().join("s01/ep1.mkv"), b"").unwrap();
        fs::write(dir.path().join("s01/ep2.mkv"), b"").unwrap();
        let renames = vec![Rename {
            from: "s01/ep1.mkv".into(),
            to: "out.mkv".into(),
        }];
        let outcome = apply(dir.path(), &renames);
        assert!(outcome.failures.is_empty());
        assert!(outcome.pruned_dirs.is_empty());
        assert!(dir.path().join("s01/ep2.mkv").is_file());
    }

    #[test]
    fn apply_reports_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let renames = vec![Rename {
            from: "нет".into(),
            to: "x".into(),
        }];
        let failures = apply(dir.path(), &renames).failures;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0.from, "нет");
    }
}
