use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use mvr::{apply, parse_pattern, plan, Rename};
use regex::Regex;

const USAGE: &str = "\
mvr — пакетное переименование файлов в текущем каталоге

использование: mvr <REGEX> <ШАБЛОН>

REGEX    выбирает файлы в текущем каталоге (поиск по имени файла)
ШАБЛОН   новое имя:
           $0      вся совпавшая часть имени
           $1…$9   группы захвата
           $i      порядковый номер файла (с 1, по алфавиту)
           $$      символ $

сначала печатается план «было => станет», затем вопрос;
y — переименовать, всё остальное — отмена;
в терминале клавиша читается сразу (Enter не нужен), из пайпа — строкой

примеры:
  mvr 'img_(\\d+)\\.jpg' 'photo_$1.jpg'
  mvr 'doc_.+\\.pdf' '$i.pdf'
";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let [regex_arg, pattern_arg] = args.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };

    let regex = match Regex::new(regex_arg) {
        Ok(regex) => regex,
        Err(err) => {
            eprintln!("некорректный regex: {err}");
            return ExitCode::from(2);
        }
    };

    let names = match current_dir_names() {
        Ok(names) => names,
        Err(err) => {
            eprintln!("не могу прочитать текущий каталог: {err}");
            return ExitCode::from(1);
        }
    };

    let renames = match plan(&names, &regex, &parse_pattern(pattern_arg)) {
        Ok(renames) => renames,
        Err(err) => {
            eprintln!("ошибка: {err}");
            return ExitCode::from(1);
        }
    };

    if !names.iter().any(|name| regex.is_match(name)) {
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

fn current_dir_names() -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(".")? {
        if let Ok(name) = entry?.file_name().into_string() {
            names.push(name);
        }
    }
    Ok(names)
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

fn finish(dir: &Path, renames: &[Rename]) -> ExitCode {
    let failures = apply(dir, renames);
    let done = renames.len() - failures.len();
    for (rename, err) in &failures {
        eprintln!("не удалось {} => {}: {}", rename.from, rename.to, err);
    }
    println!("переименовано: {done}");
    if failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("ошибок: {}", failures.len());
        ExitCode::from(1)
    }
}
