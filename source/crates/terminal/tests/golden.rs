//! Feeds tests/scenarios.txt into the Rust emulator and compares the dumps with tests/golden/*.txt, which
//! tools/parity-terminal writes from the C# TerminalEmulator. A mismatch means the Rust port is wrong.

use std::fmt::Write as _;
use std::path::PathBuf;

use aio_terminal::emulator::rune_width;
use aio_terminal::{TerminalCell, TerminalEmulator, TerminalLine};

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn unescape(text: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next().expect("escape at the end") {
            'e' => bytes.push(0x1b),
            'r' => bytes.push(0x0d),
            'n' => bytes.push(0x0a),
            't' => bytes.push(0x09),
            's' => bytes.push(0x20),
            '\\' => bytes.push(b'\\'),
            'x' => {
                let hex: String = [chars.next().unwrap(), chars.next().unwrap()].iter().collect();
                bytes.push(u8::from_str_radix(&hex, 16).unwrap());
            }
            e => panic!("unknown escape \\{e}"),
        }
    }
    bytes
}

fn escape_rune(r: char) -> String {
    let v = r as u32;
    if r == '\\' {
        "\\\\".to_string()
    } else if v < 0x20 || (0x7f..=0x9f).contains(&v) {
        format!("\\u{{{v:X}}}")
    } else {
        r.to_string()
    }
}

fn escape(text: &str) -> String {
    text.chars().map(escape_rune).collect()
}

fn dump_line(o: &mut String, label: &str, line: &TerminalLine) {
    o.push_str(label);
    o.push_str(if line.wrapped { " w |" } else { " - |" });
    for c in &line.cells {
        o.push_str(&escape_rune(c.rune));
    }
    o.push_str("|\n");
    let mut runs = Vec::new();
    let mut x = 0;
    let cells = &line.cells;
    while x < cells.len() {
        let first = cells[x];
        let start = x;
        while x < cells.len()
            && cells[x].foreground == first.foreground
            && cells[x].background == first.background
            && cells[x].flags == first.flags
        {
            x += 1;
        }
        if first.foreground != TerminalCell::DEFAULT_COLOR
            || first.background != TerminalCell::DEFAULT_COLOR
            || !first.flags.is_empty()
        {
            runs.push(format!("{}-{}:{}/{}/{}", start, x - 1, first.foreground, first.background, first.flags.0));
        }
    }
    if !runs.is_empty() {
        o.push_str("     ");
        o.push_str(&runs.join(" "));
        o.push('\n');
    }
}

fn dump(t: &TerminalEmulator, o: &mut String, number: usize, replies: &str, bells: usize) {
    let b = |v: bool| v as i32;
    writeln!(o, "# dump {number}").unwrap();
    writeln!(o, "size {}x{}", t.columns(), t.rows()).unwrap();
    writeln!(o, "cursor {},{} visible={}", t.cursor_x(), t.cursor_y(), b(t.cursor_visible())).unwrap();
    writeln!(
        o,
        "modes app_cursor={} bracketed={} alternate={}",
        b(t.application_cursor_keys()),
        b(t.bracketed_paste()),
        b(t.alternate_screen())
    )
    .unwrap();
    writeln!(o, "title {}", escape(t.title())).unwrap();
    writeln!(o, "scrollback {} dropped {}", t.scrollback().len(), t.scrollback_dropped()).unwrap();
    writeln!(o, "replies {}", escape(replies)).unwrap();
    writeln!(o, "bells {bells}").unwrap();
    o.push_str("screen\n");
    for y in 0..t.rows() {
        dump_line(o, &format!("{y:02}"), t.line(y));
    }
    o.push_str("scrollback-lines\n");
    let n = t.scrollback().len();
    for i in 0..n {
        if n > 13 && i >= 3 && i < n - 10 {
            continue;
        }
        dump_line(o, &i.to_string(), &t.scrollback()[i]);
    }
    if t.absolute_count() <= 300 {
        let text = t.text((0, 0), (t.absolute_count() as i64 - 1, t.columns() - 1));
        writeln!(o, "text {}", escape(&text)).unwrap();
    }
}

struct Run {
    term: TerminalEmulator,
    out: String,
    replies: String,
    bells: usize,
    dumps: usize,
}

