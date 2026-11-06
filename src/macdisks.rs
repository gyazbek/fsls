//! `fsls disks` on macOS: assemble disks from `diskutil list -plist`.
//! Pure function of its inputs, so it is also compiled (and tested) on other platforms.

use crate::appleraid::RaidSet;
use crate::fs::{Device, Disk, FileSystem, Part, Usage};
use crate::parttype;
use crate::plist::Value;

/// `list`: parsed `diskutil list -plist`. `device(id)`: bus/model/size of a whole disk.
/// APFS containers and AppleRAID sets are synthesized disks: they are not listed as disks but
/// as what their member partitions are used by.
pub fn assemble(
    list: &Value,
    raid: &[RaidSet],
    fss: &[FileSystem],
    device: impl Fn(&str) -> Device,
    all: bool,
) -> Vec<Disk> {
    let entries = list
        .get("AllDisksAndPartitions")
        .map(Value::array)
        .unwrap_or_default();

    // APFS container -> its physical stores (`disk0s2`).
    let containers: Vec<(&str, Vec<&str>)> = entries
        .iter()
        .filter(|e| e.get("APFSVolumes").is_some() || e.get("APFSPhysicalStores").is_some())
        .map(|e| {
            let stores = e
                .get("APFSPhysicalStores")
                .map(Value::array)
                .unwrap_or_default()
                .iter()
                .map(|s| match s.str_of("DeviceIdentifier") {
                    "" => s.str_of("APFSPhysicalStore"),
                    id => id,
                })
                .collect();
            (e.str_of("DeviceIdentifier"), stores)
        })
        .collect();

    let usage = |id: &str, node: &Value| -> Usage {
        let dev = format!("/dev/{id}");
        let mut u = match fss
            .iter()
            .find(|f| f.source.as_deref() == Some(dev.as_str()))
        {
            Some(fs) => Usage::of(fs),
            None => Usage {
                label: node.str_of("VolumeName").to_string(),
                mounts: Some(node.str_of("MountPoint"))
                    .filter(|m| !m.is_empty())
                    .map(String::from)
                    .into_iter()
                    .collect(),
                ..Usage::default()
            },
        };
        for (cid, stores) in &containers {
            if stores.contains(&id) {
                u.used_by.push(with_mounts(
                    format!("{cid} (apfs container)"),
                    mounts_in(fss, |f| f.pool.as_ref().is_some_and(|p| p.name == *cid)),
                ));
            }
        }
        for set in raid {
            if set.members.iter().any(|(m, _)| m == id) {
                let text = if set.device == "-" || set.device.is_empty() {
                    format!("{} set ({})", set.level, set.status.to_lowercase())
                } else {
                    format!("{} ({})", set.device, set.level)
                };
                let prefix = format!("/dev/{}", set.device);
                let on_set = |f: &FileSystem| {
                    f.source
                        .as_deref()
                        .is_some_and(|s| s == prefix || s.starts_with(&format!("{prefix}s")))
                };
                u.used_by.push(with_mounts(text, mounts_in(fss, on_set)));
            }
        }
        u
    };

    let mut out = Vec::new();
    for e in entries {
        let id = e.str_of("DeviceIdentifier");
        let synthesized =
            containers.iter().any(|(c, _)| *c == id) || raid.iter().any(|s| s.device == id);
        if id.is_empty() || synthesized {
            continue;
        }
        let dev = device(id);
        if !all && dev.bus == "image" {
            continue; // mounted .dmg files and RAM disks: only with --all
        }
        let parts: Vec<Part> = e
            .get("Partitions")
            .map(Value::array)
            .unwrap_or_default()
            .iter()
            .map(|p| {
                let pid = p.str_of("DeviceIdentifier");
                Part {
                    name: pid.to_string(),
                    kind: content_name(p.str_of("Content")),
                    size: p.get("Size").and_then(Value::int).unwrap_or(0).max(0) as u64,
                    usage: usage(pid, p),
                }
            })
            .collect();
        let table = match e.str_of("Content") {
            "GUID_partition_scheme" => "gpt",
            "FDisk_partition_scheme" => "mbr",
            "Apple_partition_scheme" => "apm",
            _ => "",
        };
        out.push(Disk {
            usage: if parts.is_empty() {
                usage(id, e)
            } else {
                Usage::default()
            },
            table: table.into(),
            parts,
            device: Device {
                size: match dev.size {
                    0 => e.get("Size").and_then(Value::int).unwrap_or(0).max(0) as u64,
                    s => s,
                },
                ..dev
            },
        });
    }
    out
}

