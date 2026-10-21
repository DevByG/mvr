use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

fn run(dir: &std::path::Path, args: &[&str], answer: &str) -> (String, String, Option<i32>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mvr"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mvr");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(answer.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait mvr");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
    )
}

fn touch(dir: &std::path::Path, name: &str) {
    fs::write(dir.join(name), b"").unwrap();
}

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn renames_after_explicit_yes() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "img_1.jpg");
    touch(dir.path(), "img_2.jpg");

    let (stdout, stderr, code) = run(dir.path(), &[r"img_(\d+)\.jpg", "photo_$1.jpg"], "y\n");

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        stdout.contains("img_1.jpg => photo_1.jpg"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("img_2.jpg => photo_2.jpg"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("переименовать? (N/y)"), "stdout: {stdout}");
    assert!(stdout.contains("переименовано: 2"), "stdout: {stdout}");
    assert_eq!(names(dir.path()), vec!["photo_1.jpg", "photo_2.jpg"]);
}

#[test]
fn keeps_files_on_default_answer() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "img_1.jpg");

    let (stdout, stderr, code) = run(dir.path(), &[r"img_(\d+)\.jpg", "x.jpg"], "\n");

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(stdout.contains("отменено"), "stdout: {stdout}");
    assert_eq!(names(dir.path()), vec!["img_1.jpg"]);
}

#[test]
fn numbers_files_with_index_pattern() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "b-report.pdf");
    touch(dir.path(), "a-report.pdf");

    let (stdout, stderr, code) = run(
        dir.path(),
        &[r"^(.)-report\.pdf$", "$i-report-$1.pdf"],
        "y\n",
    );

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        stdout.contains("a-report.pdf => 1-report-a.pdf"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("b-report.pdf => 2-report-b.pdf"),
        "stdout: {stdout}"
    );
    assert_eq!(names(dir.path()), vec!["1-report-a.pdf", "2-report-b.pdf"]);
}

#[test]
fn exits_nonzero_when_nothing_matches() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "keep.txt");

    let (stdout, stderr, code) = run(dir.path(), &[r"^nope$", "x"], "y\n");

    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert!(stdout.contains("ничего не найдено"), "stdout: {stdout}");
    assert_eq!(names(dir.path()), vec!["keep.txt"]);
}

#[test]
fn refuses_collision_before_touching_anything() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "a.txt");
    touch(dir.path(), "b.txt");

    let (stdout, stderr, code) = run(dir.path(), &[r"^(a|b)\.txt$", "same"], "y\n");

    assert_eq!(code, Some(1), "stdout: {stdout}");
    assert!(
        stderr.contains("одно и то же новое имя"),
        "stderr: {stderr}"
    );
    assert_eq!(names(dir.path()), vec!["a.txt", "b.txt"]);
}

#[test]
fn rejects_pattern_group_missing_in_regex() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "x.txt");

    let (stdout, stderr, code) = run(dir.path(), &[r"^x\.txt$", "v$2"], "y\n");

    assert_eq!(code, Some(1), "stdout: {stdout}");
    assert!(stderr.contains("$2"), "stderr: {stderr}");
    assert_eq!(names(dir.path()), vec!["x.txt"]);
}

#[test]
fn replacement_with_slash_creates_folders() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "ep 01.mkv");
    touch(dir.path(), "ep 02.mkv");

    let (stdout, stderr, code) = run(dir.path(), &[r"^ep (\d+)\.mkv$", "$1/mov.ts"], "y\n");

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        stdout.contains("ep 01.mkv => 01/mov.ts"),
        "stdout: {stdout}"
    );
    assert!(dir.path().join("01/mov.ts").is_file());
    assert!(dir.path().join("02/mov.ts").is_file());
}

#[test]
fn recursive_mode_matches_paths_by_segments() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("s01")).unwrap();
    fs::create_dir_all(dir.path().join("s02")).unwrap();
    touch(dir.path(), "s01/ep1.mkv");
    touch(dir.path(), "s01/ep2.mkv");
    touch(dir.path(), "s02/ep1.mkv");

    let (stdout, stderr, code) = run(
        dir.path(),
        &["-r", r"^s(\d+)/ep(\d+)\.mkv$", "сезон $1/эпизод $2.mkv"],
        "y\n",
    );

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        stdout.contains("s01/ep1.mkv => сезон 01/эпизод 1.mkv"),
        "stdout: {stdout}"
    );
    assert!(dir.path().join("сезон 01/эпизод 2.mkv").is_file());
    assert!(dir.path().join("сезон 02/эпизод 1.mkv").is_file());
    assert!(stdout.contains("опустеют и будут удалены: s01, s02"), "stdout: {stdout}");
    assert!(stdout.contains("удалено пустых каталогов: s01, s02"), "stdout: {stdout}");
    assert!(!dir.path().join("s01").exists());
    assert!(!dir.path().join("s02").exists());
}

#[test]
fn slashed_regex_requires_recursive_flag() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("a")).unwrap();
    touch(dir.path(), "a/x.txt");

    let (stdout, stderr, code) = run(dir.path(), &[r"^a/x\.txt$", "y.txt"], "y\n");

    assert_eq!(code, Some(2), "stdout: {stdout}");
    assert!(stderr.contains("-r"), "stderr: {stderr}");
    assert!(dir.path().join("a/x.txt").is_file());
}

#[test]
fn recursive_single_segment_pattern_stays_on_top_level() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("01/01")).unwrap();
    touch(dir.path(), "01/01/01.mp3");

    let (stdout, stderr, code) = run(dir.path(), &["-r", r"^\d+$", "folder-$0"], "y\n");

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(stdout.contains("01 => folder-01"), "stdout: {stdout}");
    assert_eq!(stdout.matches("=>").count(), 1, "stdout: {stdout}");
    assert!(dir.path().join("folder-01/01/01.mp3").is_file());
}
