use std::env;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use mvr::{
    apply, compile_matchers, match_paths, parse_replacement, plan, planned_prunes, scan_paths,
};

const USAGE: &str = "\
mvr — пакетное переименование файлов в текущем каталоге

использование: mvr [-r] <REGEX> <ШАБЛОН>

REGEX    матчит имена файлов в текущем каталоге;
         с -r матчит пути: «/» в REGEX разделяет компоненты пути,
         глубина шаблона равна глубине пути — без «/» шаблон
         касается только верхнего уровня;
         матчинг регистронезависим, (?-i) возвращает чувствительность
ШАБЛОН   новое имя/путь:
           $0      весь матч (при -r — весь путь)
           $1…$9   группы захвата (нумерация сквозная по сегментам)
           $i      порядковый номер файла (с 1, по алфавиту)
           $$      символ $
         «/» в ШАБЛОНЕ раскладывает по папкам (создаются сами),
         «..» и абсолютные пути разрешены — как в mv;
         каталоги, опустевшие после переименований, удаляются

сначала печатается план «было => станет», затем вопрос;
y — переименовать, всё остальное — отмена;
в терминале клавиша читается сразу (Enter не нужен), из пайпа — строкой

примеры:
  mvr 'img_(\\d+)\\.jpg' 'photo_$1.jpg'
  mvr 'doc_.+\\.pdf' '$i.pdf'
  mvr -r 's(\\d+)/ep (\\d+)\\.mkv' 'сезон $1/episode $2.mkv'
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let mut recursive = false;
    let mut positional: Vec<&String> = Vec::new();
    for arg in &args {
        match arg.as_str() {
            "-r" | "--recursive" => recursive = true,
            _ => positional.push(arg),
        }
    }
    let [regex_arg, pattern_arg] = positional.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };

    let matchers = match compile_matchers(regex_arg, true) {
        Ok(matchers) => matchers,
        Err(err) => {
            eprintln!("некорректный REGEX: {err}");
            return ExitCode::from(2);
        }
    };
    if matchers.len() > 1 && !recursive {
        eprintln!("REGEX содержит «/» — для путей нужен флаг -r");
        return ExitCode::from(2);
    }

    let paths = match scan_paths(recursive, matchers.len()) {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("не могу прочитать текущий каталог: {err}");
            return ExitCode::from(1);
        }
    };

    let exists = |path: &str| Path::new(path).exists();
    let renames = match plan(&paths, &matchers, &parse_replacement(pattern_arg), &exists) {
        Ok(renames) => renames,
        Err(err) => {
            eprintln!("ошибка: {err}");
            return ExitCode::from(1);
        }
    };

    if match_paths(&paths, &matchers).is_empty() {
        println!("ничего не найдено");
        return ExitCode::from(1);
    }
    if renames.is_empty() {
        println!("изменений нет");
        return ExitCode::SUCCESS;
    }

    for rename in &renames {
        println!("{} => {}", rename.from, rename.to);
    }
    let prunes = planned_prunes(&paths, &renames);
    if !prunes.is_empty() {
        println!("опустеют и будут удалены: {}", prunes.join(", "));
    }

    match confirm() {
        Ok(true) => finish(Path::new("."), &renames),
        Ok(false) => {
            println!("отменено");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("не могу прочитать ответ: {err}");
            ExitCode::from(1)
        }
    }
}

fn confirm() -> io::Result<bool> {
    print!("переименовать? (N/y) ");
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
        // ISIG off: Ctrl-C приходит байтом, а не сигналом — сигнал посреди
        // raw-режима оставил бы терминал без эха, а байт просто отменяет.
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        // слоты VMIN/VTIME в каноническом режиме заняты чужими значениями;
        // без явной установки read может вернуть 0 байт немедленно
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
        eprintln!("не удалось {} => {}: {}", rename.from, rename.to, err);
    }
    println!("переименовано: {done}");
    if !outcome.pruned_dirs.is_empty() {
        println!(
            "удалено пустых каталогов: {}",
            outcome.pruned_dirs.join(", ")
        );
    }
    if outcome.failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("ошибок: {}", outcome.failures.len());
        ExitCode::from(1)
    }
}
