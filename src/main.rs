mod tui;

use std::env;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use mvr::messages::Messages;
use mvr::{apply, compile_matchers, match_paths, parse_replacement, plan, planned_prunes, Scanner};

const USAGE: &str = "\
mvr - batch rename files in the current directory

usage: mvr <REGEX> <PATTERN>
no arguments in a terminal - interactive mode (TUI) with a live preview

REGEX    matches file names in the current directory;
         \"/\" in REGEX separates path components: pattern depth equals
         path depth, the walk goes exactly as deep as the pattern -
         without \"/\" the pattern only touches the top level;
         matching is case-insensitive, (?-i) restores case sensitivity
PATTERN  the new name/path:
           $0      the whole match (the whole path for paths)
           $1..$9  capture groups (numbered across segments)
           $i      ordinal file number (from 1, natural order)
           $$      a literal $
         \"/\" in PATTERN sorts files into folders (created automatically),
         \"..\" and absolute paths are allowed - like mv;
         directories emptied by the renames are removed

the plan \"old => new\" is printed first, then a question;
y - rename, anything else - cancel;
in a terminal a single keypress is read (no Enter needed), from a pipe - a line

examples:
  mvr 'img_(\\d+)\\.jpg' 'photo_$1.jpg'
  mvr 'doc_.+\\.pdf' '$i.pdf'
  mvr 's(\\d+)/ep (\\d+)\\.mkv' 'season $1/episode $2.mkv'
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.is_empty() {
        return interactive_or_usage();
    }
    let [regex_arg, pattern_arg] = args.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    cli(regex_arg, pattern_arg, mvr::messages::detect())
}

fn interactive_or_usage() -> ExitCode {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        return match tui::run_interactive() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("interactive mode: {err}");
                ExitCode::from(1)
            }
        };
    }
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn cli(regex_arg: &str, pattern_arg: &str, msg: &'static Messages) -> ExitCode {
    let matchers = match compile_matchers(regex_arg, true) {
        Ok(matchers) => matchers,
        Err(err) => {
            eprintln!("invalid REGEX: {err}");
            return ExitCode::from(2);
        }
    };
    let paths = match Scanner::new(Path::new(".")).scan(&matchers, false) {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("cannot read the current directory: {err}");
            return ExitCode::from(1);
        }
    };

    let matched = match_paths(&paths, &matchers);
    if matched.is_empty() {
        println!("{}", msg.nothing_found);
        return ExitCode::from(1);
    }

    let exists = |path: &str| Path::new(path).exists();
    let renames = match plan(&matched, &parse_replacement(pattern_arg), &exists) {
        Ok(renames) => renames,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    if renames.is_empty() {
        println!("{}", msg.nothing_to_change);
        return ExitCode::SUCCESS;
    }

    for rename in &renames {
        println!("{} => {}", rename.from, rename.to);
    }
    let prunes = planned_prunes(&paths, &renames);
    if !prunes.is_empty() {
        println!("{}{}", msg.emptied_removed, prunes.join(", "));
    }

    match confirm(msg) {
        Ok(true) => finish(Path::new("."), &renames, msg),
        Ok(false) => {
            println!("{}", msg.cancelled);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("cannot read the answer: {err}");
            ExitCode::from(1)
        }
    }
}

fn confirm(msg: &Messages) -> io::Result<bool> {
    print!("{}", msg.rename_prompt);
    io::stdout().flush()?;
    if io::stdin().is_terminal() {
        return read_key();
    }
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim().eq_ignore_ascii_case("y"))
}

fn read_key() -> io::Result<bool> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};

    // cfmakeraw semantics: Ctrl-C arrives as a key, not a signal, and the
    // terminal is restored on every path out of this function
    ratatui::crossterm::terminal::enable_raw_mode()?;
    let answer = (|| -> io::Result<bool> {
        loop {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    return Ok(matches!(key.code, KeyCode::Char('y' | 'Y')));
                }
                _ => continue,
            }
        }
    })();
    ratatui::crossterm::terminal::disable_raw_mode()?;
    println!();
    answer
}

fn finish(dir: &Path, renames: &[mvr::Rename], msg: &Messages) -> ExitCode {
    let outcome = apply(dir, renames);
    let done = renames.len() - outcome.failures.len();
    for (rename, err) in &outcome.failures {
        eprintln!("failed {} => {}: {}", rename.from, rename.to, err);
    }
    println!("{}{done}", msg.renamed_prefix);
    if !outcome.pruned_dirs.is_empty() {
        println!("removed empty dirs: {}", outcome.pruned_dirs.join(", "));
    }
    if outcome.failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("{}{}", msg.errors_prefix, outcome.failures.len());
        ExitCode::from(1)
    }
}
