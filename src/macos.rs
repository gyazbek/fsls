//! File-system enumeration via BSD `getmntinfo` / `statfs`; disks via `diskutil info -plist`.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::appleraid::{self, RaidSet};
use crate::deadline;
use crate::fs::{Device, Disk, FileSystem, Inodes, Kind, Pool, RaidHealth, flag_names, multi_disk};
use crate::macdisks;
use crate::plist;

const MNT_RDONLY: u32 = 0x0000_0001;
const MNT_REMOVABLE: u32 = 0x0000_0200;
const MNT_LOCAL: u32 = 0x0000_1000;
const MNT_DONTBROWSE: u32 = 0x0010_0000;

/// `MNT_*` flags from `<sys/mount.h>`, in display order (names follow `mount(8)`).
const FLAGS: &[(u32, &str)] = &[
    (MNT_RDONLY, "ro"),
    (MNT_LOCAL, "local"),
    (0x0080_0000, "journaled"), // MNT_JOURNALED
    (0x0000_0008, "nosuid"),    // MNT_NOSUID
    (0x0000_0010, "nodev"),     // MNT_NODEV
    (0x0000_0004, "noexec"),    // MNT_NOEXEC
    (0x1000_0000, "noatime"),   // MNT_NOATIME
    (MNT_DONTBROWSE, "nobrowse"),
    (0x0000_0400, "quarantine"),  // MNT_QUARANTINE
    (0x0040_0000, "automounted"), // MNT_AUTOMOUNTED
    (0x0020_0000, "noowners"),    // MNT_IGNORE_OWNERSHIP
    (0x0000_0080, "protect"),     // MNT_CPROTECT
    (0x4000_0000, "snapshot"),    // MNT_SNAPSHOT
    (0x0000_4000, "rootfs"),      // MNT_ROOTFS
    (MNT_REMOVABLE, "removable"),
];

/// The APFS data volume is "nobrowse" but holds all user data, so keep it visible.
const DATA_VOLUME: &str = "/System/Volumes/Data";

pub fn same_path(a: &str, b: &str) -> bool {
    a == b
}

/// Terminals here understand ANSI colour codes as-is.
pub fn enable_ansi() -> bool {
    true
}

pub fn list(timeout: Duration) -> Vec<FileSystem> {
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    // MNT_NOWAIT: never block on a hung network mount just to enumerate.
    let n = unsafe { libc::getmntinfo(&mut buf, libc::MNT_NOWAIT) };
    if n <= 0 || buf.is_null() {
        return Vec::new();
    }
    // The buffer is owned by libc and stays valid until the next getmntinfo call.
    let cached: Vec<libc::statfs> = unsafe { std::slice::from_raw_parts(buf, n as usize) }.to_vec();

    // Fresh numbers; statfs blocks on a dead network mount, so bound it.
    let fresh = deadline::run_all(
        cached.iter().map(|c| {
            let mount = cstr(&c.f_mntonname);
            move || statfs(&mount).ok().map(|s| (s, name_max(&mount)))
        }),
        timeout,
    );

    let disks = Diskutil::default();
    cached
        .iter()
        .zip(fresh)
        .map(|(c, f)| {
            let mut fs = match f {
                Some(Some((s, name_max))) => build(&s, true, false, name_max),
                Some(None) => build(c, false, false, None),
                None => build(c, false, true, None),
            };
            if let Some(id) = fs.source.as_deref().and_then(|s| s.strip_prefix("/dev/")) {
                // APFS volumes share their container's space: disk3s1s1, disk3s5 -> disk3.
                if fs.fs_type == "apfs" {
                    fs.pool = whole_disk(id).map(Pool::apfs);
                }
                let (devices, raid) = disks.physical(id);
                fs.devices = devices;
                match raid {
                    Some((level, health)) => {
                        (fs.raid, fs.raid_health) = (Some(level), Some(health))
                    }
                    None => fs.raid = multi_disk(&fs.devices),
                }
            } else if fs.fs_type == "zfs" {
                // OpenZFS: datasets (`tank/data`) share their pool's free space.
                fs.pool = fs.source.as_deref().and_then(Pool::zfs);
            }
            fs
        })
        .collect()
}

