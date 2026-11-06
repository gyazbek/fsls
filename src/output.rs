use crate::fs::{Device, Disk, FileSystem, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Units {
    /// Powers of 1024: 931G
    Binary,
    /// Powers of 1000: 1.0T
    Si,
    /// Exact byte counts
    Bytes,
}

pub struct TableOptions {
    pub units: Units,
    /// Columns in order; see [`default_columns`].
    pub columns: Vec<Col>,
    /// Extra mount points as `  + path` rows under their file system.
    pub long: bool,
    /// One row per underlying disk under each file system.
    pub disks: bool,
    pub header: bool,
    pub total: bool,
    /// ANSI colours (the caller decides: terminal, NO_COLOR, --color).
    pub color: bool,
}

/// A column of the file-system table. `--columns` takes these names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Col {
    Mount,
    Label,
    #[value(alias = "type")]
    Fs,
    Kind,
    Bus,
    Size,
    Used,
    #[value(alias = "free")]
    Avail,
    Use,
    /// Inode (file slot) count
    Inodes,
    Iused,
    Ifree,
    Iuse,
    Raid,
    #[value(alias = "encryption")]
    Enc,
    Disks,
    Id,
    Maxname,
    Options,
    Source,
}

impl Col {
    fn header(self) -> &'static str {
        match self {
            Col::Mount => "MOUNT",
            Col::Label => "LABEL",
            Col::Fs => "FS",
            Col::Kind => "KIND",
            Col::Bus => "BUS",
            Col::Size => "SIZE",
            Col::Used => "USED",
            Col::Avail => "AVAIL",
            Col::Use => "USE%",
            Col::Inodes => "INODES",
            Col::Iused => "IUSED",
            Col::Ifree => "IFREE",
            Col::Iuse => "IUSE%",
            Col::Raid => "RAID",
            Col::Enc => "ENC",
            Col::Disks => "DISKS",
            Col::Id => "ID",
            Col::Maxname => "MAXNAME",
            Col::Options => "OPTIONS",
            Col::Source => "SOURCE",
        }
    }

    fn right(self) -> bool {
        matches!(
            self,
            Col::Size | Col::Used | Col::Avail | Col::Inodes | Col::Iused | Col::Ifree
        )
    }

    /// The name `--columns` accepts, also the CSV header.
    pub fn name(self) -> String {
        clap::ValueEnum::to_possible_value(&self)
            .map(|v| v.get_name().to_string())
            .unwrap_or_default()
    }
}

/// The columns shown without `--columns`.
pub fn default_columns(long: bool, disks: bool, inodes: bool) -> Vec<Col> {
    let mut cols = vec![Col::Mount, Col::Label, Col::Fs, Col::Kind, Col::Bus];
    if inodes {
        cols.extend([Col::Inodes, Col::Iused, Col::Ifree, Col::Iuse]);
    } else {
        cols.extend([Col::Size, Col::Used, Col::Avail, Col::Use]);
    }
    if long || disks {
        cols.push(Col::Raid);
    }
    if long {
        cols.extend([
            Col::Enc,
            Col::Disks,
            Col::Id,
            Col::Maxname,
            Col::Options,
            Col::Source,
        ]);
    }
    cols
}

/// One cell. `csv`: raw values (bytes, plain numbers, empty instead of `-`).
fn cell(fs: &FileSystem, col: Col, units: Units, csv: bool) -> String {
    let none = || if csv { String::new() } else { "-".into() };
    let text = |s: &str| if s.is_empty() { none() } else { s.to_string() };
    let bytes = |n: u64| match (fs.ready, csv) {
        (false, _) => none(),
        (true, true) => n.to_string(),
        (true, false) => size(n, units),
    };
    let inode = |n: fn(crate::fs::Inodes) -> u64| match (fs.inodes, csv) {
        (None, _) => none(),
        (Some(i), true) => n(i).to_string(),
        (Some(i), false) => count(n(i), units),
    };
    let percent = |p: Option<f64>| match (p, csv) {
        (Some(p), true) => format!("{:.1}", round_percent(p, 1)),
        (Some(p), false) => percent_text(p),
        (None, true) => String::new(),
        (None, false) if fs.timed_out => "timed out".into(),
        (None, false) if !fs.ready => "not ready".into(),
        (None, false) => "-".into(),
    };
    match col {
        Col::Mount => fs.mount().to_string(),
        Col::Label => text(&fs.label),
        Col::Fs => text(&fs.fs_type),
        Col::Kind => fs.kind.as_str().to_string(),
        Col::Bus => text(&fs.buses()),
        Col::Size => bytes(fs.total),
        Col::Used => bytes(fs.used()),
        Col::Avail => bytes(fs.avail),
        Col::Use => percent(fs.use_percent()),
        Col::Inodes => inode(|i| i.total),
        Col::Iused => inode(|i| i.used()),
        Col::Ifree => inode(|i| i.free),
        Col::Iuse => percent(fs.inodes.and_then(|i| i.percent())),
        Col::Raid => match (&fs.raid, &fs.raid_health) {
            (Some(r), Some(h)) => format!("{r}: {}", h.text),
            (Some(r), None) => r.clone(),
            (None, _) => none(),
        },
        Col::Enc => fs.encryption.clone().unwrap_or_else(none),
        Col::Disks => {
            let disks: Vec<&str> = fs.devices.iter().map(|d| d.name.as_str()).collect();
            text(&disks.join(","))
        }
        Col::Id => fs.id.clone().unwrap_or_else(none),
        Col::Maxname => fs.max_name.map_or_else(none, |n| n.to_string()),
        Col::Options => text(&fs.options.join(",")),
        Col::Source => fs.source.clone().unwrap_or_else(none),
    }
}

