#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!("fsls supports Windows, Linux and macOS");

#[cfg(any(target_os = "macos", test))]
mod appleraid;
mod deadline;
mod fs;
#[cfg(any(target_os = "macos", test))]
mod macdisks;
#[cfg(any(target_os = "linux", test))]
mod mountinfo;
mod output;
mod parttype;
#[cfg(any(target_os = "macos", test))]
mod plist;
#[cfg(test)]
mod testutil;

#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
mod sys;

use std::cmp::Ordering;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use fs::{FileSystem, Kind};
use output::{Col, TableOptions, Units};

/// List mounted file systems: drives, volumes, mount points and network shares.
///
/// With PATH arguments, shows only the file systems containing those paths.
/// `fsls disks` lists physical disks and partitions instead, including unmounted ones.
///
/// Exit status: 0 ok, 1 a PATH could not be resolved, 2 --warn found a problem.
#[derive(Parser, Debug)]
#[command(name = "fsls", version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Show only the file systems that contain these paths
    paths: Vec<PathBuf>,

    /// Include hidden entries: pseudo file systems, system volumes, empty drives; with
    /// `disks`, also loop devices and disk images
    #[arg(short, long, global = true)]
    all: bool,

    /// Long listing: RAID layout, disks, id/UUID, max file name length, options/flags, source, extra mount points
    #[arg(short, long, conflicts_with = "json")]
    long: bool,

    /// List the physical disks under each file system (model, ssd/hdd, bus, size) and its RAID layout
    #[arg(short, long, conflicts_with_all = ["json", "csv"])]
    disks: bool,

    /// Show inode (file slot) usage instead of space, like `df -i` (Linux, macOS)
    #[arg(short, long)]
    inodes: bool,

    /// Columns to show, in order (comma separated): mount, label, fs, kind, bus, size, used,
    /// avail, use, inodes, iused, ifree, iuse, raid, enc, disks, id, maxname, options, source
    #[arg(
        short = 'o',
        long,
        value_enum,
        value_name = "COLS",
        value_delimiter = ',',
        hide_possible_values = true,
        conflicts_with = "json"
    )]
    columns: Vec<Col>,

    /// Print exact byte counts
    #[arg(short, long, conflicts_with = "si", global = true)]
    bytes: bool,

    /// Use powers of 1000 instead of 1024
    #[arg(long, global = true)]
    si: bool,

    /// Only these file system types (comma separated, e.g. ntfs,ext4,apfs)
    #[arg(short = 't', long = "type", value_name = "FS", value_delimiter = ',')]
    types: Vec<String>,

    /// Exclude these file system types (comma separated)
    #[arg(
        short = 'x',
        long = "exclude-type",
        value_name = "FS",
        value_delimiter = ','
    )]
    exclude_types: Vec<String>,

    /// Only these drive kinds (comma separated)
    #[arg(short, long, value_enum, value_delimiter = ',')]
    kind: Vec<Kind>,

    /// Only file systems on disks with these bus types (comma separated, e.g. nvme,usb)
    #[arg(long, value_name = "BUS", value_delimiter = ',')]
    bus: Vec<String>,

    /// Sort key; size, used, avail and use sort largest first
    #[arg(short, long, value_enum, default_value_t = SortKey::Mount)]
    sort: SortKey,

    /// Reverse the sort order
    #[arg(short, long)]
    reverse: bool,

    /// Add a grand-total row
    #[arg(long, conflicts_with_all = ["json", "csv"])]
    total: bool,

    /// Warn on stderr about file systems at least PCT% full (default 90), and exit with 2 if
    /// any is, or an array is degraded, or a mount timed out. Use as --warn or --warn=PCT
    #[arg(
        long,
        value_name = "PCT",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "90",
        value_parser = parse_pct
    )]
    warn: Option<f64>,

    /// Print JSON instead of a table
    #[arg(long, global = true, conflicts_with = "no_header")]
    json: bool,

    /// Print CSV (raw byte counts and percentages) instead of a table
    #[arg(long, conflicts_with = "json")]
    csv: bool,

    /// Colour the output: auto (when writing to a terminal and NO_COLOR is unset), always,
    /// never
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = ColorWhen::Auto, global = true)]
    color: ColorWhen,

    /// Omit the header row
    #[arg(short = 'H', long, global = true)]
    no_header: bool,

    /// Seconds to wait for a file system's size before marking it "timed out"
    /// (dead network mounts would otherwise hang the listing)
    #[arg(long, value_name = "SECS", default_value = "2", value_parser = parse_secs, global = true)]
    timeout: Duration,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    /// List physical disks and their partitions, including unformatted, unmounted and
    /// RAID-member ones (like lsblk)
    Disks,
    /// Print a shell completion script, e.g. `fsls completions bash > /etc/bash_completion.d/fsls`
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print the man page (roff), e.g. `fsls man > /usr/local/share/man/man1/fsls.1`
    Man,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ColorWhen {
    Auto,
    Always,
    Never,
}

