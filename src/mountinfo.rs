//! Parsing for Linux `/proc/self/mountinfo` and udev `/dev/disk/by-*` link names.
//! Pure functions, so they are also compiled (and tested) on other platforms.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// `major:minor` of the file system; shared by bind mounts and btrfs subvolumes.
    pub dev: String,
    pub mount: String,
    pub options: Vec<String>,
    pub fs_type: String,
    pub source: String,
}

pub fn parse(text: &str) -> Vec<Mount> {
    text.lines().filter_map(parse_line).collect()
}

/// `id parent major:minor root mount options [optional fields...] - fstype source superoptions`
fn parse_line(line: &str) -> Option<Mount> {
    let f: Vec<&str> = line.split(' ').collect();
    let sep = 6 + f.iter().skip(6).position(|&s| s == "-")?;
    Some(Mount {
        dev: f[2].to_string(),
        mount: unescape_octal(f[4]),
        options: f[5].split(',').map(String::from).collect(),
        fs_type: f.get(sep + 1)?.to_string(),
        source: unescape_octal(f.get(sep + 2)?),
    })
}

/// The kernel escapes space, tab, newline and backslash as `\NNN` octal.
fn unescape_octal(s: &str) -> String {
    unescape(s, |b| {
        if b.len() < 4 || !b[1..4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            return None;
        }
        // Up to \777 = 511 is syntactically octal but not a byte; leave those as text.
        let v = u32::from(b[1] - b'0') * 64 + u32::from(b[2] - b'0') * 8 + u32::from(b[3] - b'0');
        u8::try_from(v).ok().map(|v| (v, 4))
    })
}

/// udev escapes unsafe characters in `/dev/disk/by-label` names as `\xNN`.
pub fn unescape_udev(s: &str) -> String {
    unescape(s, |b| {
        if b.len() >= 4 && b[1] == b'x' {
            let hex = std::str::from_utf8(&b[2..4]).ok()?;
            u8::from_str_radix(hex, 16).ok().map(|v| (v, 4))
        } else {
            None
        }
    })
}

/// Replace escapes starting at `\`; `decode` gets the remaining bytes and returns (byte, length).
fn unescape(s: &str, decode: impl Fn(&[u8]) -> Option<(u8, usize)>) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && let Some((v, len)) = decode(&b[i..])
        {
            out.push(v);
            i += len;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw,errors=remount-ro
23 22 0:21 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw
40 22 8:2 /srv/data /mnt/My\\040Data rw,relatime shared:1 master:3 - ext4 /dev/sda2 rw
41 22 0:45 / /net rw - nfs4 server:/export\\134x rw,vers=4.2
bogus line";

    #[test]
    fn parses_mountinfo() {
        let m = parse(SAMPLE);
        assert_eq!(m.len(), 4);
        assert_eq!(
            m[0],
            Mount {
                dev: "8:2".into(),
                mount: "/".into(),
                options: vec!["rw".into(), "relatime".into()],
                fs_type: "ext4".into(),
                source: "/dev/sda2".into(),
            }
        );
        assert_eq!(m[1].fs_type, "proc");
        assert_eq!(m[2].mount, "/mnt/My Data");
        assert_eq!(m[2].dev, m[0].dev, "bind mount shares the device");
        assert_eq!(m[3].source, "server:/export\\x");
    }

    #[test]
    fn unescapes() {
        assert_eq!(unescape_udev(r"My\x20USB\x2fStick"), "My USB/Stick");
        assert_eq!(unescape_udev(r"trailing\x2"), r"trailing\x2");
        assert_eq!(unescape_octal(r"a\011b\9xy\"), "a\tb\\9xy\\");
        // Octal above a byte (\777 = 511) used to overflow; it is left as text.
        assert_eq!(unescape_octal(r"x\777y\377"), "x\\777y\u{fffd}");
    }

    #[test]
    fn never_panics_on_malformed_input() {
        for s in crate::testutil::inputs(SAMPLE, 3000) {
            let _ = parse(&s);
            let _ = unescape_udev(&s);
            let _ = unescape_octal(&s);
        }
    }
}