/// FileVault status, read on demand (only when displayed): one `diskutil info` per volume.
pub fn add_encryption(list: &mut [FileSystem], timeout: Duration) {
    let ids: Vec<(usize, String)> = list
        .iter()
        .enumerate()
        .filter(|(_, fs)| fs.ready && !fs.hidden)
        .filter_map(|(i, fs)| Some((i, fs.source.as_deref()?.strip_prefix("/dev/")?.to_string())))
        .collect();
    let found = deadline::run_all(
        ids.iter().map(|(_, id)| {
            let id = id.clone();
            move || encryption(&Diskutil::run(&["info", "-plist", &id]))
        }),
        timeout,
    );
    for ((i, _), enc) in ids.iter().zip(found) {
        list[*i].encryption = enc.flatten();
    }
}

/// From `diskutil info -plist <volume>`: FileVault, or another kind of encrypted volume.
fn encryption(info: &str) -> Option<String> {
    let yes = |key| plist::value(info, key).as_deref() == Some("true");
    if yes("FileVault") {
        Some("filevault".into())
    } else if yes("Encryption") || yes("Encrypted") {
        Some("encrypted".into())
    } else {
        None
    }
}

pub fn mount_of(path: &Path) -> io::Result<String> {
    statfs_bytes(path.as_os_str().as_bytes()).map(|s| cstr(&s.f_mntonname))
}

/// `fsls disks`: whole disks from `diskutil list -plist`, with AppleRAID sets resolved.
pub fn disks(list: &[FileSystem], all: bool) -> Vec<Disk> {
    let du = Diskutil::default();
    let Some(tree) = plist::parse(&Diskutil::run(&["list", "-plist"])) else {
        return Vec::new();
    };
    macdisks::assemble(
        &tree,
        du.raid_sets(),
        list,
        |id| du.device(id.to_string()),
        all,
    )
}

fn build(s: &libc::statfs, ready: bool, timed_out: bool, name_max: Option<u32>) -> FileSystem {
    let mount = cstr(&s.f_mntonname);
    let fs_type = cstr(&s.f_fstypename);
    let flags = s.f_flags;

    let kind = Kind::from_fs_type(&fs_type).unwrap_or(if flags & MNT_LOCAL == 0 {
        Kind::Network
    } else if flags & MNT_REMOVABLE != 0 {
        Kind::Removable
    } else {
        Kind::Fixed
    });

    let bs = s.f_bsize as u64;
    let total = s.f_blocks * bs;
    let hidden = (!ready && !timed_out)
        || (ready && total == 0)
        || kind == Kind::Virtual
        || (flags & MNT_DONTBROWSE != 0 && mount != DATA_VOLUME);

    FileSystem {
        // Finder names /Volumes/<X> after the volume label.
        label: mount
            .strip_prefix("/Volumes/")
            .filter(|l| !l.contains('/'))
            .unwrap_or_default()
            .to_string(),
        source: Some(cstr(&s.f_mntfromname)),
        max_name: name_max,
        mounts: vec![mount],
        fs_type,
        kind,
        ready,
        timed_out,
        hidden,
        total,
        free: s.f_bfree * bs,
        avail: s.f_bavail * bs,
        id: None,
        options: flag_names(flags, FLAGS),
        raid: None,
        raid_health: None,
        devices: Vec::new(),
        pool: None,
        // APFS reports f_files as used + free; 0 means the file system has no inode table.
        inodes: Inodes::new(s.f_files, s.f_ffree),
        encryption: None,
    }
}

/// Cached `diskutil` lookups (one process per disk, not per volume).
#[derive(Default)]
struct Diskutil {
    info: RefCell<HashMap<String, String>>,
    raid_sets: OnceCell<Vec<RaidSet>>,
}

