//! File-system enumeration from `/proc/self/mountinfo`, `statvfs` and sysfs.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::deadline;
use crate::fs::{
    Device, Disk, FileSystem, Inodes, Kind, Part, Pool, RaidHealth, Usage, multi_disk,
};
use crate::mountinfo::{self, Mount};
use crate::parttype;

pub fn same_path(a: &str, b: &str) -> bool {
    a == b
}

/// Terminals here understand ANSI colour codes as-is.
pub fn enable_ansi() -> bool {
    true
}

/// Encryption (LUKS) is already known from the device stack.
pub fn add_encryption(_list: &mut [FileSystem], _timeout: Duration) {}

pub fn list(timeout: Duration) -> Vec<FileSystem> {
    let labels = links_by_target("/dev/disk/by-label");
    let uuids = links_by_target("/dev/disk/by-uuid");

    // One entry per file system: bind mounts and btrfs subvolumes share a device id.
    let mut groups: Vec<(Mount, Vec<String>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for m in mounts() {
        if let Some(&i) = index.get(&m.dev) {
            groups[i].1.push(m.mount);
        } else {
            index.insert(m.dev.clone(), groups.len());
            let paths = vec![m.mount.clone()];
            groups.push((m, paths));
        }
    }

    // statvfs blocks forever on a dead hard-mounted NFS share or a hung FUSE daemon.
    let stats = deadline::run_all(
        groups.iter().map(|(_, paths)| {
            let path = paths[0].clone();
            move || statvfs(&path)
        }),
        timeout,
    );

    groups
        .into_iter()
        .zip(stats)
        .map(|((m, paths), st)| build(m, paths, st, &labels, &uuids))
        .collect()
}

/// The mount point of the file system containing `path` (longest matching prefix).
pub fn mount_of(path: &Path) -> io::Result<String> {
    let p = fs::canonicalize(path)?;
    mounts()
        .into_iter()
        .filter(|m| p.starts_with(&m.mount))
        // max_by_key keeps the LAST maximum: the mount stacked on top is the visible one.
        .max_by_key(|m| m.mount.len())
        .map(|m| m.mount)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no mount point found"))
}

fn mounts() -> Vec<Mount> {
    fs::read_to_string("/proc/self/mountinfo")
        .map(|t| mountinfo::parse(&t))
        .unwrap_or_default()
}

fn build(
    m: Mount,
    mounts: Vec<String>,
    stats: Option<Option<Stat>>,
    labels: &HashMap<PathBuf, String>,
    uuids: &HashMap<PathBuf, String>,
) -> FileSystem {
    let device = m
        .source
        .starts_with("/dev/")
        .then(|| fs::canonicalize(&m.source).ok())
        .flatten();
    let kind = Kind::from_fs_type(&m.fs_type).unwrap_or_else(|| match &device {
        Some(dev) => block_kind(dev),
        None if matches!(m.fs_type.as_str(), "zfs" | "overlay" | "btrfs") => Kind::Fixed,
        None => Kind::Unknown,
    });

    let timed_out = stats.is_none();
    let stats = stats.flatten();
    let ready = stats.is_some();
    let st = stats.unwrap_or_default();
    let (total, free, avail) = (st.total, st.free, st.avail);

    // A timed-out mount stays visible: the user needs to know it is hanging.
    let hidden = (!timed_out && (!ready || total == 0))
        || kind == Kind::Virtual
        // snap packages / AppImages: dozens of read-only images that are always 100% full
        || m.fs_type == "squashfs"
        // container layers, except a container's own root
        || (m.fs_type == "overlay" && mounts[0] != "/");

    let lookup = |map: &HashMap<PathBuf, String>| device.as_ref().and_then(|d| map.get(d)).cloned();
    let id = lookup(uuids);

    let sys = Sysfs {
        root: Path::new("/sys"),
    };
    let mut roots: Vec<String> = device
        .iter()
        .filter_map(|d| d.file_name()?.to_str().map(String::from))
        .collect();
    // A multi-device btrfs mounts from one member; sysfs lists them all.
    let mut profile = None;
    if m.fs_type == "btrfs"
        && let Some(uuid) = &id
    {
        let (members, p) = sys.btrfs(uuid);
        if !members.is_empty() {
            roots = members;
        }
        profile = p.filter(|p| p != "single");
    }
    let stack = sys.stack(&roots);
    let devices = stack.devices;
    let raid = stack.level.or(profile).or_else(|| multi_disk(&devices));
    // ZFS datasets share their pool's free space.
    let pool = (m.fs_type == "zfs")
        .then_some(m.source.as_str())
        .and_then(Pool::zfs);

    FileSystem {
        label: lookup(labels).unwrap_or_default(),
        id,
        source: (!m.source.is_empty() && m.source != "none").then_some(m.source),
        mounts,
        fs_type: m.fs_type,
        kind,
        ready,
        timed_out,
        hidden,
        total,
        free,
        avail,
        max_name: ready.then_some(st.max_name),
        options: m.options,
        raid,
        raid_health: stack.health,
        devices,
        pool,
        // btrfs and some FUSE file systems report 0: no fixed inode table.
        inodes: Inodes::new(st.files, st.files_free),
        encryption: stack.encryption,
    }
}

/// Block-device stack resolution against a sysfs root (`/sys`, or a fake tree in tests).
struct Sysfs<'a> {
    root: &'a Path,
}

