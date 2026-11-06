use clap::ValueEnum;

/// What kind of device backs a file system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    Fixed,
    Removable,
    #[value(alias = "remote")]
    Network,
    #[value(alias = "optical")]
    Cdrom,
    Ram,
    /// Kernel pseudo file systems (proc, sysfs, devfs, ...)
    #[value(alias = "pseudo")]
    Virtual,
    Unknown,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Fixed => "fixed",
            Kind::Removable => "removable",
            Kind::Network => "network",
            Kind::Cdrom => "cdrom",
            Kind::Ram => "ram",
            Kind::Virtual => "virtual",
            Kind::Unknown => "unknown",
        }
    }

    /// Classify by file-system type name alone; `None` means "ask the device".
    #[cfg_attr(windows, allow(dead_code))]
    pub fn from_fs_type(fs_type: &str) -> Option<Kind> {
        let t = fs_type.to_ascii_lowercase();
        let t = t.strip_prefix("fuse.").unwrap_or(&t);
        Some(match t {
            "nfs" | "nfs4" | "cifs" | "smb3" | "smbfs" | "sshfs" | "afpfs" | "webdav" | "davfs"
            | "ncpfs" | "9p" | "ceph" | "glusterfs" | "lustre" | "gpfs" | "beegfs" | "rclone"
            | "drvfs" => Kind::Network,
            "tmpfs" | "ramfs" => Kind::Ram,
            "iso9660" | "udf" | "cd9660" => Kind::Cdrom,
            "proc" | "sysfs" | "devtmpfs" | "devpts" | "devfs" | "cgroup" | "cgroup2"
            | "securityfs" | "selinuxfs" | "debugfs" | "tracefs" | "pstore" | "bpf" | "mqueue"
            | "hugetlbfs" | "configfs" | "fusectl" | "autofs" | "binfmt_misc" | "efivarfs"
            | "nsfs" | "rpc_pipefs" | "nfsd" | "rootfs" => Kind::Virtual,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct FileSystem {
    /// Every path this file system is mounted at, primary first.
    /// Empty for a Windows volume that has no mount point.
    pub mounts: Vec<String>,
    /// What is mounted: `/dev/sda1`, `server:/export`, `\\?\Volume{GUID}\`, ...
    pub source: Option<String>,
    pub label: String,
    pub fs_type: String,
    pub kind: Kind,
    /// False for empty card readers / optical drives and unreachable shares.
    pub ready: bool,
    /// The size query didn't answer within `--timeout` (dead network mount, hung FUSE).
    pub timed_out: bool,
    /// Shown only with `--all` (pseudo file systems, system volumes, empty drives, ...).
    pub hidden: bool,
    pub total: u64,
    pub free: u64,
    /// Free bytes available to the current user (honours quotas and root reserve).
    pub avail: u64,
    /// Volume serial (Windows) or file-system UUID (Linux).
    pub id: Option<String>,
    pub max_name: Option<u32>,
    /// Feature flags (Windows) or mount options (Linux, macOS).
    pub options: Vec<String>,
    /// Redundancy layout when the file system spans/mirrors disks: `raid1`, `spaces mirror`,
    /// `hardware`, `multi-disk` (spanned, striped or a layout the OS doesn't expose), ...
    pub raid: Option<String>,
    /// Array health where the OS exposes it (Linux md).
    pub raid_health: Option<RaidHealth>,
    /// Physical disks underneath, after resolving partitions, md/LVM/dm and btrfs devices.
    pub devices: Vec<Device>,
    /// Shared storage this file system draws from (APFS container, ZFS pool), so a total
    /// counts the shared space once.
    pub pool: Option<Pool>,
    /// Inode (file-slot) counts; `None` where the file system has no fixed number (NTFS).
    pub inodes: Option<Inodes>,
    /// `bitlocker`, `bitlocker (locked)`, `luks`, `filevault`, ...; `None` when not encrypted
    /// or unknown.
    pub encryption: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Inodes {
    pub total: u64,
    pub free: u64,
}

/// Storage that several file systems draw from. The two kinds report it differently, so
/// they are added up differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pool {
    /// `disk3` (APFS container) or `tank` (ZFS pool).
    pub name: String,
    /// ZFS: each dataset reports only its OWN used space, plus the pool's free space.
    /// APFS: every volume reports the whole container's size, used and free bytes, but its
    /// own used inodes against the container's shared inode capacity.
    pub own_used: bool,
}

impl Pool {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn apfs(container: String) -> Pool {
        Pool {
            name: container,
            own_used: false,
        }
    }

    /// The pool of a ZFS dataset, from its source: `tank/home/alice` -> `tank`.
    #[cfg_attr(windows, allow(dead_code))]
    pub fn zfs(source: &str) -> Option<Pool> {
        let name = source.split('/').next().filter(|n| !n.is_empty())?;
        Some(Pool {
            name: name.to_string(),
            own_used: true,
        })
    }
}

/// Combined space of several file systems, for `--total`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Space {
    pub total: u64,
    pub used: u64,
    pub avail: u64,
    pub inodes: Option<Inodes>,
}

impl Space {
    fn of(fs: &FileSystem) -> Space {
        Space {
            total: fs.total,
            used: fs.used(),
            avail: fs.avail,
            inodes: fs.inodes,
        }
    }

    /// Adds space that is not shared with what is already counted.
    fn add(&mut self, o: Space) {
        self.total += o.total;
        self.used += o.used;
        self.avail += o.avail;
        self.inodes = match (self.inodes, o.inodes) {
            (Some(a), Some(b)) => Some(Inodes {
                total: a.total + b.total,
                free: a.free + b.free,
            }),
            (a, b) => a.or(b),
        };
    }

    /// Adds another dataset of the same ZFS pool: its own used space, but free space (and
    /// free inode slots) that the pool already counted. Datasets with a quota see less of
    /// it, so the largest figure is the pool's.
    fn add_dataset(&mut self, o: Space) {
        self.used += o.used;
        self.avail = self.avail.max(o.avail);
        self.total = self.used + self.avail;
        self.inodes = match (self.inodes, o.inodes) {
            (Some(a), Some(b)) => {
                let free = a.free.max(b.free);
                Some(Inodes {
                    total: a.used() + b.used() + free,
                    free,
                })
            }
            (a, b) => a.or(b),
        };
    }

    /// Adds another volume of the same APFS container: its bytes are the container's and
    /// already counted, but each volume reports only its own used inodes against the
    /// container's shared inode capacity.
    fn add_volume(&mut self, o: Space) {
        self.inodes = match (self.inodes, o.inodes) {
            (Some(a), Some(b)) => {
                let total = a.total.max(b.total);
                Some(Inodes {
                    total,
                    free: total.saturating_sub(a.used() + b.used()),
                })
            }
            (a, b) => a.or(b),
        };
    }
}

/// Space of the ready file systems in `list`, counting shared pools once.
pub fn total_space<'a>(list: impl IntoIterator<Item = &'a FileSystem>) -> Space {
    let mut sum = Space::default();
    let mut pools: Vec<(&str, Space)> = Vec::new();
    for fs in list.into_iter().filter(|f| f.ready) {
        let space = Space::of(fs);
        let Some(pool) = &fs.pool else {
            sum.add(space);
            continue;
        };
        match pools.iter_mut().find(|(name, _)| *name == pool.name) {
            None => pools.push((&pool.name, space)),
            Some((_, seen)) if pool.own_used => seen.add_dataset(space),
            Some((_, seen)) => seen.add_volume(space),
        }
    }
    for (_, space) in pools {
        sum.add(space);
    }
    sum
}

impl Inodes {
    /// `None` when the file system has no inode table (reports 0, e.g. btrfs) or reports
    /// nonsense (9p mounts in WSL: 999 total, 1M free; NTFS on macOS: 1 total, 0 free).
    #[cfg_attr(windows, allow(dead_code))]
    pub fn new(total: u64, free: u64) -> Option<Inodes> {
        (total > 1 && free <= total).then_some(Inodes { total, free })
    }

    pub fn used(self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    pub fn percent(self) -> Option<f64> {
        percent_of(self.used(), self.free)
    }
}

/// How much of the usable space is taken: `used / (used + left)`, where `left` is what can
/// still be written. This is `df`'s definition, so space reserved for root (5% on ext4) does
/// not keep a file system that ordinary users can no longer write to below 100%. Exactly
/// 100 only when nothing is left.
pub fn percent_of(used: u64, left: u64) -> Option<f64> {
    let all = used as f64 + left as f64;
    (all > 0.0).then(|| {
        if left == 0 {
            100.0
        } else {
            used as f64 * 100.0 / all
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaidHealth {
    /// False when degraded, failed or not running.
    pub ok: bool,
    /// `ok`, `degraded`, `ok, check 31%`, `degraded, recover 42%`, `inactive`, ...
    pub text: String,
}

/// A physical (or virtual) disk backing a file system.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Device {
    /// `PhysicalDrive0`, `sda`, `nvme0n1`, `disk0`
    pub name: String,
    /// `nvme`, `sata`, `usb`, `sas`, `raid`, `spaces`, `virtio`, ...; empty if unknown.
    pub bus: String,
    pub model: String,
    /// `ssd`, `hdd`, or empty if unknown.
    pub media: String,
    pub size: u64,
    /// RAID member state when not simply in sync: `faulty`, `spare`, ...
    pub state: String,
}

/// A disk for `fsls disks`: the device, its partition table and what is on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Disk {
    pub device: Device,
    /// `gpt`, `mbr`, `apm`, or empty when the disk has no partition table.
    pub table: String,
    /// What is on the whole disk when it is not partitioned (a file system, a RAID member, ...).
    pub usage: Usage,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Part {
    /// `sda1`, `disk0s2`, `#3` (Windows numbers partitions but does not name them)
    pub name: String,
    /// Partition type: `EFI`, `Basic data`, `Linux RAID`, `APFS`, ...
    pub kind: String,
    pub size: u64,
    pub usage: Usage,
}

/// What occupies a disk or partition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub fs_type: String,
    pub label: String,
    pub mounts: Vec<String>,
    /// Larger devices built on this one, with where they are mounted:
    /// `md0 (raid1): /srv`, `disk3 (apfs container): /, /System/Volumes/Data`, `Storage Spaces pool`.
    pub used_by: Vec<String>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        self.fs_type.is_empty() && self.mounts.is_empty() && self.used_by.is_empty()
    }

    /// Type, label and mount points of a mounted file system.
    pub fn of(fs: &FileSystem) -> Usage {
        Usage {
            fs_type: fs.fs_type.clone(),
            label: fs.label.clone(),
            mounts: fs.mounts.clone(),
            used_by: Vec::new(),
        }
    }
}

impl FileSystem {
    /// Primary mount point, or the source for an unmounted volume.
    pub fn mount(&self) -> &str {
        self.mounts
            .first()
            .or(self.source.as_ref())
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    pub fn use_percent(&self) -> Option<f64> {
        percent_of(self.used(), self.avail).filter(|_| self.ready)
    }

    /// Distinct bus types of the underlying disks, e.g. `nvme` or `sata,usb`.
    pub fn buses(&self) -> String {
        let mut out: Vec<&str> = Vec::new();
        for d in &self.devices {
            if !d.bus.is_empty() && !out.contains(&d.bus.as_str()) {
                out.push(&d.bus);
            }
        }
        out.join(",")
    }
}

/// RAID label when no layout is known but the data lives on more than one disk.
pub fn multi_disk(devices: &[Device]) -> Option<String> {
    (devices.len() > 1).then(|| "multi-disk".to_string())
}

/// Names of the bits set in `flags`, in table order.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn flag_names(flags: u32, table: &[(u32, &str)]) -> Vec<String> {
    table
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

#[cfg(test)]
pub fn sample(mount: &str, fs_type: &str, total: u64, free: u64) -> FileSystem {
    FileSystem {
        mounts: vec![mount.to_string()],
        source: None,
        label: String::new(),
        fs_type: fs_type.to_string(),
        kind: Kind::Fixed,
        ready: true,
        timed_out: false,
        hidden: false,
        total,
        free,
        avail: free,
        id: None,
        max_name: None,
        options: Vec::new(),
        raid: None,
        raid_health: None,
        devices: Vec::new(),
        pool: None,
        inodes: None,
        encryption: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_and_flags() {
        let fs = sample("C:\\", "NTFS", 1000, 250);
        assert_eq!(fs.used(), 750);
        assert_eq!(fs.use_percent(), Some(75.0));
        // ext4 with blocks reserved for root: 50 of the 100 free are not available.
        let mut ext4 = sample("/", "ext4", 1000, 100);
        ext4.avail = 50;
        assert_eq!(ext4.use_percent(), Some(900.0 * 100.0 / 950.0));
        ext4.avail = 0;
        assert_eq!(ext4.use_percent(), Some(100.0));
        let table = [(4, "c"), (1, "a"), (2, "b")];
        assert_eq!(flag_names(5, &table), ["c", "a"]);
    }

    #[test]
    fn inode_counts() {
        assert_eq!(Inodes::new(0, 0), None);
        assert_eq!(Inodes::new(999, 1_000_000), None);
        assert_eq!(Inodes::new(1, 0), None);
        let i = Inodes::new(100, 25).unwrap();
        assert_eq!((i.used(), i.percent()), (75, Some(75.0)));
    }

    #[test]
    fn zfs_pool_from_source() {
        assert_eq!(
            Pool::zfs("tank/home/alice").map(|p| p.name),
            Some("tank".into())
        );
        assert_eq!(Pool::zfs("rpool").map(|p| p.name), Some("rpool".into()));
        assert_eq!(Pool::zfs(""), None);
    }

    /// statvfs on a ZFS dataset: size = its own referenced bytes + what it can still use.
    fn dataset(mount: &str, source: &str, used: u64, free: u64) -> FileSystem {
        let mut fs = sample(mount, "zfs", used + free, free);
        fs.pool = Pool::zfs(source);
        fs.inodes = Some(Inodes {
            total: used / 10 + 1000,
            free: 1000,
        });
        fs
    }

    #[test]
    fn total_adds_zfs_datasets_used_and_counts_pool_free_once() {
        let list = [
            dataset("/", "rpool/ROOT/ubuntu", 40, 500),
            dataset("/home", "rpool/home", 300, 500),
            dataset("/var/log", "rpool/var/log", 10, 500),
            sample("/boot", "ext4", 100, 60),
        ];
        let t = total_space(&list);
        assert_eq!((t.used, t.avail, t.total), (350 + 40, 500 + 60, 850 + 100));
        let i = t.inodes.unwrap();
        assert_eq!((i.used(), i.free), (4 + 30 + 1, 1000));
    }

    #[test]
    fn total_takes_the_pool_free_space_past_a_quota() {
        // refquota 100 on /home: only 10 left there, while the pool has 900.
        let list = [
            dataset("/home", "tank/home", 90, 10),
            dataset("/tank", "tank", 5, 900),
        ];
        let t = total_space(&list);
        assert_eq!((t.used, t.avail, t.total), (95, 900, 995));
    }

    /// APFS volumes in one container all report the container's size, used and free bytes,
    /// but each its own used inodes out of the container's shared inode capacity.
    #[test]
    fn total_counts_an_apfs_container_once() {
        let mut root = sample("/", "apfs", 926, 344);
        root.pool = Some(Pool::apfs("disk3".into()));
        root.inodes = Some(Inodes {
            total: 3_600,
            free: 3_600 - 427,
        });
        let mut data = sample("/System/Volumes/Data", "apfs", 926, 344);
        data.pool = Some(Pool::apfs("disk3".into()));
        data.inodes = Some(Inodes {
            total: 3_600,
            free: 3_600 - 1_400,
        });
        let t = total_space(&[root, data]);
        assert_eq!((t.total, t.used, t.avail), (926, 582, 344));
        let i = t.inodes.unwrap();
        assert_eq!((i.total, i.used()), (3_600, 427 + 1_400));
    }

    #[test]
    fn kind_from_fs_type() {
        assert_eq!(Kind::from_fs_type("NFS4"), Some(Kind::Network));
        assert_eq!(Kind::from_fs_type("fuse.sshfs"), Some(Kind::Network));
        assert_eq!(Kind::from_fs_type("tmpfs"), Some(Kind::Ram));
        assert_eq!(Kind::from_fs_type("proc"), Some(Kind::Virtual));
        assert_eq!(Kind::from_fs_type("cd9660"), Some(Kind::Cdrom));
        assert_eq!(Kind::from_fs_type("ext4"), None);
    }

    #[test]
    fn not_ready_has_no_percent() {
        let mut fs = sample("E:\\", "", 0, 0);
        fs.ready = false;
        assert_eq!(fs.use_percent(), None);
    }

    #[test]
    fn unmounted_volume_uses_guid_path() {
        let mut fs = sample("X", "NTFS", 1, 1);
        fs.mounts.clear();
        fs.source = Some(r"\\?\Volume{abc}\".to_string());
        assert_eq!(fs.mount(), r"\\?\Volume{abc}\");
    }
}
