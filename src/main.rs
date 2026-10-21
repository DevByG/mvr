mod tui;

use std::env;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use mvr::{
    apply, compile_matchers, match_paths, parse_replacement, plan, planned_prunes, scan_paths,
};

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
           $i      ordinal file number (from 1, alphabetically)
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
    cli(regex_arg, pattern_arg)
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

fn cli(regex_arg: &str, pattern_arg: &str) -> ExitCode {
    let matchers = match compile_matchers(regex_arg, true) {
        Ok(matchers) => matchers,
        Err(err) => {
            eprintln!("invalid REGEX: {err}");
            return ExitCode::from(2);
        }
    };
    let paths = match scan_paths(matchers.len()) {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("cannot read the current directory: {err}");
            return ExitCode::from(1);
        }
    };

    let exists = |path: &str| Path::new(path).exists();
    let renames = match plan(&paths, &matchers, &parse_replacement(pattern_arg), &exists) {
        Ok(renames) => renames,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };

    if match_paths(&paths, &matchers).is_empty() {
        println!("nothing found");
        return ExitCode::from(1);
    }
    if renames.is_empty() {
        println!("nothing to change");
        return ExitCode::SUCCESS;
    }

    for rename in &renames {
        println!("{} => {}", rename.from, rename.to);
    }
    let prunes = planned_prunes(&paths, &renames);
    if !prunes.is_empty() {
        println!("will be emptied and removed: {}", prunes.join(", "));
    }

    match confirm() {
        Ok(true) => finish(Path::new("."), &renames),
        Ok(false) => {
            println!("cancelled");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("cannot read the answer: {err}");
            ExitCode::from(1)
        }
    }
}

fn confirm() -> io::Result<bool> {
    print!("rename? (N/y) ");
    io::stdout().flush()?;
    if io::stdin().is_terminal() {
        return read_key();
    }
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim().eq_ignore_ascii_case("y"))
}

struct RestoreTermios(libc::termios);

impl Drop for RestoreTermios {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0) };
    }
}

fn read_key() -> io::Result<bool> {
    unsafe {
        let mut original: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &mut original) != 0 {
            return Err(io::Error::last_os_error());
        }
        let _restore = RestoreTermios(original);

        let mut raw = original;
        // ISIG off: Ctrl-C arrives as a byte, not a signal - a signal
        // mid-raw-mode would leave the terminal without echo; the byte just cancels.
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        // VMIN/VTIME slots hold foreign values in canonical mode;
        // without setting them explicitly read may return 0 bytes immediately
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut key = [0u8];
        let pressed = io::stdin().read(&mut key)?;
        println!();
        Ok(pressed == 1 && key[0].eq_ignore_ascii_case(&b'y'))
    }
}

fn finish(dir: &Path, renames: &[mvr::Rename]) -> ExitCode {
    let outcome = apply(dir, renames);
    let done = renames.len() - outcome.failures.len();
    for (rename, err) in &outcome.failures {
        eprintln!("failed {} => {}: {}", rename.from, rename.to, err);
    }
    println!("renamed: {done}");
    if !outcome.pruned_dirs.is_empty() {
        println!("removed empty dirs: {}", outcome.pruned_dirs.join(", "));
    }
    if outcome.failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("errors: {}", outcome.failures.len());
        ExitCode::from(1)
    }
}
