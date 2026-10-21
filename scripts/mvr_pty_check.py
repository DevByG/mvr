#!/usr/bin/env python3
# Live pty smoke test for the mvr TUI (bare invocation): drives the TUI in
# a pseudo-terminal, verifies frames via SIGWINCH full redraws, apply, Esc
# and a broken regex. Usage: python3 scripts/mvr_pty_check.py [path-to-mvr]
import fcntl
import os
import pty
import re
import select
import struct
import sys
import termios
import time

MVR = (sys.argv[1] if len(sys.argv) > 1
       else os.path.join(os.path.dirname(__file__), "..", "target", "release", "mvr"))
ANSI = re.compile(
    r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][0-9A-B]|\x1b[=>]|\x1b[78]|\x1b\][^\x07]*\x07|\r"
)
RESIZE = ("resize", None)
TOUCH = ("touch", None)


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


def drive(cwd, script, args=()):
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
        os.execv(MVR, [MVR, *args])
    os.close(slave)

    chunks = []
    out = b""
    alive = True
    next_height = 26
    for keys, wait in script:
        if isinstance(keys, tuple):
            if keys[0] == "touch":
                open(keys[1], "w").close()
            else:
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
    base = "/tmp/mvr-pty"
    os.system(f"rm -rf {base}")
    a, b, c, d = f"{base}/a", f"{base}/b", f"{base}/c", f"{base}/d"
    for dir_ in (a, b, c, d):
        os.makedirs(dir_)
    for n in (1, 2):
        open(f"{a}/source 0{n} junk.mkv", "w").close()
    open(f"{b}/source 01 junk.mkv", "w").close()
    open(f"{c}/x.txt", "w").close()
    open(f"{d}/Mixed.TXT", "w").close()
    e, f_, g = f"{base}/e", f"{base}/f", f"{base}/g"
    for dir_ in (e, f_, g):
        os.makedirs(dir_)
    open(f"{e}/x.txt", "w").close()
    open(f"{f_}/a.txt", "w").close()
    open(f"{f_}/b.txt", "w").close()
    open(f"{g}/new1.txt", "w").close()
    h = f"{base}/h"
    os.makedirs(f"{h}/01/sub/deep")
    open(f"{h}/01/x.txt", "w").close()
    open(f"{h}/01/sub/deep/x.txt", "w").close()

    chunks, clean = drive(a, [
        (b"source (\\d+) junk\\.mkv", 0.4),
        (RESIZE, 0.5),                       # full frame: regex only
        (b"\r", 0.2),
        (b"result $1.mkv", 0.4),
        (RESIZE, 0.5),                       # full frame: old/new preview
        (b"\r", 0.5),                        # apply
        (RESIZE, 0.5),                       # full frame: summary
        (b"\r", 0.4),                        # quit
    ])
    only_regex_frame, preview_frame, summary_frame = chunks[1], chunks[4], chunks[6]
    results.append(check("flow: clean exit", clean))
    results.append(check("flow: live regex filter",
        "source 01 junk.mkv" in only_regex_frame and "matches: 2" in only_regex_frame,
        only_regex_frame[:300]))
    results.append(check("flow: regex text visible in field",
        "│source(\\d+)junk\\.mkv" in flat(only_regex_frame),
        repr(only_regex_frame[:600])))
    results.append(check("flow: old => new preview",
        flat("source 01 junk.mkv => result 01.mkv") in flat(preview_frame)
        and flat("source 02 junk.mkv => result 02.mkv") in flat(preview_frame),
        repr(preview_frame)))
    results.append(check("flow: summary after apply",
        "renamed: 2" in summary_frame, summary_frame[:300]))
    results.append(check("flow: files renamed",
        files(a) == ["result 01.mkv", "result 02.mkv"], str(files(a))))

    chunks, clean = drive(b, [
        (b"source (\\d+) junk\\.mkv", 0.4),
        (b"\x1b", 0.5),
    ])
    results.append(check("esc: clean exit", clean))
    results.append(check("esc: files untouched", files(b) == ["source 01 junk.mkv"]))

    chunks, clean = drive(c, [
        (b"[", 0.4),
        (RESIZE, 0.5),
        (b"\x1b", 0.5),
    ])
    results.append(check("broken regex: clean exit", clean))
    results.append(check("broken regex: error on screen",
        "invalid regex" in chunks[1], chunks[1][:300]))

    chunks, clean = drive(d, [
        (b"mixed\\.txt", 0.4),
        (RESIZE, 0.5),
        (b"\x14", 0.3),          # Ctrl+T: exact case
        (RESIZE, 0.5),
        (b"\x1b", 0.4),
    ])
    results.append(check("case: insensitive by default", "matches: 1" in chunks[1], chunks[1][:300]))
    results.append(check("case: Ctrl+T enables exact",
        "matches: 0" in chunks[3] and "case: exact" in chunks[3], chunks[3][:300]))

    chunks, clean = drive(e, [
        (b"", 0.4),
        (RESIZE, 0.5),                       # [Tt] button present, mode ignored
        (b"\x1b[<0;97;3M", 0.4),             # mouse click on the button
        (RESIZE, 0.5),                       # mode toggled
        (b"\x1b", 0.4),
    ])
    results.append(check("button [Tt]: visible",
        "[Tt]" in chunks[1] and "case: ignored" in chunks[1], chunks[1][:300]))
    results.append(check("button [Tt]: click toggles",
        "case: exact" in chunks[3], chunks[3][:300]))

    i = f"{base}/i"
    os.makedirs(f"{i}/a")
    open(f"{i}/a/b.txt", "w").close()
    chunks, clean = drive(i, [
        (b"a/b", 0.4),
        (RESIZE, 0.5),
        (b"\x1b", 0.4),
    ])
    results.append(check("slash regex works with no flags",
        "matches: 1" in chunks[1] and "a/b.txt" in chunks[1], chunks[1][:300]))

    chunks, clean = drive(e, [
        (b"\x1b[<0;11;3M", 0.3),    # click on the regex field
        (b"ab", 0.3),
        (b"\x1b[<0;11;6M", 0.3),    # click on the pattern field
        (b"xy", 0.3),
        (RESIZE, 0.5),
        (b"\x1b", 0.4),
    ])
    results.append(check("mouse: click switches field",
        "│ab" in chunks[4] and "│xy" in chunks[4], chunks[4][:500]))

    chunks, clean = drive(f_, [
        (b"^(a|b)\\.txt$", 0.3),
        (b"\r", 0.2),
        (b"same", 0.4),
        (RESIZE, 0.5),
        (b"\x1b", 0.4),
    ])
    results.append(check("collision: preview stays with error",
        "a.txt=>same" in flat(chunks[3]) and "b.txt=>same" in flat(chunks[3])
        and "the same new name" in chunks[3], chunks[3][:400]))

    chunks, clean = drive(g, [
        (b"new", 0.3),
        (RESIZE, 0.4),
        (("touch", f"{g}/new2.txt"), 0.1),
        (b"", 2.6),
        (RESIZE, 0.5),
        (b"\x1b[15~", 0.3),
        (RESIZE, 0.5),
        (b"\x1b", 0.4),
    ])
    results.append(check("no tick: fs changes wait for F5",
        "matches: 1" in chunks[1] and "matches: 1" in chunks[4], chunks[4][:300]))
    results.append(check("F5 picks up new file",
        "matches: 2" in chunks[6], chunks[6][:300]))

    chunks, clean = drive(h, [
        (b"(\\d+)/x\\.txt", 0.4),
        (RESIZE, 0.5),
        (b"\r", 0.2),
        (b"z-$1.txt", 0.4),
        (RESIZE, 0.5),
        (b"\r", 0.4),
        (b"\r", 0.4),
    ])
    results.append(check("walk depth = pattern depth, no flags",
        "01/x.txt=>z-01.txt" in flat(chunks[4])
        and "deep" not in chunks[1]
        and "renamed: 1" in chunks[5], chunks[4][:400]))

    chunks, clean = drive(e, [
        (b"xy", 0.3),
        (b"\x1b[D", 0.2),          # left arrow: cursor before y
        (b"\x1b[3~", 0.3),         # Delete: remove y
        (RESIZE, 0.5),
        (b"\x1b", 0.4),
    ])
    results.append(check("Delete: removes char under cursor",
        "│x│" in flat(chunks[3]) and "│xy│" not in flat(chunks[3]),
        repr(chunks[3][:400])))

    j = f"{base}/j"
    os.makedirs(f"{j}/01")
    open(f"{j}/01/a.txt", "w").close()
    chunks, clean = drive(j, [
        (b"01/\\w+\\.txt", 0.4),
        (RESIZE, 0.5),                       # cached walk: files: 2, matches: 1
        (("touch", f"{j}/01/b.txt"), 0.1),
        (b"", 1.0),                          # no tick at depth >= 2 - cache must hold
        (RESIZE, 0.5),                       # still files: 2 - b.txt invisible
        (b"\x1b[15~", 0.3),                 # F5: force re-read
        (RESIZE, 0.5),                       # files: 3, matches: 2
        (b"\x1b", 0.4),
    ])
    results.append(check("cache: no F5 - fs changes invisible",
        "files: 2" in chunks[1] and "files: 2" in chunks[4] and "b.txt" not in chunks[4],
        chunks[4][:400]))
    results.append(check("cache: F5 re-reads the tree",
        "files: 3" in chunks[6] and "matches: 2" in chunks[6], chunks[6][:400]))

    k = f"{base}/k"
    os.makedirs(k)
    for n in range(1, 31):
        open(f"{k}/f{n:02d}.txt", "w").close()
    chunks, clean = drive(k, [
        (b"f\\d+", 0.4),
        (RESIZE, 0.5),                       # long list, tail hidden
        (b"\x1b[<65;10;12M", 0.2),           # wheel down x4 = 12 lines
        (b"\x1b[<65;10;12M", 0.2),
        (b"\x1b[<65;10;12M", 0.2),
        (b"\x1b[<65;10;12M", 0.2),
        (RESIZE, 0.5),
        (b"\x1b[<0;100;24M", 0.4),           # drag: click the scrollbar bottom
        (RESIZE, 0.5),
        (b"\x1b[<64;10;12M", 0.3),           # ONE wheel up: must move at once
        (RESIZE, 0.5),
        (b"\x1b[<0;100;10M", 0.4),           # drag: click the scrollbar top
        (RESIZE, 0.5),
        (b"\x1b", 0.4),
    ])
    results.append(check("wheel: 4 notches scroll 12 lines",
        "f01.txt" in chunks[1] and "f01.txt" not in flat(chunks[6])
        and "f13.txt" in flat(chunks[6]) and "f30.txt" not in flat(chunks[6]),
        chunks[6][:300]))
    results.append(check("scrollbar: drag bottom = last line",
        "f30.txt" in flat(chunks[8]) and "f01.txt" not in flat(chunks[8]),
        chunks[8][:300]))
    results.append(check("no overscroll: one wheel up moves back at once",
        "f30.txt" not in flat(chunks[10]) and "f13.txt" in flat(chunks[10]),
        chunks[10][:300]))
    results.append(check("scrollbar: drag top = first line",
        "f01.txt" in flat(chunks[12]) and "f30.txt" not in flat(chunks[12]),
        chunks[12][:300]))

    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
