//! Parser for `diskutil appleRAID list` (macOS software RAID).
//! Pure functions, so they are also compiled (and tested) on other platforms.

use crate::fs::RaidHealth;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RaidSet {
    /// The set's own disk: `disk4`.
    pub device: String,
    /// `mirror`, `stripe`, `concat`.
    pub level: String,
    /// `Online`, `Degraded`, `Offline`.
    pub status: String,
    /// (member device such as `disk2s2`, status such as `Online`, `Failed`, `Rebuilding 12%`)
    pub members: Vec<(String, String)>,
}

impl RaidSet {
    pub fn health(&self) -> RaidHealth {
        let ok = self.status.eq_ignore_ascii_case("online");
        let mut parts = vec![if ok {
            "ok".to_string()
        } else {
            self.status.to_lowercase()
        }];
        // Members that are neither in service nor rebuilding: "1 failed", "1 missing/damaged".
        let mut bad: Vec<(String, usize)> = Vec::new();
        for (_, s) in &self.members {
            if s.eq_ignore_ascii_case("online") || s.contains('%') {
                continue;
            }
            let s = s.to_lowercase();
            match bad.iter_mut().find(|(b, _)| *b == s) {
                Some((_, n)) => *n += 1,
                None => bad.push((s, 1)),
            }
        }
        parts.extend(bad.into_iter().map(|(s, n)| format!("{n} {s}")));
        if let Some((_, s)) = self.members.iter().find(|(_, s)| s.contains('%')) {
            parts.push(s.to_lowercase());
        }
        RaidHealth {
            ok,
            text: parts.join(", "),
        }
    }
}

/// ```text
/// Name:                 fslsraid
/// Type:                 Mirror
/// Status:               Offline
/// Device Node:          -
/// -------------------------------------------------------------------------------
/// #  DevNode   UUID                                  Status     Size
/// 0  disk5s1   00000000-0000-4000-8000-000000000002  Online     536805376
/// -  -none-    00000000-0000-4000-8000-000000000003  Missing/Damaged
/// ```
pub fn parse(text: &str) -> Vec<RaidSet> {
    let mut sets: Vec<RaidSet> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Name:") {
            sets.push(RaidSet::default());
            continue;
        }
        let Some(set) = sets.last_mut() else {
            continue;
        };
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            match key.trim() {
                "Type" => set.level = value.to_lowercase(),
                "Status" => set.status = value.to_string(),
                "Device Node" => set.device = value.to_string(),
                _ => {}
            }
            continue;
        }
        // Member rows: index ("-" for a missing member), device ("-none-" when missing),
        // UUID, status..., size...
        let t: Vec<&str> = line.split_whitespace().collect();
        let index = t
            .first()
            .is_some_and(|i| *i == "-" || i.parse::<u32>().is_ok());
        if t.len() >= 4 && index && is_uuid(t[2]) {
            let status: Vec<&str> = t[3..]
                .iter()
                .take_while(|s| s.parse::<f64>().is_err() && !s.starts_with('(') && !is_unit(s))
                .copied()
                .collect();
            set.members.push((t[1].to_string(), status.join(" ")));
        }
    }
    sets
}