/// What [`Sysfs::stack`] found below a file system.
struct Stack {
    devices: Vec<Device>,
    /// First md RAID level on the way down.
    level: Option<String>,
    /// Health of the first unhealthy md array, else of the first one.
    health: Option<RaidHealth>,
    /// `luks` / `dm-crypt` when a dm-crypt device is in the stack.
    encryption: Option<String>,
}

const BTRFS_PROFILES: &[&str] = &[
    "single", "dup", "raid0", "raid1", "raid1c3", "raid1c4", "raid10", "raid5", "raid6",
];

impl Sysfs<'_> {
    fn block(&self, name: &str) -> PathBuf {
        self.root.join("class/block").join(name)
    }

    /// Physical disks under these block devices, plus md RAID level and health.
    fn stack(&self, names: &[String]) -> Stack {
        let (mut disks, mut mds, mut crypt) = (Vec::new(), Vec::new(), None);
        for n in names {
            self.walk(n, "", &mut disks, &mut mds, &mut crypt, 0);
        }
        let healths: Vec<RaidHealth> = mds.iter().map(|md| self.md_health(md)).collect();
        Stack {
            devices: disks
                .iter()
                .map(|(d, state)| Device {
                    state: state.clone(),
                    ..self.device(d)
                })
                .collect(),
            level: mds
                .first()
                .map(|md| read_trim(&self.block(md).join("md/level"))),
            health: healths.iter().find(|h| !h.ok).or(healths.first()).cloned(),
            encryption: crypt,
        }
    }

    /// dm (LVM, LUKS) and md devices list what they are built from in `slaves/`.
    /// `state` is the md member state inherited from the array above (empty if in sync).
    fn walk(
        &self,
        name: &str,
        state: &str,
        disks: &mut Vec<(String, String)>,
        mds: &mut Vec<String>,
        crypt: &mut Option<String>,
        depth: u32,
    ) {
        if depth > 16 {
            return;
        }
        let dir = self.block(name);
        let is_md = dir.join("md/level").exists();
        if is_md && !mds.iter().any(|m| m == name) {
            mds.push(name.to_string());
        }
        // cryptsetup names its dm devices CRYPT-LUKS2-<uuid>-<name>, CRYPT-PLAIN-..., ...
        let dm_uuid = read_trim(&dir.join("dm/uuid"));
        if crypt.is_none() && dm_uuid.starts_with("CRYPT-") {
            *crypt = Some(
                if dm_uuid.starts_with("CRYPT-LUKS") {
                    "luks"
                } else {
                    "dm-crypt"
                }
                .into(),
            );
        }
        let slaves = dir_names(&dir.join("slaves"));
        if slaves.is_empty() {
            let disk = self.whole_disk(name);
            if !disks.iter().any(|(d, _)| *d == disk) {
                disks.push((disk, state.to_string()));
            }
        }
        for s in slaves {
            let member = if is_md {
                // e.g. "in_sync", "faulty", "spare", "in_sync,write_mostly"
                let st = read_trim(&dir.join(format!("md/dev-{s}/state")));
                if st.split(',').any(|f| f == "in_sync") {
                    String::new()
                } else if st == "spare" && read_trim(&dir.join("md/sync_action")) == "recover" {
                    // md marks the disk being rebuilt onto as a spare until it is in sync.
                    "rebuilding".to_string()
                } else {
                    st
                }
            } else {
                state.to_string()
            };
            self.walk(&s, &member, disks, mds, crypt, depth + 1);
        }
    }

    /// `/sys/block/mdX/md/*`: readable without root, unlike `mdadm --detail`.
    fn md_health(&self, md: &str) -> RaidHealth {
        let dir = self.block(md).join("md");
        let state = read_trim(&dir.join("array_state"));
        let missing: u32 = read_trim(&dir.join("degraded")).parse().unwrap_or(0);
        let members = read_trim(&dir.join("raid_disks"));
        let action = read_trim(&dir.join("sync_action"));
        let running = matches!(
            state.as_str(),
            "clean" | "active" | "active-idle" | "write-pending" | "read-auto" | "readonly"
        );

        let mut parts = vec![if !running {
            if state.is_empty() {
                "unknown".to_string()
            } else {
                state
            }
        } else if missing > 0 {
            format!("degraded {missing}/{members} missing")
        } else {
            "ok".to_string()
        }];
        // sync_action says "recover" as soon as a member fails, even with nothing to rebuild
        // onto; only report it once there is progress.
        if action != "idle"
            && let Some(p) = sync_percent(&read_trim(&dir.join("sync_completed")))
        {
            parts.push(format!("{action} {p}%"));
        }
        RaidHealth {
            ok: running && missing == 0,
            text: parts.join(", "),
        }
    }

    /// `sda1` -> `sda`: a partition's sysfs directory sits inside its disk's.
    fn whole_disk(&self, name: &str) -> String {
        fs::canonicalize(self.block(name))
            .ok()
            .filter(|p| p.join("partition").exists())
            .and_then(|p| Some(p.parent()?.file_name()?.to_string_lossy().into_owned()))
            .unwrap_or_else(|| name.to_string())
    }

    fn device(&self, name: &str) -> Device {
        let dir = self.block(name);
        let path = fs::canonicalize(&dir).unwrap_or_default();
        let bus = bus_of(name, &path.to_string_lossy());
        let media = match (bus, read_trim(&dir.join("queue/rotational")).as_str()) {
            ("virtual", _) => "",
            (_, "1") => "hdd",
            (_, "0") => "ssd",
            _ => "",
        };
        Device {
            name: name.to_string(),
            bus: bus.to_string(),
            model: read_trim(&dir.join("device/model")),
            media: media.to_string(),
            size: read_trim(&dir.join("size")).parse::<u64>().unwrap_or(0) * 512,
            state: String::new(),
        }
    }

    /// Member devices and data profile (`raid1`, ...) of a mounted btrfs file system.
    fn btrfs(&self, uuid: &str) -> (Vec<String>, Option<String>) {
        let base = self.root.join("fs/btrfs").join(uuid);
        let profile = dir_names(&base.join("allocation/data"))
            .into_iter()
            .find(|n| BTRFS_PROFILES.contains(&n.as_str()));
        (dir_names(&base.join("devices")), profile)
    }

    /// Whole disks with their partitions. md and dm devices are not listed as disks; they
    /// show up as "used by" on the partitions they are built from.
    fn disks(&self, list: &[FileSystem], all: bool, udev_dir: &Path) -> Vec<Disk> {
        let ctx = Mounted::new(list);
        let mut out = Vec::new();
        for name in dir_names(&self.root.join("class/block")) {
            let dir = self.block(&name);
            let Ok(real) = fs::canonicalize(&dir) else {
                continue;
            };
            if real.join("partition").exists() || dir.join("md").exists() || dir.join("dm").exists()
            {
                continue;
            }
            // The kernel's fixed ram0..ram15 block devices are never interesting (lsblk hides them).
            if name.starts_with("ram") && name[3..].bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            // loop, ram, zram: only with --all
            if !all && real.components().any(|c| c.as_os_str() == "virtual") {
                continue;
            }
            let device = self.device(&name);
            if device.size == 0 {
                continue; // empty loop devices, card readers without media
            }

            let mut parts: Vec<(u32, Part)> = Vec::new();
            for entry in dir_names(&real) {
                let pdir = real.join(&entry);
                let Ok(num) = read_trim(&pdir.join("partition")).parse::<u32>() else {
                    continue;
                };
                let udev = udev_props(udev_dir, &pdir);
                parts.push((
                    num,
                    Part {
                        kind: udev
                            .get("ID_PART_ENTRY_TYPE")
                            .map(|t| parttype::name(t))
                            .unwrap_or_default(),
                        size: read_trim(&pdir.join("size")).parse::<u64>().unwrap_or(0) * 512,
                        usage: self.usage(&entry, &udev, &ctx),
                        name: entry,
                    },
                ));
            }
            parts.sort_by_key(|(n, _)| *n);

            let udev = udev_props(udev_dir, &real);
            out.push(Disk {
                table: match udev.get("ID_PART_TABLE_TYPE").map(String::as_str) {
                    Some("dos") => "mbr".into(),
                    Some(t) => t.to_string(),
                    None => String::new(),
                },
                usage: if parts.is_empty() {
                    self.usage(&name, &udev, &ctx)
                } else {
                    Usage::default()
                },
                parts: parts.into_iter().map(|(_, p)| p).collect(),
                device,
            });
        }
        out
    }

    /// What is on a disk or partition: its own file system, or what is built on top of it.
    fn usage(&self, name: &str, udev: &HashMap<String, String>, ctx: &Mounted) -> Usage {
        let mut u = match ctx.by_dev.get(name) {
            Some(fs) => Usage::of(fs),
            None => Usage {
                // linux_raid_member, LVM2_member, crypto_LUKS, ext4, ...
                fs_type: udev.get("ID_FS_TYPE").cloned().unwrap_or_default(),
                label: udev
                    .get("ID_FS_LABEL_ENC")
                    .map(|l| mountinfo::unescape_udev(l))
                    .or_else(|| udev.get("ID_FS_LABEL").cloned())
                    .unwrap_or_default(),
                ..Usage::default()
            },
        };
        // A multi-device btrfs is mounted from one member; the others match by UUID.
        if u.mounts.is_empty()
            && let Some(fs) = udev
                .get("ID_FS_UUID")
                .and_then(|id| ctx.by_uuid.get(id.as_str()))
        {
            u.mounts = fs.mounts.clone();
        }
        for h in dir_names(&self.block(name).join("holders")) {
            u.used_by.push(self.holder(&h, ctx, 0));
        }
        u
    }

    /// `md0 (raid1) -> vg0-data (lvm): /srv`
    fn holder(&self, name: &str, ctx: &Mounted, depth: u32) -> String {
        let dir = self.block(name);
        let level = read_trim(&dir.join("md/level"));
        let mut text = if !level.is_empty() {
            format!("{name} ({level})")
        } else if dir.join("dm").exists() {
            let uuid = read_trim(&dir.join("dm/uuid"));
            let kind = if uuid.starts_with("LVM-") {
                "lvm"
            } else if uuid.starts_with("CRYPT-") {
                "crypt"
            } else {
                "dm"
            };
            let dm_name = read_trim(&dir.join("dm/name"));
            format!(
                "{} ({kind})",
                if dm_name.is_empty() { name } else { &dm_name }
            )
        } else {
            name.to_string()
        };
        if let Some(fs) = ctx.by_dev.get(name) {
            text = format!("{text}: {}", fs.mounts.join(", "));
        }
        let ups = dir_names(&dir.join("holders"));
        if depth < 8 && !ups.is_empty() {
            let ups: Vec<String> = ups.iter().map(|u| self.holder(u, ctx, depth + 1)).collect();
            text = format!("{text} -> {}", ups.join(", "));
        }
        text
    }
}