/// Whether to colour stdout: `--color`, then NO_COLOR (no-color.org) and a terminal check.
fn use_color(when: ColorWhen) -> bool {
    use std::io::IsTerminal;
    let wanted = match when {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => {
            std::io::stdout().is_terminal()
                && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
                && std::env::var_os("TERM").is_none_or(|t| t != "dumb")
        }
    };
    // Windows consoles interpret escape codes only once asked to.
    wanted && sys::enable_ansi()
}

fn parse_pct(s: &str) -> Result<f64, String> {
    match s.trim_end_matches('%').parse::<f64>() {
        Ok(v) if v > 0.0 && v <= 100.0 => Ok(v),
        _ => Err("expected a percentage between 0 and 100".into()),
    }
}

fn parse_secs(s: &str) -> Result<Duration, String> {
    match s.parse::<f64>() {
        Ok(v) if v > 0.0 && v <= 3600.0 => Ok(Duration::from_secs_f64(v)),
        _ => Err("expected a number of seconds between 0 and 3600".into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SortKey {
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let units = if cli.bytes {
        Units::Bytes
    } else if cli.si {
        Units::Si
    } else {
        Units::Binary
    };

    match cli.command {
        Some(Command::Disks) => {
            // The file-system list says what is mounted where; disks() maps it onto partitions.
            let disks = sys::disks(&sys::list(cli.timeout), cli.all);
            let text = if cli.json {
                output::disks_json(&disks)
            } else {
                output::disks_table(&disks, units, !cli.no_header, use_color(cli.color))
            };
            let _ = std::io::stdout().lock().write_all(text.as_bytes());
            return ExitCode::SUCCESS;
        }
        Some(Command::Completions { shell }) => {
            use clap::CommandFactory;
            let mut out = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "fsls", &mut out);
            let _ = std::io::stdout().lock().write_all(&out);
            return ExitCode::SUCCESS;
        }
        Some(Command::Man) => {
            use clap::CommandFactory;
            let mut out = Vec::new();
            if clap_mangen::Man::new(Cli::command())
                .render(&mut out)
                .is_err()
            {
                return ExitCode::FAILURE;
            }
            let _ = std::io::stdout().lock().write_all(&out);
            return ExitCode::SUCCESS;
        }
        None => {}
    }

    let mut failed = false;

    let mut list = sys::list(cli.timeout);

    if !cli.paths.is_empty() {
        let mut wanted: Vec<String> = Vec::new();
        // Resolving a path on a dead mount blocks too.
        let found = deadline::run_all(
            cli.paths.iter().map(|p| {
                let p = p.clone();
                move || sys::mount_of(&p)
            }),
            cli.timeout,
        );
        for (p, r) in cli.paths.iter().zip(found) {
            match r {
                Some(Ok(m)) => wanted.push(m),
                Some(Err(e)) => {
                    let msg = format!("{}: {e}", p.display());
                    eprintln!("fsls: {}", output::clean(&msg));
                    failed = true;
                }
                None => {
                    let msg = format!(
                        "{}: no response within {:.1}s",
                        p.display(),
                        cli.timeout.as_secs_f64()
                    );
                    eprintln!("fsls: {}", output::clean(&msg));
                    failed = true;
                }
            }
        }
        list.retain(|fs| {
            fs.mounts
                .iter()
                .any(|m| wanted.iter().any(|w| sys::same_path(w, m)))
        });
    } else if !cli.all {
        list.retain(|fs| !fs.hidden);
    }

    filter(&mut list, &cli);
    sort(&mut list, cli.sort, cli.reverse);

    let columns = if cli.columns.is_empty() {
        output::default_columns(cli.long, cli.disks, cli.inodes)
    } else {
        cli.columns.clone()
    };
    // BitLocker (COM) and FileVault (diskutil per volume) cost time: fetch only when shown.
    if cli.json || columns.contains(&Col::Enc) {
        sys::add_encryption(&mut list, cli.timeout);
    }

    let text = if cli.json {
        output::json(&list)
    } else if cli.csv {
        output::csv(&list, &columns, !cli.no_header)
    } else {
        output::table(
            &list,
            &TableOptions {
                units,
                columns,
                long: cli.long,
                disks: cli.disks,
                header: !cli.no_header,
                total: cli.total,
                color: use_color(cli.color),
            },
        )
    };
    // Ignore write errors so `fsls | Select-Object -First 1` doesn't panic on a closed pipe.
    let _ = std::io::stdout().lock().write_all(text.as_bytes());

    let warned = warnings(&list, cli.timeout, cli.warn);
    for line in &warned {
        eprintln!("fsls: warning: {}", output::clean(line));
    }

    if failed {
        ExitCode::FAILURE
    } else if cli.warn.is_some() && !warned.is_empty() {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

/// Problems worth flagging even in a plain listing: degraded arrays and hung mounts, plus
/// file systems at least `full`% used when `--warn` is given.
fn warnings(list: &[FileSystem], timeout: Duration, full: Option<f64>) -> Vec<String> {
    let mut out = Vec::new();
    for fs in list {
        // Read-only images (ISO, squashfs, a sealed system volume) are full by design.
        let read_only =
            fs.kind == Kind::Cdrom || fs.options.iter().any(|o| o == "ro" || o == "readonly");
        if let (Some(limit), Some(p)) = (full, fs.use_percent())
            && p >= limit
            && !read_only
        {
            out.push(format!("{}: {p:.0}% full (--warn={limit})", fs.mount()));
        }
        // Out of inodes = "disk full" with free space left; same threshold.
        if let (Some(limit), Some(p)) = (full, fs.inodes.and_then(|i| i.percent()))
            && p >= limit
            && !read_only
        {
            out.push(format!(
                "{}: {p:.0}% of inodes used (--warn={limit})",
                fs.mount()
            ));
        }
        if let Some(h) = fs.raid_health.as_ref().filter(|h| !h.ok) {
            let raid = fs.raid.as_deref().unwrap_or("RAID");
            out.push(format!("{}: {raid} array is {}", fs.mount(), h.text));
        }
        if fs.timed_out {
            out.push(format!(
                "{}: no response within {:.1}s (dead network mount?)",
                fs.mount(),
                timeout.as_secs_f64()
            ));
        }
    }
    out
}

fn filter(list: &mut Vec<FileSystem>, cli: &Cli) {
    let has =
        |set: &[String], fs: &FileSystem| set.iter().any(|t| t.eq_ignore_ascii_case(&fs.fs_type));
    list.retain(|fs| {
        (cli.types.is_empty() || has(&cli.types, fs))
            && !has(&cli.exclude_types, fs)
            && (cli.kind.is_empty() || cli.kind.contains(&fs.kind))
            && (cli.bus.is_empty()
                || fs
                    .devices
                    .iter()
                    .any(|d| cli.bus.iter().any(|b| b.eq_ignore_ascii_case(&d.bus))))
    });
}

fn sort(list: &mut [FileSystem], key: SortKey, reverse: bool) {
    let text = |a: &str, b: &str| a.to_lowercase().cmp(&b.to_lowercase());
    let pct = |fs: &FileSystem| fs.use_percent().unwrap_or(-1.0);
    list.sort_by(|a, b| {
        let ord = match key {
            SortKey::Mount => Ordering::Equal,
            SortKey::Label => text(&a.label, &b.label),
            SortKey::Fs => text(&a.fs_type, &b.fs_type),
            SortKey::Kind => a.kind.as_str().cmp(b.kind.as_str()),
            SortKey::Bus => a.buses().cmp(&b.buses()),
            SortKey::Size => b.total.cmp(&a.total),
            SortKey::Used => b.used().cmp(&a.used()),
            SortKey::Avail => b.avail.cmp(&a.avail),
            SortKey::Use => pct(b).total_cmp(&pct(a)),
        };
        ord.then_with(|| text(a.mount(), b.mount()))
    });
    if reverse {
        list.reverse();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::sample;

    fn mounts(list: &[FileSystem]) -> Vec<&str> {
        list.iter().map(|f| f.mount()).collect()
    }

    #[test]
    fn sorts() {
        let mut list = vec![
            sample("D:\\", "NTFS", 100, 90),
            sample("c:\\", "exFAT", 300, 30),
            sample("E:\\", "NTFS", 200, 20),
        ];
        sort(&mut list, SortKey::Mount, false);
        assert_eq!(mounts(&list), ["c:\\", "D:\\", "E:\\"]);
        sort(&mut list, SortKey::Size, false);
        assert_eq!(mounts(&list), ["c:\\", "E:\\", "D:\\"]);
        sort(&mut list, SortKey::Use, true);
        assert_eq!(mounts(&list), ["D:\\", "E:\\", "c:\\"]);
        sort(&mut list, SortKey::Fs, false);
        assert_eq!(mounts(&list), ["c:\\", "D:\\", "E:\\"]);
    }

    #[test]
    fn filters_by_type_and_kind() {
        let mut usb = sample("F:\\", "FAT32", 10, 5);
        usb.kind = Kind::Removable;
        usb.devices = vec![fs::Device {
            name: "PhysicalDrive3".into(),
            bus: "usb".into(),
            ..Default::default()
        }];
        let base = vec![
            sample("C:\\", "NTFS", 10, 5),
            sample("D:\\", "ReFS", 10, 5),
            usb,
        ];

        let mut list = base.clone();
        filter(&mut list, &Cli::parse_from(["fsls", "-t", "ntfs,refs"]));
        assert_eq!(mounts(&list), ["C:\\", "D:\\"]);

        let mut list = base.clone();
        filter(
            &mut list,
            &Cli::parse_from(["fsls", "-x", "NTFS", "-k", "removable"]),
        );
        assert_eq!(mounts(&list), ["F:\\"]);

        let mut list = base.clone();
        filter(&mut list, &Cli::parse_from(["fsls", "--bus", "nvme,USB"]));
        assert_eq!(mounts(&list), ["F:\\"]);
    }

    #[test]
    fn cli_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        assert_eq!(Cli::parse_from(["fsls"]).timeout, Duration::from_secs(2));
        assert_eq!(
            Cli::parse_from(["fsls", "--timeout", "0.5"]).timeout,
            Duration::from_millis(500)
        );
        assert!(Cli::try_parse_from(["fsls", "--timeout", "-1"]).is_err());
    }

    #[test]
    fn warns_about_degraded_arrays_and_hung_mounts() {
        let mut md = sample("/srv", "ext4", 10, 5);
        md.raid = Some("raid1".into());
        md.raid_health = Some(fs::RaidHealth {
            ok: false,
            text: "degraded 1/2 missing".into(),
        });
        let mut nfs = sample("/mnt/nfs", "nfs4", 0, 0);
        nfs.timed_out = true;
        let mut fine = sample("/", "ext4", 10, 5);
        fine.raid_health = Some(fs::RaidHealth {
            ok: true,
            text: "ok, check 10%".into(),
        });
        assert_eq!(
            warnings(&[md, nfs, fine], Duration::from_secs(2), None),
            [
                "/srv: raid1 array is degraded 1/2 missing",
                "/mnt/nfs: no response within 2.0s (dead network mount?)",
            ]
        );
    }

    #[test]
    fn warn_threshold() {
        let full = sample("C:\\", "NTFS", 100, 4); // 96%
        let half = sample("D:\\", "NTFS", 100, 50);
        let mut iso = sample("/media/cd", "iso9660", 100, 0);
        iso.kind = Kind::Cdrom;
        let mut squash = sample("/snap/x", "squashfs", 100, 0);
        squash.options = vec!["ro".into()];
        let list = [full, half, iso, squash];
        let t = Duration::from_secs(2);
        assert!(warnings(&list, t, None).is_empty());
        assert_eq!(
            warnings(&list, t, Some(90.0)),
            ["C:\\: 96% full (--warn=90)"]
        );
        assert_eq!(warnings(&list, t, Some(50.0)).len(), 2);

        assert_eq!(Cli::parse_from(["fsls", "--warn"]).warn, Some(90.0));
        assert_eq!(Cli::parse_from(["fsls", "--warn=75%"]).warn, Some(75.0));
        // require_equals: a following PATH is not taken as the percentage.
        let cli = Cli::parse_from(["fsls", "--warn", "/srv"]);
        assert_eq!((cli.warn, cli.paths.len()), (Some(90.0), 1));
        assert!(Cli::try_parse_from(["fsls", "--warn=150"]).is_err());
    }

    #[test]
    fn disks_subcommand() {
        let cli = Cli::parse_from(["fsls", "disks", "--json", "-a"]);
        assert_eq!(cli.command, Some(Command::Disks));
        assert!(cli.json && cli.all);
        assert!(Cli::parse_from(["fsls", "--si", "disks"]).si);
        assert!(Cli::try_parse_from(["fsls", "disks", "--total"]).is_err());
        // Options before it turn subcommands off, so "disks" is then a PATH.
        let cli = Cli::parse_from(["fsls", "-d", "disks"]);
        assert_eq!((cli.command, cli.paths.len()), (None, 1));
        assert!(Cli::try_parse_from(["fsls", "--json", "-l"]).is_err());
        assert!(Cli::try_parse_from(["fsls", "disks", "--json", "-H"]).is_err());
    }
}
