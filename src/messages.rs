// Happy-path UI strings. PlanError texts and USAGE stay English on purpose:
// errors are developer-facing, this table is the user-facing chrome only.
pub struct Messages {
    pub rename_prompt: &'static str,
    pub nothing_found: &'static str,
    pub nothing_to_change: &'static str,
    pub cancelled: &'static str,
    pub renamed_prefix: &'static str,
    pub emptied_removed: &'static str,
    pub removed_empty_dirs: &'static str,
    pub errors_prefix: &'static str,
    pub type_regex_hint: &'static str,
    pub files: &'static str,
    pub matches: &'static str,
    pub case_ignored: &'static str,
    pub case_exact: &'static str,
    pub pattern_label: &'static str,
    pub case_label: &'static str,
    pub more_suffix: &'static str,
    pub any_key_quit: &'static str,
    pub help: &'static str,
}

pub static EN: Messages = Messages {
    rename_prompt: "rename? (N/y) ",
    nothing_found: "nothing found",
    nothing_to_change: "nothing to change",
    cancelled: "cancelled",
    renamed_prefix: "renamed: ",
    emptied_removed: "will be emptied and removed: ",
    removed_empty_dirs: "removed empty dirs: ",
    errors_prefix: "; errors: ",
    type_regex_hint: " type a regex - matches appear here live",
    files: "files",
    matches: "matches",
    case_ignored: "case: ignored",
    case_exact: "case: exact",
    pattern_label: " pattern ",
    case_label: " case ",
    more_suffix: " more",
    any_key_quit: " any key to quit",
    help:
        " Tab fields · Enter next/apply · Ctrl+T case · F5 rescan · ↑↓ PgUp/PgDn scroll · Esc quit",
};

pub static RU: Messages = Messages {
    rename_prompt: "переименовать? (N/y) ",
    nothing_found: "ничего не найдено",
    nothing_to_change: "изменений нет",
    cancelled: "отменено",
    renamed_prefix: "переименовано: ",
    emptied_removed: "опустеют и будут удалены: ",
    removed_empty_dirs: "удалено пустых каталогов: ",
    errors_prefix: "; ошибок: ",
    type_regex_hint: " введите regex — совпадения появятся здесь",
    files: "файлов",
    matches: "матчей",
    case_ignored: "регистр: без учёта",
    case_exact: "регистр: с учётом",
    pattern_label: " шаблон ",
    case_label: " регистр ",
    more_suffix: " ещё",
    any_key_quit: " любая клавиша — выход",
    help: " Tab поля · Enter далее/применить · Ctrl+T регистр · F5 rescan · ↑↓ PgUp/PgDn прокрутка · Esc выход",
};

pub fn detect() -> &'static Messages {
    // an empty LC_* variable means "inherit from LANG" in the glibc
    // convention - it must not shadow a real value further down the chain
    if let Ok(forced) = std::env::var("MVR_LANG") {
        if !forced.is_empty() {
            return lang_of(&forced);
        }
    }
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                return lang_of(&value);
            }
        }
    }
    &EN
}

pub fn lang_of(value: &str) -> &'static Messages {
    let first = value.split(':').next().unwrap_or("");
    let language = first.split(['_', '.', '@']).next().unwrap_or("");
    if language.eq_ignore_ascii_case("ru") {
        &RU
    } else {
        &EN
    }
}