impl Run {
    fn collect(&mut self) {
        for r in self.term.take_replies() {
            self.replies.push_str(&r);
        }
        self.bells += self.term.take_bells();
    }

    fn dump(&mut self) {
        self.collect();
        self.dumps += 1;
        dump(&self.term, &mut self.out, self.dumps, &self.replies, self.bells);
    }
}

fn run_scenarios() -> Vec<(String, String)> {
    let script = std::fs::read_to_string(tests_dir().join("scenarios.txt")).unwrap();
    let mut results = Vec::new();
    let mut run: Option<Run> = None;
    let mut name = String::new();
    for raw in script.lines() {
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let (keyword, rest) = raw.split_once(' ').unwrap_or((raw, ""));
        match keyword {
            "scenario" => {
                let parts: Vec<&str> = rest.split(' ').collect();
                name = parts[0].to_string();
                run = Some(Run {
                    term: TerminalEmulator::new(parts[1].parse().unwrap(), parts[2].parse().unwrap()),
                    out: String::new(),
                    replies: String::new(),
                    bells: 0,
                    dumps: 0,
                });
            }
            "feed" => {
                let r = run.as_mut().unwrap();
                r.term.feed(&unescape(rest));
                r.collect();
            }
            "repeat" => {
                let r = run.as_mut().unwrap();
                let (n, body) = rest.split_once(' ').unwrap();
                for i in 0..n.parse::<usize>().unwrap() {
                    r.term.feed(&unescape(&body.replace("%d", &i.to_string())));
                }
                r.collect();
            }
            "resize" => {
                let parts: Vec<usize> = rest.split(' ').map(|p| p.parse().unwrap()).collect();
                run.as_mut().unwrap().term.resize(parts[0], parts[1]);
            }
            "select" => {
                let r = run.as_mut().unwrap();
                let v: Vec<i64> = rest.split(' ').map(|p| p.parse().unwrap()).collect();
                let text = r.term.text((v[0], v[1] as usize), (v[2], v[3] as usize));
                writeln!(r.out, "select {rest} {}", escape(&text)).unwrap();
            }
            "dump" => run.as_mut().unwrap().dump(),
            "end" => {
                let mut r = run.take().unwrap();
                r.dump();
                results.push((name.clone(), r.out));
            }
            _ => panic!("unknown line: {raw}"),
        }
    }
    results
}

#[test]
fn screens_match_csharp() {
    let golden = tests_dir().join("golden");
    let mut failures = Vec::new();
    let results = run_scenarios();
    assert!(results.len() >= 25, "only {} scenarios", results.len());
    for (name, actual) in &results {
        let expected = std::fs::read_to_string(golden.join(format!("{name}.txt")))
            .unwrap_or_else(|e| panic!("{name}: no golden file ({e}); written by tools/parity-terminal, in the git history up to commit 50dd986"));
        if &expected != actual {
            let line = expected.lines().zip(actual.lines()).position(|(a, b)| a != b).unwrap_or(0);
            let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.actual.txt"));
            std::fs::write(&out, actual).unwrap();
            failures.push(format!(
                "{name}: line {}\n  expected: {:?}\n  actual:   {:?}\n  ({})",
                line + 1,
                expected.lines().nth(line).unwrap_or(""),
                actual.lines().nth(line).unwrap_or(""),
                out.display()
            ));
        }
    }
    assert!(failures.is_empty(), "{} of {} scenarios differ:\n{}", failures.len(), results.len(), failures.join("\n"));
}

#[test]
fn zero_width_table_matches_dotnet() {
    let ranges = std::fs::read_to_string(tests_dir().join("golden").join("zero_width.txt")).unwrap();
    let mut expected = vec![false; 0x110000];
    for line in ranges.lines() {
        let (a, b) = line.split_once(' ').unwrap();
        for c in u32::from_str_radix(a, 16).unwrap()..=u32::from_str_radix(b, 16).unwrap() {
            expected[c as usize] = true;
        }
    }
    for c in 0x300u32..0x110000 {
        let Some(ch) = char::from_u32(c) else { continue };
        assert_eq!(rune_width(ch) == 0, expected[c as usize], "U+{c:04X}");
    }
}
