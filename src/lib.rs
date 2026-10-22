pub mod messages;

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};
use rhai::{Engine, Scope};

pub struct Scanner {
    root: PathBuf,
    // every directory read once and cached until a forced scan (F5/apply):
    // pattern edits reuse the cache, the filesystem is not re-read
    listed: HashMap<String, Vec<(String, bool)>>,
}

impl Scanner {
    pub fn new(root: &Path) -> Self {
        Scanner {
            root: root.to_path_buf(),
            listed: HashMap::new(),
        }
    }

    pub fn scan(&mut self, matchers: &[Regex], force: bool) -> io::Result<Vec<String>> {
        if force {
            self.listed.clear();
        }
        if matchers.len() < 2 {
            // the flat top-level list is one cheap read_dir - keep it fresh
            return top_level_names(&self.root);
        }
        let depth = matchers.len();
        let mut paths = Vec::new();
        self.walk("", 1, depth, matchers, "", &mut paths)?;
        Ok(paths)
    }

    // the walk goes exactly as deep as the pattern, and it only enters
    // a directory whose name matches the segment for its level: with exact
    // depth matching, nothing below a non-matching directory can match
    fn walk(
        &mut self,
        dir: &str,
        level: usize,
        depth: usize,
        matchers: &[Regex],
        prefix: &str,
        out: &mut Vec<String>,
    ) -> io::Result<()> {
        let children = self.list_dir(dir)?.clone();
        for (name, is_dir) in children {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            out.push(path.clone());
            if level < depth && is_dir && matchers[level - 1].is_match(&name) {
                let child = if dir.is_empty() {
                    name.clone()
                } else {
                    format!("{dir}/{name}")
                };
                self.walk(&child, level + 1, depth, matchers, &path, out)?;
            }
        }
        Ok(())
    }

    fn list_dir(&mut self, dir: &str) -> io::Result<&Vec<(String, bool)>> {
        if !self.listed.contains_key(dir) {
            let path = if dir.is_empty() {
                self.root.clone()
            } else {
                self.root.join(dir)
            };
            let entries = fs::read_dir(path)?
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let name = entry.file_name().into_string().ok()?;
                    let is_dir = entry.file_type().ok().is_some_and(|kind| kind.is_dir());
                    Some((name, is_dir))
                })
                .collect();
            self.listed.insert(dir.to_string(), entries);
        }
        Ok(&self.listed[dir])
    }
}

fn top_level_names(root: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(root)? {
        if let Ok(name) = entry?.file_name().into_string() {
            names.push(name);
        }
    }
    Ok(names)
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
    Script(String),
}

pub struct MatchedFile {
    pub path: String,
    pub groups: Vec<Option<String>>,
}