/// Mounted file systems by kernel device name (`sda1`, `dm-0`) and by file-system UUID.
struct Mounted<'a> {
    by_dev: HashMap<String, &'a FileSystem>,
    by_uuid: HashMap<&'a str, &'a FileSystem>,
}

impl<'a> Mounted<'a> {
    fn new(list: &'a [FileSystem]) -> Self {
        let mut by_dev = HashMap::new();
        for fs in list {
            let Some(src) = fs.source.as_deref().filter(|s| s.starts_with("/dev/")) else {
                continue;
            };
            // /dev/mapper/vg0-data -> /dev/dm-0
            let real = fs::canonicalize(src).unwrap_or_else(|_| PathBuf::from(src));
            if let Some(n) = real.file_name() {
                by_dev.entry(n.to_string_lossy().into_owned()).or_insert(fs);
            }
        }
        let by_uuid = list
            .iter()
            .filter_map(|fs| Some((fs.id.as_deref()?, fs)))
            .collect();
        Mounted { by_dev, by_uuid }
    }
}

pub fn disks(list: &[FileSystem], all: bool) -> Vec<Disk> {
    Sysfs {
        root: Path::new("/sys"),
    }
    .disks(list, all, Path::new("/run/udev/data"))
}

/// udev's properties for a block device (`E:KEY=value` lines in `/run/udev/data/b8:1`).
/// Readable without root; empty when udev is not running.
fn udev_props(udev_dir: &Path, sys_dir: &Path) -> HashMap<String, String> {
    let dev = read_trim(&sys_dir.join("dev"));
    let Ok(text) = fs::read_to_string(udev_dir.join(format!("b{dev}"))) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|l| l.strip_prefix("E:")?.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Bus type from the device's canonical sysfs path, e.g.
/// `/sys/devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda`.
fn bus_of(name: &str, sys_path: &str) -> &'static str {
    let parts: Vec<&str> = sys_path.split('/').collect();
    let any = |f: &dyn Fn(&str) -> bool| parts.iter().any(|p| f(p));
    let numbered = |p: &str, prefix: &str| {
        p.strip_prefix(prefix)
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
    };
    if name.starts_with("nvme") || any(&|p| p == "nvme") {
        "nvme"
    } else if any(&|p| numbered(p, "usb")) {
        "usb"
    } else if name.starts_with("mmcblk") || any(&|p| numbered(p, "mmc")) {
        "sd"
    } else if any(&|p| numbered(p, "virtio")) {
        "virtio"
    } else if any(&|p| {
        p.starts_with("VMBUS") || p.starts_with("MSFT1000") || p.starts_with("vmbus")
    }) {
        "hyperv"
    } else if any(&|p| p.starts_with("vbd-")) {
        "xen"
    } else if any(&|p| numbered(p, "session")) {
        "iscsi"
    } else if any(&|p| p.starts_with("end_device-") || p.starts_with("expander-")) {
        "sas"
    } else if any(&|p| p.starts_with("rport-")) {
        "fc"
    } else if any(&|p| numbered(p, "ata")) {
        "sata"
    } else if any(&|p| p == "virtual") {
        "virtual"
    } else if any(&|p| numbered(p, "host")) {
        "scsi"
    } else {
        ""
    }
}