/// Colour for a cell: usage by how full, RAID by health.
fn paint(fs: &FileSystem, col: Col) -> Option<&'static str> {
    let level = |p: Option<f64>| match p {
        Some(p) if p >= 90.0 => Some(RED),
        Some(p) if p >= 75.0 => Some(YELLOW),
        Some(_) => Some(GREEN),
        None if fs.timed_out => Some(RED),
        None => None,
    };
    match col {
        Col::Use => level(fs.use_percent()),
        Col::Iuse => level(fs.inodes.and_then(|i| i.percent())),
        Col::Raid => fs
            .raid_health
            .as_ref()
            .map(|h| if h.ok { GREEN } else { RED }),
        _ => None,
    }
}

const BOLD: &str = "1";
const DIM: &str = "2";
const RED: &str = "31";
const GREEN: &str = "32";
const YELLOW: &str = "33";

/// Inode counts: `67M`, `177K`, `951` (`-b` for exact).
fn count(n: u64, units: Units) -> String {
    if units == Units::Bytes || n < 1000 {
        return n.to_string();
    }
    let mut v = n as f64;
    let mut i = 0;
    const SUFFIX: [&str; 6] = ["", "K", "M", "G", "T", "P"];
    while v >= 1000.0 && i < SUFFIX.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if v < 10.0 {
        format!("{v:.1}{}", SUFFIX[i])
    } else {
        format!("{v:.0}{}", SUFFIX[i])
    }
}

pub fn table(list: &[FileSystem], opt: &TableOptions) -> String {
    let cols = &opt.columns;
    let at = |c: Col| cols.iter().position(|x| *x == c);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut styles: Vec<Vec<Option<&'static str>>> = Vec::new();
    let blank = || vec![String::new(); cols.len()];

    if opt.header {
        rows.push(cols.iter().map(|c| c.header().to_string()).collect());
        styles.push(vec![Some(BOLD); cols.len()]);
    }
    for fs in list {
        rows.push(
            cols.iter()
                .map(|c| cell(fs, *c, opt.units, false))
                .collect(),
        );
        styles.push(cols.iter().map(|c| paint(fs, *c)).collect());
        if opt.long
            && let Some(i) = at(Col::Mount)
        {
            for extra in fs.mounts.iter().skip(1) {
                let mut r = blank();
                r[i] = format!("  + {extra}");
                rows.push(r);
                styles.push(vec![Some(DIM); cols.len()]);
            }
        }
        if opt.disks {
            // Disk rows reuse the columns: LABEL = model, FS = media (ssd/hdd),
            // KIND = RAID member state (faulty, spare, ...).
            for d in &fs.devices {
                let mut r = blank();
                let mut set = |c: Col, v: String| {
                    if let Some(i) = at(c) {
                        r[i] = v;
                    }
                };
                set(Col::Mount, format!("  > {}", d.name));
                set(Col::Label, d.model.clone());
                set(Col::Fs, d.media.clone());
                set(Col::Kind, d.state.clone());
                set(
                    Col::Bus,
                    if d.bus.is_empty() {
                        "-".into()
                    } else {
                        d.bus.clone()
                    },
                );
                set(
                    Col::Size,
                    if d.size > 0 {
                        size(d.size, opt.units)
                    } else {
                        "-".into()
                    },
                );
                let failed = !d.state.is_empty();
                rows.push(r);
                styles.push(
                    cols.iter()
                        .map(|c| {
                            if failed && *c == Col::Kind {
                                Some(RED)
                            } else {
                                None
                            }
                        })
                        .collect(),
                );
            }
        }
    }

    if opt.total {
        let sum = crate::fs::total_space(list);
        let (total, used, avail) = (sum.total, sum.used, sum.avail);
        let mut r = blank();
        let mut set = |c: Col, v: String| {
            if let Some(i) = at(c) {
                r[i] = v;
            }
        };
        set(Col::Mount, "total".into());
        set(Col::Size, size(total, opt.units));
        set(Col::Used, size(used, opt.units));
        set(Col::Avail, size(avail, opt.units));
        if let Some(p) = crate::fs::percent_of(used, avail) {
            set(Col::Use, percent_text(p));
        }
        if let Some(i) = sum.inodes {
            set(Col::Inodes, count(i.total, opt.units));
            set(Col::Iused, count(i.used(), opt.units));
            set(Col::Ifree, count(i.free, opt.units));
            if let Some(p) = i.percent() {
                set(Col::Iuse, percent_text(p));
            }
        }
        rows.push(r);
        styles.push(vec![Some(BOLD); cols.len()]);
    }

    let styles: &[Vec<Option<&str>>] = if opt.color { &styles } else { &[] };
    render_styled(&rows, |c| cols.get(c).is_some_and(|c| c.right()), styles)
}

