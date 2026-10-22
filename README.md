# mvr

Pronounced **"mover"** — `mv`, upgraded with an R: **R**egex, **R**ust, **R**ecursive.

Batch-rename files by regex. Every change is shown before it happens and applied
only after an explicit confirmation.

```console
$ mvr 'img_(\d+)\.jpg' 'photo_$1.jpg'
img_01.jpg => photo_01.jpg
img_02.jpg => photo_02.jpg
rename? (N/y) y
renamed: 2
```

A script part computes pieces a plain template cannot - zero-padding, case
folding, anything:

```console
$ mvr 's (\d+)\.mkv' 'e$|if len(g1) < 2 { "0" + g1 } else { g1 }|$.mkv'
s 1.mkv => e01.mkv
s 2.mkv => e02.mkv
s 10.mkv => e10.mkv
rename? (N/y) y
renamed: 3
```

Two faces, one binary:

- `mvr REGEX PATTERN` — one-shot CLI: prints the `old => new` plan, asks, applies.
- `mvr` (no arguments, in a terminal) — interactive TUI with a live preview that
  updates on every keystroke: type the regex and see what gets filtered, type the
  pattern and see where everything lands.

In a pipe or without a tty a bare `mvr` prints usage instead of hanging a script.
`mvr -i` keeps the TUI open after applying - tweak the inputs and run the next
batch of renames in the same session.

## Install

From source:

```console
$ cargo install --path .
```

Or grab a prebuilt binary from the [releases] page.

On Arch, a local PKGBUILD is included:

```console
$ cd packaging && makepkg -si
```

[releases]: https://github.com/DevByG/mvr/releases

## Semantics

REGEX matches file names in the current directory. A `/` in REGEX separates path
components: **pattern depth equals path depth**, and the scan goes exactly as
deep as the pattern, entering only directories whose names match the
corresponding segment — `(\d+)/x\.txt` matches `01/x.txt`, never `01/a/x.txt`,
and directories like `.cache` are not even opened. A pattern without `/` only
touches the top level. Directories read by the scanner are cached: edits to the
pattern reuse the cache, `F5` (or applying) re-reads the filesystem.

PATTERN builds the new name (or path):

| token   | meaning                                                     |
| ------- | ----------------------------------------------------------- |
| `$0`    | the whole match (the whole path for path patterns)          |
| `$1..$9`| capture groups (numbered across path segments)               |
| `$i`    | ordinal number of the file (from 1, natural order)         |
| `$$`    | a literal `$`                                                |
| `$|…|$` | script part, a rhai expression: `g0`..`g9` hold the whole match and capture groups, `i` is the file number; must evaluate to a string (an int is stringified) |

Behavior details:

- matching is case-insensitive by default; `(?-i)` restores case sensitivity
- `/` in PATTERN sorts files into folders, created automatically;
  `..` and absolute paths are allowed — like in `mv`
- directories emptied by a run are removed, and the plan tells you which
- safety first: duplicate targets, already-taken names and chains are refused
  before anything moves, and the destination is re-checked right before every
  single rename — a silent overwrite is impossible

## The TUI

```
mvr  ·  files: 12  ·  matches: 12  ·  case: ignored
┌ regex ──────────────────────────────┐┌ case ──┐
│ (\d+) junk\.mkv                     ││ [Tt]   │
└─────────────────────────────────────┘└────────┘
┌ pattern ────────────────────────────┐
│ clean-$1.mkv                        │
└─────────────────────────────────────┘
01 junk.mkv => clean-01.mkv
02 junk.mkv => clean-02.mkv
...
 Tab fields · Enter next/apply · Ctrl+T case · F5 rescan · ↑↓ PgUp/PgDn scroll · Esc quit
```

Conflicting lines (duplicate or taken target names) are highlighted in red
right in the preview, with the error spelled out below.

Keys: `Tab` switch fields · `Enter` next field / apply · `Ctrl+T` or the `[Tt]`
button (clickable) toggles case sensitivity · `F5` rescan · `Esc` quit.

## Development

```console
$ cargo test                        # unit + integration suites
$ python3 scripts/mvr_pty_check.py  # live pty smoke test of the TUI
```

## License

[GPL-3.0](LICENSE)
