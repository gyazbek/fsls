//! Test-only input generator for "never panics on malformed input" tests: a cheap,
//! deterministic stand-in for fuzzing that runs on every platform and in CI.

/// Hostile-ish fragments: format syntax, escapes, multi-byte and control characters.
const TOKENS: &[&str] = &[
    "<",
    ">",
    "/",
    "\\",
    "<dict>",
    "</dict>",
    "<array>",
    "</array>",
    "<key>",
    "</key>",
    "<string>",
    "</string>",
    "<integer>",
    "</integer>",
    "<true/>",
    "<plist>",
    "\t",
    "\n",
    "\r\n",
    " ",
    "-",
    ":",
    "%",
    "0",
    "7",
    "9",
    "\\777",
    "\\040",
    "\\x",
    "\\x2",
    "\\xzz",
    "é",
    "→",
    "𝄞",
    "\u{1b}[2J",
    "\u{9b}",
    "\u{202e}",
    "\0",
    "disk",
    "s1",
    "-none-",
    " - ",
    "Name:",
    "Status:",
    "Device Node:",
    "V\t",
    "P\t",
    "18446744073709551616",
    "-1",
    "",
];

/// Inputs derived from `fixture`: every prefix, `n` random edits of it, and `n` strings of
/// random tokens.
pub fn inputs(fixture: &str, n: usize) -> Vec<String> {
    let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ fixture.len() as u64;
    let mut next = move || {
        // xorshift64
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut out: Vec<String> = Vec::new();
    let bounds: Vec<usize> = fixture
        .char_indices()
        .map(|(i, _)| i)
        .chain([fixture.len()])
        .collect();
    out.extend(bounds.iter().map(|&i| fixture[..i].to_string()));
    for _ in 0..n {
        let mut s = fixture.to_string();
        for _ in 0..1 + next() % 4 {
            let b: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
            let at = b[(next() % b.len() as u64) as usize];
            let end = b[b.iter().position(|&x| x == at).unwrap()..]
                .get((next() % 4) as usize)
                .copied()
                .unwrap_or(at);
            let token = TOKENS[(next() % TOKENS.len() as u64) as usize];
            s.replace_range(at..end, token);
        }
        out.push(s);
    }
    for _ in 0..n {
        let len = next() % 40;
        out.push(
            (0..len)
                .map(|_| TOKENS[(next() % TOKENS.len() as u64) as usize])
                .collect(),
        );
    }
    out
}
