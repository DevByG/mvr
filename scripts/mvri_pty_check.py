#!/usr/bin/env python3
# Живой pty-смоук mvri: гоняет TUI в псевдотерминале, проверяет кадры через
# SIGWINCH-перерисовку (полный кадр ratatui), применение, Esc и битый regex.
# Запуск: python3 scripts/mvri_pty_check.py [путь-к-mvri]
import fcntl
import os
import pty
import re
import select
import struct
import sys
import termios
import time

RMVI = (sys.argv[1] if len(sys.argv) > 1
       else os.path.join(os.path.dirname(__file__), "..", "target", "release", "mvri"))
ANSI = re.compile(
    r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][0-9A-B]|\x1b[=>]|\x1b[78]|\x1b\][^\x07]*\x07|\r"
)
RESIZE = ("resize", None)


def drain(master, out, seconds):
    deadline = time.time() + seconds
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.05)
        if ready:
            try:
                data = os.read(master, 65536)
            except OSError:
                return out, False
            if not data:
                return out, False
            out += data
    return out, True


def drive(cwd, script):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
    pid = os.fork()
    if pid == 0:
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        for fd in (0, 1, 2):
            os.dup2(slave, fd)
        os.close(master)
        os.close(slave)
        os.chdir(cwd)
        os.execv(RMVI, [RMVI])
    os.close(slave)

    chunks = []
    out = b""
    alive = True
    next_height = 26
    for keys, wait in script:
        if isinstance(keys, tuple):
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", next_height, 100, 0, 0))
            next_height = 25 if next_height == 26 else 26
        elif keys:
            os.write(master, keys)
        out = b""
        out, alive = drain(master, out, wait)
        chunks.append(ANSI.sub("", out.decode(errors="replace")))
    try:
        os.close(master)
    except OSError:
        pass
    deadline = time.time() + 2.0
    waited, status = 0, 0
    while time.time() < deadline:
        waited, status = os.waitpid(pid, os.WNOHANG)
        if waited != 0:
            break
        time.sleep(0.1)
    exited_cleanly = waited != 0 and os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0
    if waited == 0:
        os.kill(pid, 15)
        os.waitpid(pid, 0)
        exited_cleanly = False
    return chunks, exited_cleanly


def flat(text):
    return re.sub(r"\s+", "", text)


def files(path):
    return sorted(os.listdir(path))


def check(name, ok, detail=""):
    print(("PASS" if ok else "FAIL"), name, "" if ok else detail)
    return ok


def main():
    results = []
    base = "/tmp/mvri-pty"
    os.system(f"rm -rf {base}")
    a, b, c, d = f"{base}/a", f"{base}/b", f"{base}/c", f"{base}/d"
    for dir_ in (a, b, c, d):
        os.makedirs(dir_)
    for n in (1, 2):
        open(f"{a}/source 0{n} junk.mkv", "w").close()
    open(f"{b}/source 01 junk.mkv", "w").close()
    open(f"{c}/x.txt", "w").close()
    open(f"{d}/Mixed.TXT", "w").close()
    e = f"{base}/e"
    os.makedirs(e)
    open(f"{e}/x.txt", "w").close()

    chunks, clean = drive(a, [
        (b"source (\\d+) junk\\.mkv", 0.4),
        (RESIZE, 0.5),                       # полный кадр: только regex
        (b"\r", 0.2),
        (b"result $1.mkv", 0.4),
        (RESIZE, 0.5),                       # полный кадр: превью было/станет
        (b"\r", 0.5),                        # применить
        (RESIZE, 0.5),                       # полный кадр: сводка
        (b"\r", 0.4),                        # выход
    ])
    only_regex_frame, preview_frame, summary_frame = chunks[1], chunks[4], chunks[6]
    results.append(check("флоу: чистый выход", clean))
    results.append(check("флоу: фильтр по regex вживую",
        "source 01 junk.mkv" in only_regex_frame and "матчей: 2" in only_regex_frame,
        only_regex_frame[:300]))
    results.append(check("флоу: текст regex виден в поле",
        "│source(\\d+)junk\\.mkv" in flat(only_regex_frame),
        repr(only_regex_frame[:600])))
    results.append(check("флоу: превью было => станет",
        flat("source 01 junk.mkv => result 01.mkv") in flat(preview_frame)
        and flat("source 02 junk.mkv => result 02.mkv") in flat(preview_frame),
        repr(preview_frame)))
    results.append(check("флоу: сводка после применения",
        "переименовано: 2" in summary_frame, summary_frame[:300]))
    results.append(check("флоу: файлы переименованы",
        files(a) == ["result 01.mkv", "result 02.mkv"], str(files(a))))

    chunks, clean = drive(b, [
        (b"source (\\d+) junk\\.mkv", 0.4),
        (b"\x1b", 0.5),
    ])
    results.append(check("esc: чистый выход", clean))
    results.append(check("esc: файлы не тронуты", files(b) == ["source 01 junk.mkv"]))

    chunks, clean = drive(c, [
        (b"[", 0.4),
        (RESIZE, 0.5),
        (b"\x1b", 0.5),
    ])
    results.append(check("битый regex: чистый выход", clean))
    results.append(check("битый regex: ошибка на экране",
        "некорректный regex" in chunks[1], chunks[1][:300]))

    chunks, clean = drive(d, [
        (b"mixed\\.txt", 0.4),
        (RESIZE, 0.5),
        (b"\x14", 0.3),          # Ctrl+T: регистр с учётом
        (RESIZE, 0.5),
        (b"\x1b", 0.4),
    ])
    results.append(check("регистр: insensitive по умолчанию", "матчей: 1" in chunks[1], chunks[1][:300]))
    results.append(check("регистр: Ctrl+T включает точный",
        "матчей: 0" in chunks[3] and "с учётом" in chunks[3], chunks[3][:300]))

    chunks, clean = drive(e, [
        (b"", 0.4),
        (RESIZE, 0.5),                       # кнопка [Tt] на месте, режим без учёта
        (b"\x1b[<0;97;3M", 0.4),             # клик мышью по кнопке
        (RESIZE, 0.5),                       # режим сменился
        (b"\x1b", 0.4),
    ])
    results.append(check("кнопка [Tt]: есть на экране",
        "[Tt]" in chunks[1] and "без учёта" in chunks[1], chunks[1][:300]))
    results.append(check("кнопка [Tt]: клик переключает",
        "с учётом" in chunks[3], chunks[3][:300]))

    chunks, clean = drive(e, [
        (b"x/(y)", 0.4),
        (RESIZE, 0.5),
        (b"\x1b", 0.4),
    ])
    results.append(check("подсказка: путь в regex без -r",
        "mvri -r" in chunks[1], chunks[1][:300]))

    chunks, clean = drive(e, [
        (b"\x1b[<0;11;3M", 0.3),    # клик по полю regex (фокус и так там)
        (b"ab", 0.3),
        (b"\x1b[<0;11;6M", 0.3),    # клик по полю шаблон
        (b"xy", 0.3),
        (RESIZE, 0.5),
        (b"\x1b", 0.4),
    ])
    results.append(check("мышь: клик переключает поле",
        "│ab" in chunks[4] and "│xy" in chunks[4], chunks[4][:500]))

    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