fn is_unit(s: &str) -> bool {
    matches!(
        s,
        "B" | "KB" | "MB" | "GB" | "TB" | "PB" | "Bytes" | "Bytes)"
    )
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "\
AppleRAID sets (2 found)
===============================================================================
Name:                 Mirror
Unique ID:            00000000-0000-4000-8000-000000000011
Type:                 Mirror
Status:               Degraded
Size:                 499.8 GB (499763838976 Bytes)
Rebuild:              manual
Device Node:          disk4
-------------------------------------------------------------------------------
#  DevNode   UUID                                  Status     Size
-------------------------------------------------------------------------------
0  disk2s2   00000000-0000-4000-8000-000000000012  Online     499763838976
1  disk3s2   00000000-0000-4000-8000-000000000013  Rebuilding 37%  499763838976
2  -         00000000-0000-4000-8000-000000000014  Failed     499.8 GB (499763838976 Bytes)
===============================================================================
Name:                 Fast
Unique ID:            00000000-0000-4000-8000-000000000021
Type:                 Stripe
Status:               Online
Size:                 1.0 TB (999527677952 Bytes)
Rebuild:              manual
Device Node:          disk7
-------------------------------------------------------------------------------
#  DevNode   UUID                                  Status     Size
-------------------------------------------------------------------------------
0  disk5s2   00000000-0000-4000-8000-000000000022  Online     499763838976
1  disk6s2   00000000-0000-4000-8000-000000000023  Online     499763838976
===============================================================================
";

    #[test]
    fn parses_sets_and_members() {
        let sets = parse(LIST);
        assert_eq!(sets.len(), 2);
        assert_eq!(
            (sets[0].device.as_str(), sets[0].level.as_str()),
            ("disk4", "mirror")
        );
        assert_eq!(
            sets[0].members,
            [
                ("disk2s2".to_string(), "Online".to_string()),
                ("disk3s2".to_string(), "Rebuilding 37%".to_string()),
                ("-".to_string(), "Failed".to_string()),
            ]
        );
        assert_eq!(
            sets[0].health(),
            RaidHealth {
                ok: false,
                text: "degraded, 1 failed, rebuilding 37%".into()
            }
        );
        assert_eq!(
            sets[1].health(),
            RaidHealth {
                ok: true,
                text: "ok".into()
            }
        );
        assert!(parse("No AppleRAID sets found\n").is_empty());
    }

    /// macOS output format (identifiers replaced): a RAM-disk mirror, then the same set after
    /// one member's disk was detached (the set went Offline rather than Degraded).
    const REAL_ONLINE: &str = "\
AppleRAID sets (1 found)
===============================================================================
Name:                 fslsraid
Unique ID:            00000000-0000-4000-8000-000000000001
Type:                 Mirror
Status:               Online
Size:                 536.8 MB (536805376 Bytes)
Rebuild:              manual
Device Node:          disk7
-------------------------------------------------------------------------------
#  DevNode   UUID                                  Status     Size
-------------------------------------------------------------------------------
0  disk5s1   00000000-0000-4000-8000-000000000002  Online     536805376
1  disk6s1   00000000-0000-4000-8000-000000000003  Online     536805376
===============================================================================
";

    const REAL_OFFLINE: &str = "\
AppleRAID sets (1 found)
===============================================================================
Name:                 fslsraid
Unique ID:            00000000-0000-4000-8000-000000000001
Type:                 Mirror
Status:               Offline
Size:                 536.8 MB (536805376 Bytes)
Rebuild:              manual
Device Node:          -
-------------------------------------------------------------------------------
#  DevNode   UUID                                  Status     Size
-------------------------------------------------------------------------------
0  disk5s1   00000000-0000-4000-8000-000000000002  Online     536805376
-  -none-    00000000-0000-4000-8000-000000000003  Missing/Damaged
===============================================================================
";

    #[test]
    fn parses_real_macos_output() {
        let online = &parse(REAL_ONLINE)[0];
        assert_eq!(
            (online.device.as_str(), online.level.as_str()),
            ("disk7", "mirror")
        );
        assert_eq!(online.members.len(), 2);
        assert_eq!(
            online.health(),
            RaidHealth {
                ok: true,
                text: "ok".into()
            }
        );

        let offline = &parse(REAL_OFFLINE)[0];
        assert_eq!(offline.device, "-");
        assert_eq!(
            offline.members,
            [
                ("disk5s1".to_string(), "Online".to_string()),
                ("-none-".to_string(), "Missing/Damaged".to_string()),
            ]
        );
        assert_eq!(
            offline.health(),
            RaidHealth {
                ok: false,
                text: "offline, 1 missing/damaged".into()
            }
        );
    }

    #[test]
    fn never_panics_on_malformed_input() {
        for s in crate::testutil::inputs(REAL_OFFLINE, 3000) {
            for set in parse(&s) {
                let _ = set.health();
            }
        }
    }
}