/// Where the file systems matching `f` are mounted: the visible mount points, or
/// `3 system volumes` when only hidden ones (Preboot, xarts, ...) are mounted.
fn mounts_in(fss: &[FileSystem], f: impl Fn(&FileSystem) -> bool) -> String {
    let matching: Vec<&FileSystem> = fss.iter().filter(|x| f(x)).collect();
    let visible: Vec<&str> = matching
        .iter()
        .filter(|x| !x.hidden)
        .flat_map(|x| x.mounts.iter().map(String::as_str))
        .collect();
    match (visible.is_empty(), matching.len()) {
        (false, _) => visible.join(", "),
        (true, 0) => String::new(),
        (true, 1) => "1 system volume".into(),
        (true, n) => format!("{n} system volumes"),
    }
}

fn with_mounts(text: String, mounts: String) -> String {
    if mounts.is_empty() {
        text
    } else {
        format!("{text}: {mounts}")
    }
}

/// diskutil's partition `Content` (`Apple_APFS`, `EFI`, `Microsoft Basic Data`, or a GPT GUID).
fn content_name(c: &str) -> String {
    let known = match c {
        "Apple_APFS" => "APFS",
        "Apple_APFS_ISC" => "APFS (iSC)",
        "Apple_APFS_Recovery" => "APFS recovery",
        "Apple_HFS" => "HFS+",
        "Apple_Boot" => "Apple boot",
        "Apple_RAID" => "AppleRAID",
        "Apple_CoreStorage" => "CoreStorage",
        "EFI" => "EFI",
        "Microsoft Basic Data" => "Basic data",
        "Microsoft Reserved" => "MS reserved",
        "Windows_NTFS" => "NTFS",
        "DOS_FAT_32" => "FAT32",
        "Linux" => "Linux",
        "Linux_Swap" => "Linux swap",
        _ => return parttype::gpt(c).map_or_else(|| c.to_string(), String::from),
    };
    known.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appleraid;
    use crate::fs::sample;
    use crate::plist;

    const LIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>AllDisks</key>
	<array><string>disk0</string><string>disk0s1</string></array>
	<key>AllDisksAndPartitions</key>
	<array>
		<dict>
			<key>Content</key><string>GUID_partition_scheme</string>
			<key>DeviceIdentifier</key><string>disk0</string>
			<key>OSInternal</key><false/>
			<key>Partitions</key>
			<array>
				<dict><key>Content</key><string>Apple_APFS_ISC</string><key>DeviceIdentifier</key><string>disk0s1</string><key>Size</key><integer>524288000</integer></dict>
				<dict><key>Content</key><string>Apple_APFS</string><key>DeviceIdentifier</key><string>disk0s2</string><key>Size</key><integer>994662584320</integer></dict>
				<dict><key>Content</key><string>Apple_APFS_Recovery</string><key>DeviceIdentifier</key><string>disk0s3</string><key>Size</key><integer>5369810944</integer></dict>
			</array>
			<key>Size</key><integer>1000555581440</integer>
		</dict>
		<dict>
			<key>APFSPhysicalStores</key>
			<array><dict><key>DeviceIdentifier</key><string>disk0s2</string></dict></array>
			<key>APFSVolumes</key>
			<array><dict><key>DeviceIdentifier</key><string>disk3s1</string><key>MountPoint</key><string>/</string></dict></array>
			<key>Content</key><string>EF57347C-0000-11AA-AA11-00306543ECAC</string>
			<key>DeviceIdentifier</key><string>disk3</string>
			<key>Size</key><integer>994662584320</integer>
		</dict>
		<dict>
			<key>APFSPhysicalStores</key>
			<array><dict><key>DeviceIdentifier</key><string>disk0s1</string></dict></array>
			<key>APFSVolumes</key>
			<array><dict><key>DeviceIdentifier</key><string>disk1s1</string></dict></array>
			<key>DeviceIdentifier</key><string>disk1</string>
		</dict>
		<dict>
			<key>Content</key><string>FDisk_partition_scheme</string>
			<key>DeviceIdentifier</key><string>disk4</string>
			<key>Partitions</key>
			<array>
				<dict><key>Content</key><string>Windows_NTFS</string><key>DeviceIdentifier</key><string>disk4s1</string><key>MountPoint</key><string>/Volumes/RECORDS</string><key>Size</key><integer>34359738368</integer><key>VolumeName</key><string>RECORDS</string></dict>
			</array>
			<key>Size</key><integer>61530439680</integer>
		</dict>
		<dict>
			<key>Content</key><string></string>
			<key>DeviceIdentifier</key><string>disk5</string>
			<key>Size</key><integer>536870912</integer>
		</dict>
		<dict>
			<key>Content</key><string>GUID_partition_scheme</string>
			<key>DeviceIdentifier</key><string>disk6</string>
			<key>Partitions</key>
			<array><dict><key>Content</key><string>Apple_RAID</string><key>DeviceIdentifier</key><string>disk6s1</string><key>Size</key><integer>536805376</integer></dict></array>
			<key>Size</key><integer>536870912</integer>
		</dict>
		<dict>
			<key>Content</key><string>Apple_HFS</string>
			<key>DeviceIdentifier</key><string>disk8</string>
			<key>MountPoint</key><string>/Volumes/fslsraid</string>
			<key>Size</key><integer>536805376</integer>
			<key>VolumeName</key><string>fslsraid</string>
		</dict>
	</array>
</dict>
</plist>"#;

    const RAID: &str = "\
Name:                 fslsraid
Unique ID:            00000000-0000-4000-8000-000000000031
Type:                 Mirror
Status:               Online
Device Node:          disk8
-------------------------------------------------------------------------------
#  DevNode   UUID                                  Status     Size
0  disk6s1   00000000-0000-4000-8000-000000000032  Online     536805376
";

    fn fs_at(
        mount: &str,
        source: &str,
        fs_type: &str,
        pool: Option<&str>,
        hidden: bool,
    ) -> FileSystem {
        let mut f = sample(mount, fs_type, 10, 5);
        f.source = Some(source.into());
        f.pool = pool.map(|p| crate::fs::Pool::apfs(p.into()));
        f.hidden = hidden;
        f
    }

    #[test]
    fn assembles_disks() {
        let list = plist::parse(LIST).expect("fixture parses");
        let raid = appleraid::parse(RAID);
        let fss = [
            fs_at("/", "/dev/disk3s1s1", "apfs", Some("disk3"), false),
            fs_at(
                "/System/Volumes/Data",
                "/dev/disk3s5",
                "apfs",
                Some("disk3"),
                false,
            ),
            fs_at(
                "/System/Volumes/VM",
                "/dev/disk3s6",
                "apfs",
                Some("disk3"),
                true,
            ),
            fs_at("/Volumes/RECORDS", "/dev/disk4s1", "ntfs", None, false),
            fs_at("/Volumes/fslsraid", "/dev/disk8", "hfs", None, false),
            fs_at(
                "/System/Volumes/xarts",
                "/dev/disk1s2",
                "apfs",
                Some("disk1"),
                true,
            ),
            fs_at(
                "/System/Volumes/iSCPreboot",
                "/dev/disk1s1",
                "apfs",
                Some("disk1"),
                true,
            ),
            fs_at(
                "/System/Volumes/Hardware",
                "/dev/disk1s3",
                "apfs",
                Some("disk1"),
                true,
            ),
        ];
        let device = |id: &str| Device {
            name: id.to_string(),
            bus: if id == "disk5" || id == "disk6" {
                "image".into()
            } else {
                "usb".into()
            },
            ..Device::default()
        };

        let disks = assemble(&list, &raid, &fss, device, false);
        let names: Vec<&str> = disks.iter().map(|d| d.device.name.as_str()).collect();
        assert_eq!(
            names,
            ["disk0", "disk4"],
            "container, RAID set and images hidden"
        );

        let d0 = &disks[0];
        assert_eq!((d0.table.as_str(), d0.device.size), ("gpt", 1000555581440));
        let kinds: Vec<&str> = d0.parts.iter().map(|p| p.kind.as_str()).collect();
        assert_eq!(kinds, ["APFS (iSC)", "APFS", "APFS recovery"]);
        assert_eq!(
            d0.parts[1].usage.used_by,
            ["disk3 (apfs container): /, /System/Volumes/Data"]
        );
        assert_eq!(
            d0.parts[0].usage.used_by,
            ["disk1 (apfs container): 3 system volumes"]
        );

        let d4 = &disks[1];
        assert_eq!(d4.table, "mbr");
        let p = &d4.parts[0];
        assert_eq!(
            (p.kind.as_str(), p.usage.fs_type.as_str()),
            ("NTFS", "ntfs")
        );
        assert_eq!(p.usage.mounts, ["/Volumes/RECORDS"]);

        let all = assemble(&list, &raid, &fss, device, true);
        let names: Vec<&str> = all.iter().map(|d| d.device.name.as_str()).collect();
        assert_eq!(names, ["disk0", "disk4", "disk5", "disk6"]);
        assert!(
            all[2].usage.is_empty() && all[2].parts.is_empty(),
            "disk5 is blank"
        );
        assert_eq!(
            all[3].parts[0].usage.used_by,
            ["disk8 (mirror): /Volumes/fslsraid"]
        );
    }
}