impl Diskutil {
    fn run(args: &[&str]) -> String {
        Command::new("/usr/sbin/diskutil")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    fn info(&self, id: &str) -> String {
        if let Some(xml) = self.info.borrow().get(id) {
            return xml.clone();
        }
        let xml = Self::run(&["info", "-plist", id]);
        self.info.borrow_mut().insert(id.to_string(), xml.clone());
        xml
    }

    fn raid_sets(&self) -> &[RaidSet] {
        self.raid_sets
            .get_or_init(|| appleraid::parse(&Self::run(&["appleRAID", "list"])))
    }

    /// Physical disks under a volume (`disk3s1s1`) and its AppleRAID layout. An APFS volume
    /// lives in a synthesized container disk whose physical stores (`disk0s2`; more than one
    /// for a Fusion drive) sit on real disks, or on an AppleRAID set made of real disks.
    fn physical(&self, volume: &str) -> (Vec<Device>, Option<(String, RaidHealth)>) {
        let Some(whole) = whole_disk(volume) else {
            return (Vec::new(), None);
        };
        let mut stores = plist::values(&self.info(&whole), "APFSPhysicalStore");
        if stores.is_empty() {
            stores = plist::values(&self.info(volume), "APFSPhysicalStore");
        }
        if stores.is_empty() {
            stores.push(whole);
        }
        let mut disks: Vec<String> = Vec::new();
        for s in stores.iter().filter_map(|s| whole_disk(s)) {
            if !disks.contains(&s) {
                disks.push(s);
            }
        }

        let mut devices = Vec::new();
        let mut raid = None;
        for d in disks {
            let Some(set) = self.raid_sets().iter().find(|s| s.device == d) else {
                devices.push(self.device(d));
                continue;
            };
            raid = Some((set.level.clone(), set.health()));
            for (member, status) in &set.members {
                // A missing member shows as "-"; the set's health already reports it.
                let Some(m) = whole_disk(member) else {
                    continue;
                };
                let state = if status.eq_ignore_ascii_case("online") {
                    String::new()
                } else {
                    status.to_lowercase()
                };
                devices.push(Device {
                    state,
                    ..self.device(m)
                });
            }
        }
        (devices, raid)
    }

    fn device(&self, name: String) -> Device {
        let xml = self.info(&name);
        let get = |k: &str| plist::value(&xml, k).unwrap_or_default();
        let bus = bus_name(&get("BusProtocol"));
        let media = match (bus.as_str(), get("SolidState").as_str()) {
            ("image", _) => "",
            (_, "true") => "ssd",
            (_, "false") => "hdd",
            _ => "",
        };
        Device {
            name,
            bus,
            model: get("MediaName"),
            media: media.into(),
            size: get("Size").parse().unwrap_or(0),
            state: String::new(),
        }
    }
}

/// `disk3s1s1` -> `disk3`.
fn whole_disk(id: &str) -> Option<String> {
    let digits = id.strip_prefix("disk")?;
    let n = digits.len()
        - digits
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    (n > 0).then(|| format!("disk{}", &digits[..n]))
}

/// diskutil's `BusProtocol` in the short lowercase style used on other platforms.
fn bus_name(protocol: &str) -> String {
    match protocol {
        "PCI-Express" => "pcie",
        "Apple Fabric" => "fabric",
        "Disk Image" => "image",
        "Secure Digital" => "sd",
        "FireWire" => "1394",
        p => return p.to_lowercase().replace(' ', "-"),
    }
    .to_string()
}

fn statfs(path: &str) -> io::Result<libc::statfs> {
    statfs_bytes(path.as_bytes())
}

fn statfs_bytes(path: &[u8]) -> io::Result<libc::statfs> {
    let c = CString::new(path).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(s)
}

fn name_max(path: &str) -> Option<u32> {
    let c = CString::new(path).ok()?;
    let n = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_NAME_MAX) };
    (n > 0).then_some(n as u32)
}

fn cstr(chars: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_root() {
        let all = list(Duration::from_secs(10));
        let root = all.iter().find(|f| f.mount() == "/").expect("root listed");
        assert!(root.ready && root.total > 0 && !root.hidden);
        assert!(root.options.iter().any(|o| o == "local"));
        assert_eq!(mount_of(Path::new("/")).unwrap(), "/");
        assert!(mount_of(Path::new("/definitely/not/here/fsls")).is_err());
        assert!(
            !root.devices.is_empty() && !root.devices[0].bus.is_empty(),
            "{:?}",
            root.devices
        );
    }

    #[test]
    fn names() {
        assert_eq!(whole_disk("disk3s1s1").as_deref(), Some("disk3"));
        assert_eq!(whole_disk("disk12").as_deref(), Some("disk12"));
        assert_eq!(whole_disk("map auto_home"), None);
        assert_eq!(bus_name("USB"), "usb");
        assert_eq!(bus_name("Apple Fabric"), "fabric");
    }

    #[test]
    fn encryption_from_diskutil_info() {
        let info = |k: &str, v: &str| format!("<plist><dict><key>{k}</key><{v}/></dict></plist>");
        assert_eq!(
            encryption(&info("FileVault", "true")).as_deref(),
            Some("filevault")
        );
        assert_eq!(
            encryption(&info("Encryption", "true")).as_deref(),
            Some("encrypted")
        );
        assert_eq!(encryption(&info("FileVault", "false")), None);
        assert_eq!(encryption(""), None);
    }
}