fn read_trim(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// md `sync_completed` is `"done / total"` in sectors, or `none`.
fn sync_percent(s: &str) -> Option<u64> {
    let (done, total) = s.split_once('/')?;
    let (done, total): (u64, u64) = (done.trim().parse().ok()?, total.trim().parse().ok()?);
    (total > 0).then(|| done * 100 / total)
}

fn dir_names(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Removable / USB / optical / fixed, from the block device's sysfs entry.
fn block_kind(dev: &Path) -> Kind {
    let Some(name) = dev.file_name().and_then(|n| n.to_str()) else {
        return Kind::Unknown;
    };
    if name.starts_with("sr") {
        return Kind::Cdrom;
    }
    if name.starts_with("zram") {
        return Kind::Ram;
    }
    let Ok(sys) = fs::canonicalize(format!("/sys/class/block/{name}")) else {
        return Kind::Fixed;
    };
    // Partitions carry the "removable" attribute on their parent disk.
    let disk = match sys.join("partition").exists() {
        true => sys.parent().unwrap_or(&sys),
        false => &sys,
    };
    let removable = fs::read_to_string(disk.join("removable")).is_ok_and(|s| s.trim() == "1");
    // USB disks often report removable=0, but their sysfs path goes through a USB bus.
    if removable || sys.to_string_lossy().contains("/usb") {
        Kind::Removable
    } else {
        Kind::Fixed
    }
}

/// `/dev/disk/by-*` symlinks, keyed by the device they point to.
fn links_by_target(dir: &str) -> HashMap<PathBuf, String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let target = fs::canonicalize(e.path()).ok()?;
            Some((
                target,
                mountinfo::unescape_udev(&e.file_name().to_string_lossy()),
            ))
        })
        .collect()
}