#[derive(Debug, Clone)]
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
    ScriptError {
        message: String,
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

impl PlanError {
    pub fn target(&self) -> Option<&str> {
        match self {
            PlanError::SameTarget { target, .. }
            | PlanError::TakenTarget { target }
            | PlanError::ChainedTarget { target }
            | PlanError::TargetInsideSource { target, .. } => Some(target),
            _ => None,
        }
    }
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::ScriptError { message } => {
                write!(f, "script error: {message}")
            }
            PlanError::InvalidRegex { segment, error } => {
                write!(f, "invalid regex (segment {segment}): {error}")
            }
            PlanError::EmptyPatternSegment { segment } => write!(
                f,
                "empty segment {segment} in REGEX (\"//\" or a trailing \"/\")"
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
    let mut chars = pattern.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '$' {
            literal.push(c);
            continue;
        }
        match chars.next() {
            Some('|') => {
                // $ never appears in rhai identifiers, so the closing |$
                // cannot occur inside the script body
                let mut body = String::new();
                let mut terminated = false;
                while let Some(c) = chars.next() {
                    if c == '|' && chars.peek() == Some(&'$') {
                        chars.next();
                        terminated = true;
                        break;
                    }
                    body.push(c);
                }
                if terminated {
                    flush(&mut literal, &mut parts);
                    parts.push(Part::Script(body));
                } else {
                    literal.push_str("$|");
                    literal.push_str(&body);
                }
            }
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
    // natural order (like `ls -v`): digit runs compare numerically, so
    // "s 2" precedes "s 10"; byte compare breaks numeric ties deterministically
    matched.sort_by(|a, b| natord::compare(&a.path, &b.path).then_with(|| a.path.cmp(&b.path)));
    matched
}

fn expand(
    engine: &Engine,
    parts: &[Part],
    groups: &[Option<String>],
    index: usize,
) -> Result<String, String> {
    let mut out = String::new();
    for part in parts {
        match part {
            Part::Literal(text) => out.push_str(text),
            Part::Group(number) => out.push_str(
                groups
                    .get(*number)
                    .cloned()
                    .flatten()
                    .unwrap_or_default()
                    .as_str(),
            ),
            Part::Index => out.push_str(&index.to_string()),
            Part::Script(body) => {
                let mut scope = Scope::new();
                scope.push("i", index as i64);
                for (number, value) in groups.iter().enumerate().take(10) {
                    if let Some(value) = value {
                        scope.push(format!("g{number}"), value.clone());
                    }
                }
                // full rhai (statements + final expression): mutating string
                // methods return (), so the idiom is let/mutate/return-s
                let value = engine
                    .eval_with_scope::<rhai::Dynamic>(&mut scope, body)
                    .map_err(|err| format!("{err}"))?;
                // a bare `i` or `len(g1)` is an int - stringify it instead
                // of forcing every script into string concatenation
                let text = if value.is_string() {
                    value.into_string().unwrap()
                } else if value.is_int() {
                    value.as_int().unwrap().to_string()
                } else {
                    return Err(format!(
                        "script must return a string, got {}",
                        value.type_name()
                    ));
                };
                out.push_str(&text);
            }
        }
    }
    Ok(out)
}

pub struct PlanOutcome {
    pub renames: Vec<Rename>,
    pub error: Option<PlanError>,
    // every target the validation found at fault - the single source of
    // conflict knowledge for both the CLI error and the TUI coloring
    pub conflicting: Vec<String>,
}

pub fn plan(
    matched: &[MatchedFile],
    parts: &[Part],
    exists: &dyn Fn(&str) -> bool,
) -> Result<Vec<Rename>, PlanError> {
    let PlanOutcome { renames, error, .. } = plan_outcome(matched, parts, exists);
    error.map_or(Ok(renames), Err)
}

pub fn plan_outcome(
    matched: &[MatchedFile],
    parts: &[Part],
    exists: &dyn Fn(&str) -> bool,
) -> PlanOutcome {
    let available = matched.first().map_or(1, |file| file.groups.len());
    if let Err(err) = validate_groups(parts, available) {
        return PlanOutcome {
            renames: Vec::new(),
            error: Some(err),
            conflicting: Vec::new(),
        };
    }

    let mut engine = Engine::new();
    // statements are allowed, so a runaway loop must die on its own:
    // the op budget turns it into a script error instead of a hang
    engine.set_max_operations(1_000_000);
    let mut renames: Vec<Rename> = Vec::new();
    for (position, file) in matched.iter().enumerate() {
        let to = match expand(&engine, parts, &file.groups, position + 1) {
            Ok(to) => to,
            Err(message) => {
                return PlanOutcome {
                    renames: Vec::new(),
                    error: Some(PlanError::ScriptError { message }),
                    conflicting: Vec::new(),
                };
            }
        };
        if to != file.path {
            renames.push(Rename {
                from: file.path.clone(),
                to,
            });
        }
    }

    let errors = validate_targets(&renames, exists);
    PlanOutcome {
        error: errors.first().cloned(),
        conflicting: errors
            .iter()
            .filter_map(|err| err.target().map(str::to_string))
            .collect(),
        renames,
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

fn validate_targets(renames: &[Rename], exists: &dyn Fn(&str) -> bool) -> Vec<PlanError> {
    let mut by_target: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for rename in renames {
        by_target
            .entry(rename.to.as_str())
            .or_default()
            .push(rename.from.as_str());
    }

    let mut errors = Vec::new();
    for (target, sources) in by_target {
        if target.is_empty() {
            errors.push(PlanError::EmptyTarget);
        } else if sources.len() > 1 {
            errors.push(PlanError::SameTarget {
                target: target.into(),
                sources: sources.into_iter().map(String::from).collect(),
            });
        } else if target.contains('/') {
            // only a path-like target can live inside a source; the flat
            // common case skips the prefix scan entirely
            if let Some(from) = renames
                .iter()
                .find(|rename| target.starts_with(&format!("{}/", rename.from)))
                .map(|rename| rename.from.as_str())
            {
                errors.push(PlanError::TargetInsideSource {
                    from: from.into(),
                    target: target.into(),
                });
                continue;
            }
            if exists(target) {
                errors.push(taken_or_chained(target, renames));
            }
        } else if exists(target) {
            errors.push(taken_or_chained(target, renames));
        }
    }
    errors
}

fn taken_or_chained(target: &str, renames: &[Rename]) -> PlanError {
    let chained = renames.iter().any(|rename| rename.from == target);
    if chained {
        PlanError::ChainedTarget {
            target: target.into(),
        }
    } else {
        PlanError::TakenTarget {
            target: target.into(),
        }
    }
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

    fn matched_of(paths: &[&str], pattern: &str) -> Vec<MatchedFile> {
        let owned: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        match_paths(&owned, &matchers_of(pattern))
    }

    fn exists_among<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |path| names.contains(&path)
    }

    #[test]
    fn scanner_walks_only_matching_subtrees_to_pattern_depth() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("01/sub")).unwrap();
        fs::create_dir_all(dir.path().join("ab/sub")).unwrap();
        fs::write(dir.path().join("01/x.txt"), b"").unwrap();
        fs::write(dir.path().join("01/sub/deep.txt"), b"").unwrap();
        fs::write(dir.path().join("ab/sub/x.txt"), b"").unwrap();

        let mut scanner = Scanner::new(dir.path());
        let paths = scanner.scan(&matchers_of(r"\d+/\w+\.txt"), false).unwrap();
        assert!(paths.contains(&"01/x.txt".to_string()));
        assert!(paths.contains(&"01/sub".to_string()));
        assert!(!paths.iter().any(|p| p.starts_with("ab/")));

        let deep = scanner
            .scan(&matchers_of(r"\d+/sub/\w+\.txt"), false)
            .unwrap();
        assert!(deep.contains(&"01/sub/deep.txt".to_string()));
    }

    #[test]
    fn scanner_cache_stays_stale_until_forced() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("01")).unwrap();
        fs::write(dir.path().join("01/a.txt"), b"").unwrap();

        let mut scanner = Scanner::new(dir.path());
        let matchers = matchers_of(r"01/\w+\.txt");
        let first = scanner.scan(&matchers, false).unwrap();
        assert!(first.contains(&"01/a.txt".to_string()));

        fs::write(dir.path().join("01/b.txt"), b"").unwrap();
        let cached = scanner.scan(&matchers, false).unwrap();
        assert!(!cached.contains(&"01/b.txt".to_string()));

        let forced = scanner.scan(&matchers, true).unwrap();
        assert!(forced.contains(&"01/b.txt".to_string()));
    }

    #[test]
    fn language_detection_parses_locale_forms() {
        use crate::messages::{lang_of, EN, RU};

        assert!(std::ptr::eq(lang_of("ru_RU.UTF-8"), &RU));
        assert!(std::ptr::eq(lang_of("RU"), &RU));
        assert!(std::ptr::eq(lang_of("ru"), &RU));
        assert!(std::ptr::eq(lang_of("en_US.UTF-8"), &EN));
        assert!(std::ptr::eq(lang_of("C"), &EN));
        assert!(std::ptr::eq(lang_of(""), &EN));
        assert!(std::ptr::eq(lang_of("ru:en"), &RU));
    }

    #[test]
    fn natural_order_numbers_inside_names() {
        let paths: Vec<String> = ["s 101", "s 2", "s 11", "s 1", "s 10"]
            .iter()
            .map(|p| p.to_string())
            .collect();
        let matched = match_paths(&paths, &matchers_of("s"));
        let names: Vec<&str> = matched.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, vec!["s 1", "s 2", "s 10", "s 11", "s 101"]);

        let renames = plan(&matched, &parts_of("e$i"), &exists_among(&[])).unwrap();
        let targets: Vec<&str> = renames.iter().map(|r| r.to.as_str()).collect();
        assert_eq!(targets, vec!["e1", "e2", "e3", "e4", "e5"]);
    }

    #[test]
    fn natural_order_is_deterministic_for_numeric_ties() {
        let one: Vec<String> = ["s 01", "s 1", "s 1a"]
            .iter()
            .map(|p| p.to_string())
            .collect();
        let two: Vec<String> = ["s 1a", "s 1", "s 01"]
            .iter()
            .map(|p| p.to_string())
            .collect();
        let first: Vec<String> = match_paths(&one, &matchers_of("s"))
            .iter()
            .map(|f| f.path.clone())
            .collect();
        let second: Vec<String> = match_paths(&two, &matchers_of("s"))
            .iter()
            .map(|f| f.path.clone())
            .collect();
        assert_eq!(first, second);
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
    fn script_part_parses_with_pipe_dollars() {
        assert_eq!(
            parts_of("$|g1|$.txt"),
            vec![Part::Script("g1".into()), Part::Literal(".txt".into())]
        );
        // unterminated: degrades to literal like every stray dollar
        assert_eq!(parts_of("$|g1"), vec![Part::Literal("$|g1".into())]);
    }

    #[test]
    fn script_part_pads_numbers() {
        let renames = plan(
            &matched_of(&["s 1.txt", "s 2.txt", "s 10.txt"], r"s (\d+)\.txt"),
            &parts_of("e$|if len(g1) < 2 { \"0\" + g1 } else { g1 }|$.txt"),
            &exists_among(&[]),
        )
        .unwrap();
        let targets: Vec<&str> = renames.iter().map(|r| r.to.as_str()).collect();
        assert_eq!(targets, vec!["e01.txt", "e02.txt", "e10.txt"]);
    }

    #[test]
    fn script_part_sees_the_index() {
        let renames = plan(
            &matched_of(&["a.txt", "b.txt"], r"."),
            &parts_of("n$|i|$"),
            &exists_among(&[]),
        )
        .unwrap();
        let targets: Vec<&str> = renames.iter().map(|r| r.to.as_str()).collect();
        assert_eq!(targets, vec!["n1", "n2"]);
    }

    #[test]
    fn script_part_supports_statements_and_mutating_methods() {
        let renames = plan(
            &matched_of(&["a-b.txt"], r"(a-b)"),
            &parts_of("$|let s = g0; s.replace(\"a\", \"X\"); s|$"),
            &exists_among(&[]),
        )
        .unwrap();
        assert_eq!(renames[0].to, "X-b.txt");
    }

    #[test]
    fn script_part_infinite_loop_hits_the_op_budget() {
        let err = plan(
            &matched_of(&["a.txt"], r"(a)"),
            &parts_of("$|loop {}|$"),
            &exists_among(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::ScriptError { .. }));
    }

    #[test]
    fn script_part_error_is_a_plan_error() {
        let err = plan(
            &matched_of(&["a.txt"], r"(a)"),
            &parts_of("$|missing_variable|$"),
            &exists_among(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::ScriptError { .. }));
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
        let engine = Engine::new();
        assert_eq!(
            expand(&engine, &parts_of("$2_$1_$i"), &groups, 7).unwrap(),
            "42_img_7"
        );
        assert_eq!(
            expand(&engine, &parts_of("$0"), &groups, 1).unwrap(),
            "img-42"
        );
    }

    #[test]
    fn expand_missing_optional_group_is_empty() {
        let groups = vec![Some("ab".into()), Some("a".into()), None, Some("b".into())];
        let engine = Engine::new();
        assert_eq!(
            expand(&engine, &parts_of("$1[$2]$3"), &groups, 1).unwrap(),
            "a[]b"
        );
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
        let renames = plan(
            &matched_of(&["b/2.txt", "a/1.txt"], r"(\w+)/\d\.txt"),
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
            &matched_of(&["keep"], "keep"),
            &parts_of("$0"),
            &exists_among(&[]),
        )
        .unwrap();
        assert!(renames.is_empty());
    }

    #[test]
    fn plan_outcome_keeps_renames_on_validation_error() {
        let outcome = plan_outcome(
            &matched_of(&["a.txt", "b.txt"], r"^(a|b)\.txt$"),
            &parts_of("same"),
            &exists_among(&[]),
        );
        assert!(matches!(outcome.error, Some(PlanError::SameTarget { .. })));
        assert_eq!(outcome.renames.len(), 2);
    }

    #[test]
    fn plan_outcome_collects_all_conflicting_targets() {
        let mut matched = matched_of(&["a.txt", "b.txt", "c.txt"], r"^(a|b|c)\.txt$");
        // третья цель - занятое имя на диске, первые две - дубликаты
        matched.truncate(2);
        let outcome = plan_outcome(
            &matched_of(&["a.txt", "b.txt"], r"^(a|b)\.txt$"),
            &parts_of("same"),
            &exists_among(&[]),
        );
        assert_eq!(outcome.conflicting, vec!["same".to_string()]);

        let taken = plan_outcome(
            &matched_of(&["a.txt"], r"^a\.txt$"),
            &parts_of("taken"),
            &exists_among(&["a.txt", "taken"]),
        );
        assert_eq!(taken.conflicting, vec!["taken".to_string()]);
    }

    #[test]
    fn plan_rejects_unknown_group() {
        let err = plan(
            &matched_of(&["x"], "x"),
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
            &matched_of(&["a.txt", "b.txt"], r"^(a|b)\.txt$"),
            &parts_of("same"),
            &exists_among(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::SameTarget { .. }));
    }

    #[test]
    fn plan_rejects_target_taken_by_bystander() {
        let err = plan(
            &matched_of(&["a", "b"], "^a$"),
            &parts_of("b"),
            &exists_among(&["a", "b"]),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::TakenTarget { .. }));
    }

    #[test]
    fn plan_rejects_chained_target() {
        let err = plan(
            &matched_of(&["a", "za"], "."),
            &parts_of("z$0"),
            &exists_among(&["a", "za"]),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::ChainedTarget { .. }));
    }

    #[test]
    fn plan_rejects_moving_directory_into_itself() {
        let err = plan(
            &matched_of(&["d/x"], r"d/x"),
            &parts_of("d/x/y"),
            &exists_among(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::TargetInsideSource { .. }));
    }

    #[test]
    fn plan_allows_dots_and_folder_targets() {
        let renames = plan(
            &matched_of(&["01 x.mp3"], r"^(\d+) x\.mp3$"),
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