/// `--csv`: one line per file system, raw values (bytes, plain numbers), RFC 4180 quoting.
pub fn csv(list: &[FileSystem], columns: &[Col], header: bool) -> String {
    let field = |s: &str| {
        let s = clean(s);
        if s.contains([',', '"', '\n', '\r']) || s.starts_with(' ') || s.ends_with(' ') {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.into_owned()
        }
    };
    let mut out = String::new();
    if header {
        let names: Vec<String> = columns.iter().map(|c| c.name()).collect();
        out.push_str(&names.join(","));
        out.push('\n');
    }
    for fs in list {
        let cells: Vec<String> = columns
            .iter()
            .map(|c| field(&cell(fs, *c, Units::Bytes, true)))
            .collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

pub fn size(n: u64, units: Units) -> String {
    let base = match units {
        Units::Bytes => return n.to_string(),
        Units::Binary => 1024.0,
        Units::Si => 1000.0,
    };
    const SUFFIX: [&str; 7] = ["B", "K", "M", "G", "T", "P", "E"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= base && i < SUFFIX.len() - 1 {
        v /= base;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else if v < 10.0 && format!("{v:.1}") != "10.0" {
        format!("{v:.1}{}", SUFFIX[i])
    } else {
        format!("{:.0}{}", v, SUFFIX[i])
    }
}

/// `p` rounded to `digits` decimals, except that it never rounds up to 100 while space is
/// left or down to 0 while something is stored: 2 TB with 1 GB free is 99.95% used, and
/// showing it as 100% would read as "full".
pub fn round_percent(p: f64, digits: i32) -> f64 {
    let step = 10f64.powi(-digits);
    let r = (p / step).round() * step;
    if p < 100.0 && r >= 100.0 {
        100.0 - step
    } else if p > 0.0 && r <= 0.0 {
        step
    } else {
        r
    }
}

/// ` 99% [#########.]`
fn percent_text(p: f64) -> String {
    format!("{:>3.0}% {}", round_percent(p, 0), bar(p, 10))
}

/// A usage bar; it is only completely filled when the file system is full.
pub fn bar(percent: f64, width: usize) -> String {
    let most = if percent < 100.0 {
        width.saturating_sub(1)
    } else {
        width
    };
    let filled = ((percent / 100.0) * width as f64)
        .round()
        .clamp(0.0, most as f64) as usize;
    format!("[{}{}]", "#".repeat(filled), ".".repeat(width - filled))
}

/// Make text from the system safe to print on a terminal. Volume labels, mount paths and
/// model strings can be set by whoever formatted a USB stick or created a directory, and an
/// embedded escape sequence would otherwise be interpreted by the terminal (moving the
/// cursor, rewriting earlier lines, changing the title). Control characters and bidi
/// overrides become visible `\x1b` / `\u{202e}` escapes.
pub fn clean(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(unsafe_char) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            c if c.is_control() && (c as u32) < 0x100 => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c if unsafe_char(c) => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// C0/C1 controls, DEL, and the bidirectional overrides used to disguise text.
fn unsafe_char(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Align rows into columns (two spaces apart); `right(col)` columns are right-aligned. Each
/// cell can carry an ANSI style (`styles[row][col]`, e.g. `"31"` = red); escape codes wrap
/// the cell text only, after widths are measured, so alignment holds.
fn render_styled(
    all: &[Vec<String>],
    right: impl Fn(usize) -> bool,
    styles: &[Vec<Option<&str>>],
) -> String {
    let all: Vec<Vec<String>> = all
        .iter()
        .map(|r| r.iter().map(|c| clean(c).into_owned()).collect())
        .collect();
    let cols = all.first().map_or(0, |r| r.len());
    let width: Vec<usize> = (0..cols)
        .map(|c| all.iter().map(|r| r[c].chars().count()).max().unwrap_or(0))
        .collect();

    let mut out = String::new();
    for (r, row) in all.iter().enumerate() {
        let mut line = String::new();
        for (c, cell) in row.iter().enumerate() {
            if c > 0 {
                line.push_str("  ");
            }
            let pad = width[c] - cell.chars().count();
            let text = match styles.get(r).and_then(|s| s.get(c)).copied().flatten() {
                Some(code) if !cell.is_empty() => format!("\x1b[{code}m{cell}\x1b[0m"),
                _ => cell.clone(),
            };
            if right(c) {
                line.extend(std::iter::repeat_n(' ', pad));
                line.push_str(&text);
            } else {
                line.push_str(&text);
                line.extend(std::iter::repeat_n(' ', pad));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// `fsls disks`: one row per disk, then one per partition. DETAILS, the only free-length
/// column, comes last: the device on disk rows, what is on it on partition rows.
pub fn disks_table(disks: &[Disk], units: Units, header: bool, color: bool) -> String {
    let mut all: Vec<Vec<String>> = Vec::new();
    let mut styles: Vec<Vec<Option<&'static str>>> = Vec::new();
    if header {
        all.push(
            ["NAME", "SIZE", "TYPE", "FS", "LABEL", "DETAILS"]
                .map(String::from)
                .to_vec(),
        );
        styles.push(vec![Some(BOLD); 6]);
    }
    let sz = |n: u64| if n > 0 { size(n, units) } else { "-".into() };
    let row = |name: String, size: String, kind: &str, u: &Usage| {
        let details = match (u.is_empty(), use_text(u)) {
            (true, _) => "-".to_string(),
            // A file system nobody mounted: often exactly what you are looking for.
            (false, t) if t.is_empty() => "not mounted".to_string(),
            (false, t) => t,
        };
        vec![
            name,
            size,
            kind.to_string(),
            u.fs_type.clone(),
            u.label.clone(),
            details,
        ]
    };
    for d in disks {
        let blank = d.parts.is_empty() && d.usage.is_empty();
        let kind = match (d.table.as_str(), blank) {
            (_, true) => "unused",
            ("", false) => "-",
            (t, false) => t,
        };
        all.push(vec![
            d.device.name.clone(),
            sz(d.device.size),
            kind.into(),
            String::new(),
            String::new(),
            describe(&d.device),
        ]);
        styles.push(vec![
            Some(BOLD),
            None,
            blank.then_some(YELLOW),
            None,
            None,
            None,
        ]);
        // A file system (or RAID member) on the whole disk, without a partition table.
        if !d.usage.is_empty() {
            all.push(row(
                "  (whole disk)".into(),
                sz(d.device.size),
                "",
                &d.usage,
            ));
            styles.push(Vec::new());
        }
        for p in &d.parts {
            all.push(row(format!("  {}", p.name), sz(p.size), &p.kind, &p.usage));
            styles.push(Vec::new());
        }
    }
    let styles: &[Vec<Option<&str>>] = if color { &styles } else { &[] };
    render_styled(&all, |c| c == 1, styles)
}

/// `APPLE SSD AP1024Z (fabric, ssd)`
fn describe(d: &Device) -> String {
    let extra: Vec<&str> = [d.bus.as_str(), d.media.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    match (d.model.is_empty(), extra.is_empty()) {
        (_, true) => d.model.clone(),
        (true, false) => extra.join(", "),
        (false, false) => format!("{} ({})", d.model, extra.join(", ")),
    }
}

/// Mount points, then what this device is part of: `C:\`, `-> md0 (raid1): /srv`.
fn use_text(u: &Usage) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !u.mounts.is_empty() {
        parts.push(u.mounts.join(", "));
    }
    parts.extend(u.used_by.iter().map(|b| format!("-> {b}")));
    parts.join("; ")
}

pub fn disks_json(disks: &[Disk]) -> String {
    let list = |v: &[String]| {
        format!(
            "[{}]",
            v.iter().map(|s| quote(s)).collect::<Vec<_>>().join(", ")
        )
    };
    let usage = |u: &Usage| {
        [
            ("fs_type", quote(&u.fs_type)),
            ("label", quote(&u.label)),
            ("mounts", list(&u.mounts)),
            ("used_by", list(&u.used_by)),
        ]
    };
    let mut out = String::from("[");
    for (i, d) in disks.iter().enumerate() {
        out.push_str(if i == 0 { "\n" } else { ",\n" });
        let parts: Vec<String> = d
            .parts
            .iter()
            .map(|p| {
                let mut f = vec![
                    ("name", quote(&p.name)),
                    ("type", quote(&p.kind)),
                    ("size_bytes", p.size.to_string()),
                ];
                f.extend(usage(&p.usage));
                let f: Vec<String> = f.iter().map(|(k, v)| format!("\"{k}\": {v}")).collect();
                format!("\n      {{{}}}", f.join(", "))
            })
            .collect();
        let mut fields = vec![
            ("name", quote(&d.device.name)),
            ("bus", quote(&d.device.bus)),
            ("model", quote(&d.device.model)),
            ("media", quote(&d.device.media)),
            ("size_bytes", d.device.size.to_string()),
            ("table", quote(&d.table)),
        ];
        fields.extend(usage(&d.usage));
        let close = if parts.is_empty() { "" } else { "\n    " };
        fields.push(("partitions", format!("[{}{close}]", parts.join(","))));
        out.push_str("  {\n");
        for (j, (k, v)) in fields.iter().enumerate() {
            let sep = if j + 1 < fields.len() { "," } else { "" };
            out.push_str(&format!("    \"{k}\": {v}{sep}\n"));
        }
        out.push_str("  }");
    }
    out.push_str(if disks.is_empty() { "]\n" } else { "\n]\n" });
    out
}

pub fn json(list: &[FileSystem]) -> String {
    let mut out = String::from("[");
    for (i, fs) in list.iter().enumerate() {
        out.push_str(if i == 0 { "\n" } else { ",\n" });
        let mounts: Vec<String> = fs.mounts.iter().map(|m| quote(m)).collect();
        let options: Vec<String> = fs.options.iter().map(|o| quote(o)).collect();
        let devices: Vec<String> = fs.devices.iter().map(device_json).collect();
        let fields = [
            ("mount", quote(fs.mount())),
            ("mounts", format!("[{}]", mounts.join(", "))),
            ("source", fs.source.as_deref().map_or("null".into(), quote)),
            ("label", quote(&fs.label)),
            ("fs_type", quote(&fs.fs_type)),
            ("kind", quote(fs.kind.as_str())),
            ("ready", fs.ready.to_string()),
            ("timed_out", fs.timed_out.to_string()),
            ("total_bytes", fs.total.to_string()),
            ("used_bytes", fs.used().to_string()),
            ("free_bytes", fs.free.to_string()),
            ("available_bytes", fs.avail.to_string()),
            (
                "use_percent",
                fs.use_percent()
                    .map_or("null".into(), |p| format!("{:.1}", round_percent(p, 1))),
            ),
            ("id", fs.id.as_deref().map_or("null".into(), quote)),
            (
                "max_name_length",
                fs.max_name.map_or("null".into(), |n| n.to_string()),
            ),
            (
                "inodes_total",
                fs.inodes.map_or("null".into(), |i| i.total.to_string()),
            ),
            (
                "inodes_used",
                fs.inodes.map_or("null".into(), |i| i.used().to_string()),
            ),
            (
                "inodes_free",
                fs.inodes.map_or("null".into(), |i| i.free.to_string()),
            ),
            (
                "encryption",
                fs.encryption.as_deref().map_or("null".into(), quote),
            ),
            ("options", format!("[{}]", options.join(", "))),
            ("raid", fs.raid.as_deref().map_or("null".into(), quote)),
            (
                "raid_health",
                fs.raid_health.as_ref().map_or("null".into(), |h| {
                    format!("{{\"ok\": {}, \"text\": {}}}", h.ok, quote(&h.text))
                }),
            ),
            ("devices", format!("[{}]", devices.join(", "))),
        ];
        out.push_str("  {\n");
        for (j, (k, v)) in fields.iter().enumerate() {
            let sep = if j + 1 < fields.len() { "," } else { "" };
            out.push_str(&format!("    \"{k}\": {v}{sep}\n"));
        }
        out.push_str("  }");
    }
    out.push_str(if list.is_empty() { "]\n" } else { "\n]\n" });
    out
}

fn device_json(d: &Device) -> String {
    format!(
        "{{\"name\": {}, \"bus\": {}, \"model\": {}, \"media\": {}, \"size_bytes\": {}, \"state\": {}}}",
        quote(&d.name),
        quote(&d.bus),
        quote(&d.model),
        quote(&d.media),
        d.size,
        quote(&d.state)
    )
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // All controls (C0, DEL, C1) and bidi overrides: valid JSON either way, but
            // escaping keeps `fsls --json` safe to print on a terminal too.
            c if unsafe_char(c) => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::sample;

    /// Table options with the default columns for `long`/`disks`, binary units, no colour.
    fn opts(long: bool, disks: bool, header: bool, total: bool) -> TableOptions {
        TableOptions {
            units: Units::Binary,
            columns: default_columns(long, disks, false),
            long,
            disks,
            header,
            total,
            color: false,
        }
    }

    #[test]
    fn custom_columns_and_csv() {
        let mut fs = sample("/srv, \"data\"", "ext4", 2048, 1024);
        fs.label = " padded ".into();
        fs.encryption = Some("luks".into());
        let mut o = opts(false, false, true, false);
        o.columns = vec![Col::Mount, Col::Use, Col::Enc];
        assert_eq!(
            table(&[fs.clone()], &o),
            "MOUNT         USE%               ENC\n/srv, \"data\"   50% [#####.....]  luks\n"
        );

        let cols = [
            Col::Mount,
            Col::Label,
            Col::Size,
            Col::Use,
            Col::Iuse,
            Col::Raid,
        ];
        assert_eq!(
            csv(&[fs], &cols, true),
            "mount,label,size,use,iuse,raid\n\"/srv, \"\"data\"\"\",\" padded \",2048,50.0,,\n"
        );
        // Names round-trip through clap, including the aliases' canonical names.
        assert_eq!(Col::Avail.name(), "avail");
        assert_eq!(
            <Col as clap::ValueEnum>::from_str("free", true).unwrap(),
            Col::Avail
        );
    }

    #[test]
    fn inode_view() {
        let mut fs = sample("/", "ext4", 100, 50);
        fs.inodes = Some(crate::fs::Inodes {
            total: 67_108_864,
            free: 66_931_495,
        });
        let ntfs = sample("C:\\", "NTFS", 100, 50); // no fixed inode count
        let mut o = opts(false, false, true, true);
        o.columns = default_columns(false, false, true);
        let t = table(&[fs, ntfs], &o);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(
            lines[0],
            "MOUNT  LABEL  FS    KIND   BUS  INODES  IUSED  IFREE  IUSE%"
        );
        assert_eq!(
            lines[1],
            "/      -      ext4  fixed  -       67M   177K    67M    1% [..........]"
        );
        assert_eq!(
            lines[2],
            "C:\\    -      NTFS  fixed  -         -      -      -  -"
        );
        assert!(
            lines[3].starts_with("total") && lines[3].contains("67M"),
            "{t}"
        );
    }

    #[test]
    fn colour_keeps_alignment() {
        let mut full = sample("/full", "ext4", 100, 5);
        full.raid = Some("raid1".into());
        full.raid_health = Some(crate::fs::RaidHealth {
            ok: false,
            text: "degraded".into(),
        });
        let half = sample("/half", "ext4", 100, 50);
        let mut o = opts(true, false, true, false);
        let plain = table(&[full.clone(), half.clone()], &o);
        o.color = true;
        let coloured = table(&[full, half], &o);
        assert!(
            coloured.contains("\x1b[31m 95% [#########.]\x1b[0m"),
            "{coloured:?}"
        );
        assert!(coloured.contains("\x1b[31mraid1: degraded\x1b[0m"));
        assert!(coloured.contains("\x1b[32m 50%"));
        assert!(coloured.starts_with("\x1b[1mMOUNT\x1b[0m"));
        // Removing the escape codes gives exactly the plain table.
        let stripped = coloured
            .split('\x1b')
            .enumerate()
            .map(|(i, s)| {
                if i == 0 {
                    s
                } else {
                    &s[s.find('m').unwrap() + 1..]
                }
            })
            .collect::<String>();
        assert_eq!(stripped, plain);
    }

    #[test]
    fn sizes() {
        assert_eq!(size(0, Units::Binary), "0B");
        assert_eq!(size(1023, Units::Binary), "1023B");
        assert_eq!(size(1536, Units::Binary), "1.5K");
        assert_eq!(size(10 * 1024 * 1024 - 1, Units::Binary), "10M");
        assert_eq!(size(1_000_204_886_016, Units::Binary), "932G");
        assert_eq!(size(1_000_204_886_016, Units::Si), "1.0T");
        assert_eq!(size(12345, Units::Bytes), "12345");
    }

    #[test]
    fn bars() {
        assert_eq!(bar(0.0, 10), "[..........]");
        assert_eq!(bar(63.0, 10), "[######....]");
        assert_eq!(bar(100.0, 4), "[####]");
        assert_eq!(bar(99.6, 10), "[#########.]");
    }

    #[test]
    fn percent_rounding_never_claims_full_or_empty() {
        assert_eq!(percent_text(99.6), " 99% [#########.]");
        assert_eq!(percent_text(100.0), "100% [##########]");
        assert_eq!(percent_text(0.2), "  1% [..........]");
        assert_eq!(percent_text(0.0), "  0% [..........]");
        assert_eq!(percent_text(42.5), " 43% [####......]");
        assert_eq!(format!("{:.1}", round_percent(99.97, 1)), "99.9");
        assert_eq!(format!("{:.1}", round_percent(0.01, 1)), "0.1");
        assert_eq!(format!("{:.1}", round_percent(37.26, 1)), "37.3");
    }

    #[test]
    fn json_escapes_and_nulls() {
        let mut fs = sample("C:\\", "NTFS", 100, 40);
        fs.label = "My \"Disk\"".into();
        let j = json(&[fs]);
        assert!(j.contains(r#""mount": "C:\\","#));
        assert!(j.contains(r#""label": "My \"Disk\"","#));
        assert!(j.contains(r#""source": null,"#));
        assert!(j.contains(r#""use_percent": 60.0,"#));
        assert_eq!(json(&[]), "[]\n");
    }

    /// A USB stick labelled with an escape sequence must not reach the terminal raw.
    #[test]
    fn hostile_labels_are_neutralized() {
        let evil = "x\u{1b}]0;pwned\u{7}\u{1b}[2J\u{9b}31m\u{202e}gnp.exe\r\nfake";
        assert_eq!(
            clean(evil),
            r"x\x1b]0;pwned\x07\x1b[2J\x9b31m\u{202e}gnp.exe\x0d\x0afake"
        );
        assert!(matches!(clean("plain é 𝄞"), std::borrow::Cow::Borrowed(_)));

        let mut fs = sample("/media/usb", "vfat", 10, 5);
        fs.label = evil.into();
        let opt = opts(true, true, true, true);
        for out in [table(&[fs.clone()], &opt), json(&[fs.clone()])] {
            assert!(
                !out.chars()
                    .any(|c| (c.is_control() && c != '\n') || c == '\u{202e}'),
                "{out:?}"
            );
        }
    }

    /// Both JSON outputs are hand-written: check they parse and round-trip awkward values.
    #[test]
    fn json_is_valid_and_round_trips() {
        use crate::fs::{Disk, Part};
        let evil = "q\"uote \\ back\u{1b}[31m\u{7f}\u{85} é 𝄞\t\n";
        let mut fs = sample(evil, "ext4", 100, 40);
        fs.label = evil.into();
        fs.source = Some(evil.into());
        fs.raid = Some("raid1".into());
        fs.raid_health = Some(crate::fs::RaidHealth {
            ok: false,
            text: evil.into(),
        });
        fs.devices = vec![Device {
            name: evil.into(),
            ..Device::default()
        }];
        let v: serde_json::Value =
            serde_json::from_str(&json(&[fs.clone(), sample("/", "ext4", 0, 0)]))
                .expect("valid json");
        assert_eq!(v[0]["label"], evil);
        assert_eq!(v[0]["source"], evil);
        assert_eq!(v[0]["raid_health"]["text"], evil);
        assert_eq!(v[0]["devices"][0]["name"], evil);
        assert_eq!(v[1]["use_percent"], serde_json::Value::Null);

        let usage = Usage {
            fs_type: evil.into(),
            label: evil.into(),
            mounts: vec![evil.into()],
            used_by: vec![evil.into()],
        };
        let disks = [
            Disk {
                device: Device {
                    name: evil.into(),
                    size: 1,
                    ..Device::default()
                },
                table: "gpt".into(),
                usage: Usage::default(),
                parts: vec![Part {
                    name: evil.into(),
                    kind: evil.into(),
                    size: 2,
                    usage: usage.clone(),
                }],
            },
            Disk {
                usage,
                ..Disk::default()
            },
        ];
        let v: serde_json::Value = serde_json::from_str(&disks_json(&disks)).expect("valid json");
        assert_eq!(v[0]["partitions"][0]["type"], evil);
        assert_eq!(v[0]["partitions"][0]["mounts"][0], evil);
        assert_eq!(v[1]["used_by"][0], evil);
        assert_eq!(v[1]["partitions"], serde_json::json!([]));
        for empty in [json(&[]), disks_json(&[])] {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&empty).unwrap(),
                serde_json::json!([])
            );
        }
    }

    /// A long DETAILS cell must not push any other column right (one long APFS line would
    /// otherwise put BUS/MEDIA/MODEL ~100 columns in).
    #[test]
    fn disks_table_keeps_free_text_last() {
        use crate::fs::{Disk, Part};
        const G: u64 = 1 << 30;
        let dev = |name: &str, model: &str, bus: &str, media: &str| Device {
            name: name.into(),
            model: model.into(),
            bus: bus.into(),
            media: media.into(),
            size: 932 * G,
            state: String::new(),
        };
        let apfs = Part {
            name: "disk0s2".into(),
            kind: "APFS".into(),
            size: 926 * G,
            usage: Usage {
                used_by: vec![
                    "disk3 (apfs container): /, /System/Volumes/Data, /a/very/long/mount/point"
                        .into(),
                ],
                ..Usage::default()
            },
        };
        let disks = [
            Disk {
                device: dev("disk0", "APPLE SSD AP1024Z", "fabric", "ssd"),
                table: "gpt".into(),
                usage: Usage::default(),
                parts: vec![apfs],
            },
            Disk {
                device: dev("sdf", "Virtual Disk", "hyperv", ""),
                table: String::new(),
                usage: Usage {
                    fs_type: "ext4".into(),
                    mounts: vec!["/".into()],
                    ..Usage::default()
                },
                parts: vec![],
            },
            Disk {
                device: dev("sdg", "", "usb", ""),
                ..Disk::default()
            },
            Disk {
                device: dev("sdh", "", "", ""),
                usage: Usage {
                    fs_type: "ext4".into(),
                    ..Usage::default()
                },
                ..Disk::default()
            },
        ];
        let t = disks_table(&disks, Units::Binary, true, false);
        assert_eq!(
            t,
            "\
NAME            SIZE  TYPE    FS    LABEL  DETAILS
disk0           932G  gpt                  APPLE SSD AP1024Z (fabric, ssd)
  disk0s2       926G  APFS                 -> disk3 (apfs container): /, /System/Volumes/Data, /a/very/long/mount/point
sdf             932G  -                    Virtual Disk (hyperv)
  (whole disk)  932G          ext4         /
sdg             932G  unused               usb
sdh             932G  -
  (whole disk)  932G          ext4         not mounted
"
        );
    }

    /// `/` and `/System/Volumes/Data` share one APFS container and both report its full size.
    #[test]
    fn total_counts_a_shared_pool_once() {
        const G: u64 = 1 << 30;
        let mut root = sample("/", "apfs", 926 * G, 344 * G);
        root.pool = Some(crate::fs::Pool::apfs("disk3".into()));
        let mut data = sample("/System/Volumes/Data", "apfs", 926 * G, 344 * G);
        data.pool = Some(crate::fs::Pool::apfs("disk3".into()));
        let usb = sample("/Volumes/RECORDS", "ntfs", 32 * G, 24 * G);
        let opt = opts(false, false, false, true);
        let t = table(&[root, data, usb], &opt);
        let total = t.lines().last().unwrap();
        // 926G + 32G, not 926G * 2 + 32G
        assert!(
            total.starts_with("total") && total.contains(" 958G "),
            "{t}"
        );
        assert!(total.contains(" 590G "), "used = 582G + 8G: {t}");
    }

    #[test]
    fn table_aligns_columns_and_totals() {
        let list = [
            sample("C:\\", "NTFS", 1000 * 1024 * 1024, 500 * 1024 * 1024),
            sample("D:\\", "exFAT", 2048, 1024),
        ];
        let t = table(&list, &opts(false, false, true, true));
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines[0],
            "MOUNT  LABEL  FS     KIND   BUS   SIZE  USED  AVAIL  USE%"
        );
        assert!(lines[1].starts_with(
            "C:\\    -      NTFS   fixed  -    1000M  500M   500M   50% [#####.....]"
        ));
        assert!(lines[3].starts_with("total"));
    }

    #[test]
    fn disk_rows_and_raid() {
        let disk = |name: &str, bus: &str| Device {
            name: name.into(),
            bus: bus.into(),
            model: "WDC WD20EFRX".into(),
            media: "hdd".into(),
            size: 2 * 1024 * 1024 * 1024 * 1024,
            state: String::new(),
        };
        let mut fs = sample("/srv", "ext4", 1024, 512);
        fs.raid = Some("raid1".into());
        fs.raid_health = Some(crate::fs::RaidHealth {
            ok: false,
            text: "degraded 1/3 missing".into(),
        });
        fs.devices = vec![disk("sda", "sata"), disk("sdb", "sata"), disk("sdc", "usb")];
        fs.devices[2].state = "faulty".into();
        let t = table(&[fs.clone()], &opts(false, true, false, false));
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(
            lines[0].contains(" sata,usb ") && lines[0].ends_with(" raid1: degraded 1/3 missing"),
            "{t}"
        );
        // KIND is 6 wide because of "faulty".
        assert_eq!(
            lines[1],
            format!(
                "  > sda  WDC WD20EFRX  hdd{}sata{}2.0T",
                " ".repeat(11),
                " ".repeat(6)
            )
        );
        assert!(lines[3].contains("  hdd   faulty  usb "), "{t}");

        let mut hung = sample("/mnt/nfs", "nfs4", 0, 0);
        (hung.ready, hung.timed_out) = (false, true);
        assert!(
            table(&[hung], &opts(false, false, false, false))
                .trim_end()
                .ends_with("timed out")
        );

        let j = json(&[fs]);
        assert!(j.contains(r#""raid": "raid1","#));
        assert!(j.contains(r#""raid_health": {"ok": false, "text": "degraded 1/3 missing"},"#));
        assert!(j.contains(r#"{"name": "sdc", "bus": "usb", "model": "WDC WD20EFRX", "media": "hdd", "size_bytes": 2199023255552, "state": "faulty"}"#));
    }
}