/// Sizes in bytes, max name length, and inode counts from `statvfs`.
#[derive(Clone, Copy, Default)]
struct Stat {
    total: u64,
    free: u64,
    avail: u64,
    max_name: u32,
    files: u64,
    files_free: u64,
}

fn statvfs(path: &str) -> Option<Stat> {
    let c = CString::new(path).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let bs = (if s.f_frsize > 0 {
        s.f_frsize
    } else {
        s.f_bsize
    }) as u64;
    Some(Stat {
        total: s.f_blocks as u64 * bs,
        free: s.f_bfree as u64 * bs,
        avail: s.f_bavail as u64 * bs,
        max_name: s.f_namemax as u32,
        files: s.f_files as u64,
        files_free: s.f_ffree as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_root() {
        let all = list(Duration::from_secs(10));
        let root = all
            .iter()
            .find(|f| f.mounts.iter().any(|m| m == "/"))
            .expect("root listed");
        assert!(root.ready && root.total > 0 && !root.hidden);
        assert!(root.max_name.is_some_and(|n| n > 0));
        let proc = all
            .iter()
            .find(|f| f.fs_type == "proc")
            .expect("proc listed");
        assert!(proc.hidden && proc.kind == Kind::Virtual);
    }

    #[test]
    fn mount_of_paths() {
        assert_eq!(mount_of(Path::new("/")).unwrap(), "/");
        assert_eq!(mount_of(Path::new("/proc/self")).unwrap(), "/proc");
        assert!(mount_of(Path::new("/definitely/not/here/fsls")).is_err());
    }

    #[test]
    fn root_has_a_disk() {
        let all = list(Duration::from_secs(10));
        let root = all.iter().find(|f| f.mount() == "/").unwrap();
        let disk = root.devices.first().expect("root has a disk");
        assert!(!disk.bus.is_empty() && disk.size >= root.total, "{disk:?}");
    }

    #[test]
    fn bus_from_sysfs_path() {
        let pci = "/sys/devices/pci0000:00/0000:00:17.0";
        assert_eq!(
            bus_of(
                "sda",
                &format!("{pci}/ata1/host0/target0:0:0/0:0:0:0/block/sda")
            ),
            "sata"
        );
        assert_eq!(
            bus_of(
                "sdb",
                &format!("{pci}/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0/block/sdb")
            ),
            "usb"
        );
        assert_eq!(
            bus_of("nvme0n1", &format!("{pci}/0000:02:00.0/nvme/nvme0/nvme0n1")),
            "nvme"
        );
        assert_eq!(bus_of("vda", &format!("{pci}/virtio1/block/vda")), "virtio");
        assert_eq!(
            bus_of(
                "sdc",
                "/sys/devices/platform/host3/session1/target3:0:0/3:0:0:0/block/sdc"
            ),
            "iscsi"
        );
        assert_eq!(bus_of("md0", "/sys/devices/virtual/block/md0"), "virtual");
        assert_eq!(
            bus_of(
                "sdd",
                "/sys/devices/LNXSYSTM:00/LNXSYBUS:00/ACPI0004:00/MSFT1000:00/x/host0/block/sdd"
            ),
            "hyperv"
        );
        assert_eq!(
            bus_of(
                "sde",
                &format!(
                    "{pci}/0000:03:00.0/host2/port-2:0/end_device-2:0/target2:0:0/2:0:0:0/block/sde"
                )
            ),
            "sas"
        );
        assert_eq!(
            bus_of("data", "/sys/devices/x/database/block/data"),
            "",
            "'database' is not an ata port"
        );
    }

    /// Fake sysfs: LVM (dm-0) on md0 (raid1) over sda1 + sdb1 (SATA HDDs), an NVMe SSD,
    /// and a btrfs raid1 over nvme0n1 + sda1.
    #[test]
    fn resolves_raid_stacks() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("fsls-sysfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let write = |p: &Path, s: &str| {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, s).unwrap();
        };
        let class = root.join("class/block");
        fs::create_dir_all(&class).unwrap();
        let pci = root.join("devices/pci0000:00");

        for (i, disk) in ["sda", "sdb"].iter().enumerate() {
            let dir = pci.join(format!(
                "0000:00:17.0/ata{i}/host{i}/target{i}:0:0/{i}:0:0:0/block/{disk}"
            ));
            write(&dir.join("size"), "3907029168");
            write(&dir.join("queue/rotational"), "1");
            write(&dir.join("device/model"), "WDC WD20EFRX   ");
            write(&dir.join(format!("{disk}1/partition")), "1");
            symlink(&dir, class.join(disk)).unwrap();
            symlink(dir.join(format!("{disk}1")), class.join(format!("{disk}1"))).unwrap();
        }
        let nvme = pci.join("0000:02:00.0/nvme/nvme0/nvme0n1");
        write(&nvme.join("size"), "1000215216");
        write(&nvme.join("queue/rotational"), "0");
        write(&nvme.join("device/model"), "Samsung SSD 990 PRO 2TB");
        symlink(&nvme, class.join("nvme0n1")).unwrap();

        let virt = root.join("devices/virtual/block");
        let md = virt.join("md0/md");
        write(&md.join("level"), "raid1\n");
        write(&md.join("array_state"), "clean\n");
        write(&md.join("raid_disks"), "2\n");
        write(&md.join("degraded"), "0\n");
        write(&md.join("sync_action"), "idle\n");
        write(&md.join("sync_completed"), "none\n");
        write(&md.join("dev-sda1/state"), "in_sync\n");
        write(&md.join("dev-sdb1/state"), "in_sync,write_mostly\n");
        fs::create_dir_all(virt.join("md0/slaves/sda1")).unwrap();
        fs::create_dir_all(virt.join("md0/slaves/sdb1")).unwrap();
        fs::create_dir_all(virt.join("dm-0/slaves/md0")).unwrap();
        symlink(virt.join("md0"), class.join("md0")).unwrap();
        symlink(virt.join("dm-0"), class.join("dm-0")).unwrap();
        // dm-1: LUKS2 on the NVMe disk; dm-2: an LVM volume on top of it.
        write(
            &virt.join("dm-1/dm/uuid"),
            "CRYPT-LUKS2-0123abcd-cryptroot\n",
        );
        fs::create_dir_all(virt.join("dm-1/slaves/nvme0n1")).unwrap();
        write(&virt.join("dm-2/dm/uuid"), "LVM-xyz\n");
        fs::create_dir_all(virt.join("dm-2/slaves/dm-1")).unwrap();
        symlink(virt.join("dm-1"), class.join("dm-1")).unwrap();
        symlink(virt.join("dm-2"), class.join("dm-2")).unwrap();

        let btrfs = root.join("fs/btrfs/1234-uuid");
        fs::create_dir_all(btrfs.join("devices/nvme0n1")).unwrap();
        fs::create_dir_all(btrfs.join("devices/sda1")).unwrap();
        fs::create_dir_all(btrfs.join("allocation/data/raid1")).unwrap();
        write(&btrfs.join("allocation/data/total_bytes"), "1");

        let sys = Sysfs { root: &root };
        let health = |s: &Stack| s.health.as_ref().map(|h| (h.ok, h.text.clone()));

        let s = sys.stack(&["dm-0".into()]);
        assert_eq!(s.level.as_deref(), Some("raid1"));
        assert_eq!(health(&s), Some((true, "ok".into())));
        assert_eq!(s.encryption, None);
        // LVM on LUKS: the encryption is found one layer down.
        let s = sys.stack(&["dm-2".into()]);
        assert_eq!(s.encryption.as_deref(), Some("luks"));
        assert_eq!(s.devices[0].name, "nvme0n1");
        let s = sys.stack(&["dm-0".into()]);
        let names: Vec<_> = s.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["sda", "sdb"]);
        assert_eq!(
            s.devices[0],
            Device {
                name: "sda".into(),
                bus: "sata".into(),
                model: "WDC WD20EFRX".into(),
                media: "hdd".into(),
                size: 3907029168 * 512,
                state: String::new(),
            }
        );
        assert_eq!(s.devices[1].state, "", "write_mostly is still in sync");

        // sdb1 fails: degraded, but nothing to rebuild onto yet.
        write(&md.join("degraded"), "1\n");
        write(&md.join("dev-sdb1/state"), "faulty\n");
        write(&md.join("sync_action"), "recover\n");
        let s = sys.stack(&["dm-0".into()]);
        assert_eq!(health(&s), Some((false, "degraded 1/2 missing".into())));
        assert_eq!(s.devices[1].state, "faulty");

        // A replacement is rebuilding.
        write(&md.join("dev-sdb1/state"), "spare\n");
        write(&md.join("sync_completed"), "421 / 1000\n");
        let s = sys.stack(&["dm-0".into()]);
        assert_eq!(
            health(&s),
            Some((false, "degraded 1/2 missing, recover 42%".into()))
        );
        assert_eq!(s.devices[1].state, "rebuilding");

        write(&md.join("array_state"), "inactive\n");
        assert_eq!(
            health(&sys.stack(&["md0".into()])).unwrap().1,
            "inactive, recover 42%"
        );

        let s = sys.stack(&["nvme0n1".into()]);
        assert_eq!((s.level, s.health), (None, None));
        assert_eq!(
            (s.devices[0].bus.as_str(), s.devices[0].media.as_str()),
            ("nvme", "ssd")
        );

        let (members, profile) = sys.btrfs("1234-uuid");
        assert_eq!(members, ["nvme0n1", "sda1"]);
        assert_eq!(profile.as_deref(), Some("raid1"));
        let names: Vec<_> = sys
            .stack(&members)
            .devices
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, ["nvme0n1", "sda"]);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sync_progress() {
        assert_eq!(sync_percent("421 / 1000"), Some(42));
        assert_eq!(sync_percent("none"), None);
        assert_eq!(sync_percent("5 / 0"), None);
    }

    /// Fake sysfs + udev: sda (gpt) = EFI partition + a Linux RAID partition in md0 (raid1),
    /// with LVM vg0-data on md0 mounted at /srv; sdb is a whole-disk btrfs member mounted
    /// through another device; sdc is blank; loop0 only shows with --all.
    #[test]
    fn lists_disks_with_partitions_and_holders() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("fsls-disks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let write = |p: &Path, s: &str| {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, s).unwrap();
        };
        let class = root.join("class/block");
        let udev = root.join("udev");
        fs::create_dir_all(&class).unwrap();
        fs::create_dir_all(&udev).unwrap();
        let ata = root.join("devices/pci0000:00/0000:00:17.0");
        let virt = root.join("devices/virtual/block");

        let disk = |name: &str, n: u32, minor: u32| {
            let dir = ata.join(format!(
                "ata{n}/host{n}/target{n}:0:0/{n}:0:0:0/block/{name}"
            ));
            write(&dir.join("size"), "2097152"); // 1 GiB
            write(&dir.join("dev"), &format!("8:{minor}"));
            write(&dir.join("queue/rotational"), "1");
            symlink(&dir, class.join(name)).unwrap();
            dir
        };
        let sda = disk("sda", 0, 0);
        write(&udev.join("b8:0"), "E:ID_PART_TABLE_TYPE=gpt\n");
        for (p, ty, fs_type) in [
            (1, "c12a7328-f81f-11d2-ba4b-00a0c93ec93b", "vfat"),
            (
                2,
                "a19d880f-05fc-4d3b-a006-743f0f84911e",
                "linux_raid_member",
            ),
        ] {
            let dir = sda.join(format!("sda{p}"));
            write(&dir.join("partition"), &p.to_string());
            write(&dir.join("size"), "1024000");
            write(&dir.join("dev"), &format!("8:{p}"));
            write(
                &udev.join(format!("b8:{p}")),
                &format!(
                    "S:junk\nE:ID_PART_ENTRY_TYPE={ty}\nE:ID_FS_TYPE={fs_type}\nE:ID_FS_LABEL_ENC=my\\x20data\n"
                ),
            );
            symlink(&dir, class.join(format!("sda{p}"))).unwrap();
        }
        fs::create_dir_all(sda.join("sda2/holders/md0")).unwrap();
        write(&virt.join("md0/md/level"), "raid1");
        fs::create_dir_all(virt.join("md0/holders/dm-0")).unwrap();
        write(&virt.join("dm-0/dm/uuid"), "LVM-abc");
        write(&virt.join("dm-0/dm/name"), "vg0-data");
        symlink(virt.join("md0"), class.join("md0")).unwrap();
        symlink(virt.join("dm-0"), class.join("dm-0")).unwrap();

        let sdb = disk("sdb", 1, 16);
        write(
            &udev.join("b8:16"),
            "E:ID_FS_TYPE=btrfs\nE:ID_FS_UUID=b7-uuid\n",
        );
        let _ = sdb;
        disk("sdc", 2, 32);
        let loop0 = virt.join("loop0");
        write(&loop0.join("size"), "2048");
        symlink(&loop0, class.join("loop0")).unwrap();

        let mut srv = crate::fs::sample("/srv", "ext4", 10, 5);
        srv.source = Some("/dev/dm-0".into());
        let mut efi = crate::fs::sample("/boot/efi", "vfat", 10, 5);
        efi.source = Some("/dev/sda1".into());
        let mut pool = crate::fs::sample("/pool", "btrfs", 10, 5);
        (pool.source, pool.id) = (Some("/dev/sdz".into()), Some("b7-uuid".into()));
        let list = [srv, efi, pool];

        let sys = Sysfs { root: &root };
        let disks = sys.disks(&list, false, &udev);
        let names: Vec<&str> = disks.iter().map(|d| d.device.name.as_str()).collect();
        assert_eq!(
            names,
            ["sda", "sdb", "sdc"],
            "no md/dm rows, no loop without --all"
        );

        let sda = &disks[0];
        assert_eq!(
            (sda.table.as_str(), sda.device.bus.as_str()),
            ("gpt", "sata")
        );
        assert_eq!(sda.parts.len(), 2);
        let efi = &sda.parts[0];
        assert_eq!(
            (efi.name.as_str(), efi.kind.as_str(), efi.size),
            ("sda1", "EFI", 1024000 * 512)
        );
        assert_eq!(efi.usage.mounts, ["/boot/efi"]);
        let raid = &sda.parts[1];
        assert_eq!(raid.kind, "Linux RAID");
        assert_eq!(raid.usage.fs_type, "linux_raid_member");
        assert_eq!(raid.usage.label, "my data");
        assert_eq!(raid.usage.used_by, ["md0 (raid1) -> vg0-data (lvm): /srv"]);

        let sdb = &disks[1];
        assert!(sdb.parts.is_empty() && sdb.table.is_empty());
        assert_eq!(
            (sdb.usage.fs_type.as_str(), sdb.usage.mounts.clone()),
            ("btrfs", vec!["/pool".to_string()])
        );

        assert!(
            disks[2].usage.is_empty() && disks[2].parts.is_empty(),
            "sdc is blank"
        );

        let names: Vec<String> = sys
            .disks(&list, true, &udev)
            .into_iter()
            .map(|d| d.device.name)
            .collect();
        assert_eq!(names, ["loop0", "sda", "sdb", "sdc"]);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn real_disks_include_the_root_disk() {
        let list = list(Duration::from_secs(10));
        let disks = disks(&list, false);
        let root_dev = list
            .iter()
            .find(|f| f.mount() == "/")
            .and_then(|f| f.devices.first())
            .map(|d| d.name.clone());
        if let Some(root_dev) = root_dev {
            let d = disks
                .iter()
                .find(|d| d.device.name == root_dev)
                .expect("root disk listed");
            let mounted = d
                .usage
                .mounts
                .iter()
                .chain(d.parts.iter().flat_map(|p| &p.usage.mounts));
            assert!(mounted.into_iter().any(|m| m == "/"), "{d:?}");
        }
    }
}
