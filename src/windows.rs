//! File-system enumeration via the Win32 volume management API.

use std::collections::{HashMap, HashSet};
use std::mem::{offset_of, size_of};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, GetLastError, INVALID_HANDLE_VALUE, PROPERTYKEY,
};
use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, FindFirstVolumeW, FindNextVolumeW,
    FindVolumeClose, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW,
    GetVolumeInformationW, GetVolumePathNameW, GetVolumePathNamesForVolumeNameW,
    IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING, QueryDosDeviceW,
};
use windows_sys::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::Console::{
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
    SetConsoleMode,
};
use windows_sys::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetErrorMode};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DEVICE_SEEK_PENALTY_DESCRIPTOR, DISK_EXTENT, DISK_GEOMETRY_EX, DRIVE_LAYOUT_INFORMATION_EX,
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, IOCTL_STORAGE_QUERY_PROPERTY,
    PARTITION_INFORMATION_EX, PARTITION_INFORMATION_GPT, PARTITION_INFORMATION_MBR,
    PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_RESILIENCY_DESCRIPTOR,
    STORAGE_PROPERTY_ID, STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
    StorageDeviceResiliencyProperty, StorageDeviceSeekPenaltyProperty, VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::UI::Shell::PropertiesSystem::{
    GPS_DEFAULT, PSGetPropertyKeyFromName, SHGetPropertyStoreFromParsingName,
};
use windows_sys::core::GUID;

use crate::deadline;
use crate::fs::{Device, Disk, FileSystem, Kind, Part, RaidHealth, Usage, flag_names, multi_disk};
use crate::parttype;

/// `FILE_*` file-system flags reported by `GetVolumeInformationW`, in display order.
const FLAGS: &[(u32, &str)] = &[
    (0x0008_0000, "readonly"),       // FILE_READ_ONLY_VOLUME
    (0x0000_0001, "case-sensitive"), // FILE_CASE_SENSITIVE_SEARCH
    (0x0000_0008, "acls"),           // FILE_PERSISTENT_ACLS
    (0x0000_0010, "compression"),    // FILE_FILE_COMPRESSION
    (0x0000_8000, "compressed"),     // FILE_VOLUME_IS_COMPRESSED
    (0x0002_0000, "encryption"),     // FILE_SUPPORTS_ENCRYPTION
    (0x0000_0040, "sparse"),         // FILE_SUPPORTS_SPARSE_FILES
    (0x0000_0080, "reparse"),        // FILE_SUPPORTS_REPARSE_POINTS
    (0x0040_0000, "hardlinks"),      // FILE_SUPPORTS_HARD_LINKS
    (0x0004_0000, "streams"),        // FILE_NAMED_STREAMS
    (0x0000_0020, "quotas"),         // FILE_VOLUME_QUOTAS
    (0x0800_0000, "block-clone"),    // FILE_SUPPORTS_BLOCK_REFCOUNTING
    (0x2000_0000, "dax"),            // FILE_DAX_VOLUME
];

pub fn same_path(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Switch the console to interpreting ANSI colour codes. `false` if it refuses (very old
/// consoles); `true` when stdout is not a console at all (`--color=always` into a pipe).
pub fn enable_ansi() -> bool {
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0;
        if GetConsoleMode(h, &mut mode) == 0 {
            return true;
        }
        mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
            || SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

/// Where a drive letter that is not a volume points: a `subst` drive's folder
/// (`C:\Users\me\proj`) or a mapped network drive's share (`\\server\share`).
fn drive_source(root: &str) -> Option<String> {
    let letter = root.get(..2).filter(|l| l.ends_with(':'))?;
    let w = wide(letter);
    let mut buf = [0u16; 1024];
    if unsafe { QueryDosDeviceW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } > 0
        && let Some(path) = from_wide(&buf).strip_prefix(r"\??\")
    {
        return Some(path.to_string());
    }
    let mut len = buf.len() as u32;
    // Reads the local connection table; does not contact the server.
    (unsafe { WNetGetConnectionW(w.as_ptr(), buf.as_mut_ptr(), &mut len) } == 0)
        .then(|| from_wide(&buf))
}

/// BitLocker status of local volumes, read on demand (only when it is displayed).
pub fn add_encryption(list: &mut [FileSystem], timeout: Duration) {
    let targets: Vec<(usize, String)> = list
        .iter()
        .enumerate()
        .filter(|(_, fs)| fs.ready && matches!(fs.kind, Kind::Fixed | Kind::Removable))
        .filter_map(|(i, fs)| Some((i, fs.mounts.first()?.clone())))
        .collect();
    if targets.is_empty() {
        return;
    }
    let roots: Vec<String> = targets.iter().map(|(_, r)| r.clone()).collect();
    // COM on a thread of our own, bounded like every other system query.
    let Some(Some(found)) = deadline::run_all([move || bitlocker(&roots)], timeout).pop() else {
        return;
    };
    for ((i, _), status) in targets.iter().zip(found) {
        list[*i].encryption = status;
    }
}

/// IPropertyStore: IUnknown (3 slots), then GetCount, GetAt, GetValue, SetValue, Commit.
#[repr(C)]
struct PropertyStoreVtbl {
    _query_interface: usize,
    _add_ref: usize,
    release: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32,
    _get_count: usize,
    _get_at: usize,
    get_value: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        *const PROPERTYKEY,
        *mut PROPVARIANT,
    ) -> i32,
}

const IID_IPROPERTYSTORE: GUID = GUID {
    data1: 0x886d_8eeb,
    data2: 0x8cf2,
    data3: 0x4446,
    data4: [0x8d, 0x02, 0xcd, 0xba, 0x1d, 0xbd, 0xcf, 0x99],
};

/// Explorer's `System.Volume.BitLockerProtection` for each root (no admin needed, unlike
/// manage-bde or the WMI BitLocker class).
fn bitlocker(roots: &[String]) -> Vec<Option<String>> {
    let mut out = vec![None; roots.len()];
    unsafe {
        let com = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) >= 0;
        let mut key: PROPERTYKEY = std::mem::zeroed();
        let name = wide("System.Volume.BitLockerProtection");
        if PSGetPropertyKeyFromName(name.as_ptr(), &mut key) >= 0 {
            for (slot, root) in out.iter_mut().zip(roots) {
                *slot = bitlocker_status(protection_of(root, &key));
            }
        }
        if com {
            CoUninitialize();
        }
    }
    out
}

unsafe fn protection_of(root: &str, key: &PROPERTYKEY) -> Option<u32> {
    let path = wide(root);
    let mut store: *mut core::ffi::c_void = null_mut();
    let hr = unsafe {
        SHGetPropertyStoreFromParsingName(
            path.as_ptr(),
            null_mut(),
            GPS_DEFAULT,
            &IID_IPROPERTYSTORE,
            &mut store,
        )
    };
    if hr < 0 || store.is_null() {
        return None;
    }
    unsafe {
        let vtbl = &**(store as *const *const PropertyStoreVtbl);
        let mut v: PROPVARIANT = std::mem::zeroed();
        let value = if (vtbl.get_value)(store, key, &mut v) >= 0 {
            let inner = &v.Anonymous.Anonymous;
            match inner.vt {
                3 | 22 => u32::try_from(inner.Anonymous.lVal).ok(), // VT_I4, VT_INT
                19 | 23 => Some(inner.Anonymous.ulVal),             // VT_UI4, VT_UINT
                _ => None,
            }
        } else {
            None
        };
        PropVariantClear(&mut v);
        (vtbl.release)(store);
        value
    }
}

/// Values of `System.Volume.BitLockerProtection` (2 = off, 0 = not encryptable).
fn bitlocker_status(v: Option<u32>) -> Option<String> {
    let s = match v? {
        1 => "bitlocker",
        3 => "bitlocker (encrypting)",
        4 => "bitlocker (decrypting)",
        5 => "bitlocker (suspended)",
        6 => "bitlocker (locked)",
        8 => "bitlocker (waiting for activation)",
        _ => return None,
    };
    Some(s.into())
}

/// Every file system the machine knows about: local volumes (mounted or not)
/// plus drive letters that are not volumes (network shares, subst drives).
pub fn list(timeout: Duration) -> Vec<FileSystem> {
    // Never pop up "There is no disk in the drive" dialogs for empty readers.
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS) };

    // (query root, volume GUID path, mount points)
    let mut targets: Vec<(String, Option<String>, Vec<String>)> = Vec::new();
    let mut covered = HashSet::new();
    for (guid, mounts) in volumes() {
        for m in &mounts {
            covered.insert(m.to_uppercase());
        }
        let root = mounts.first().unwrap_or(&guid).clone();
        targets.push((root, Some(guid), mounts));
    }
    for drive in logical_drives() {
        if !covered.contains(&drive.to_uppercase()) {
            targets.push((drive.clone(), None, vec![drive]));
        }
    }

    // GetVolumeInformation / GetDiskFreeSpaceEx can stall for a long time on a
    // disconnected network drive.
    let infos = deadline::run_all(
        targets.iter().map(|(root, _, _)| {
            let root = root.clone();
            move || volume_info(&root)
        }),
        timeout,
    );

    let mut disks = HashMap::new();
    let mut list: Vec<FileSystem> = targets
        .into_iter()
        .zip(infos)
        .map(|((root, volume, mounts), info)| {
            let mut fs = build(&root, volume, mounts, info);
            if let (Some(guid), true) = (&fs.source, fs.ready) {
                (fs.devices, fs.raid) = volume_devices(guid, &mut disks);
            }
            if fs.source.is_none() {
                fs.source = drive_source(&root);
            }
            fs
        })
        .collect();

    // Pool members and health are only in the Storage Management API; ask it once, and only
    // when a Storage Spaces disk is actually present (PowerShell takes ~0.5 s to start).
    if list
        .iter()
        .any(|fs| fs.devices.iter().any(|d| d.bus == "spaces"))
    {
        // Outer Option: timed out; inner: PowerShell could not be started.
        let spaces = deadline::run_all([query_spaces], timeout.max(Duration::from_secs(10)))
            .pop()
            .flatten()
            .flatten()
            .unwrap_or_default();
        for fs in &mut list {
            apply_spaces(fs, &spaces);
        }
    }
    list
}

/// A Storage Spaces virtual disk, keyed by its disk number.
#[derive(Debug, Default, PartialEq)]
struct Space {
    health: Option<RaidHealth>,
    members: Vec<Device>,
}

const SPACES_QUERY: &str = r#"$ErrorActionPreference = 'SilentlyContinue'
foreach ($v in Get-VirtualDisk) {
  $n = ($v | Get-Disk).Number
  "V`t$n`t$($v.HealthStatus)`t$($v.OperationalStatus -join ',')"
  foreach ($p in ($v | Get-PhysicalDisk)) {
    "P`t$n`t$($p.DeviceId)`t$($p.FriendlyName)`t$($p.BusType)`t$($p.MediaType)`t$($p.Size)`t$($p.HealthStatus)`t$($p.OperationalStatus -join ',')`t$($p.Usage)"
  }
}"#;

fn query_spaces() -> Option<HashMap<u32, Space>> {
    let out = std::process::Command::new(powershell())
        .args(["-NoProfile", "-NonInteractive", "-Command", SPACES_QUERY])
        .output()
        .ok()?;
    Some(parse_spaces(&String::from_utf8_lossy(&out.stdout)))
}

/// Full path, never a bare name: Windows looks for bare names in fsls.exe's own folder first,
/// so a `powershell.exe` dropped next to a downloaded fsls.exe would run instead.
fn powershell() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    std::path::Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// Tab-separated `V` (virtual disk) and `P` (its physical disks) lines from [`SPACES_QUERY`].
fn parse_spaces(text: &str) -> HashMap<u32, Space> {
    let mut out: HashMap<u32, Space> = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
        let Some(n) = f.get(1).and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        match (f[0], f.len()) {
            ("V", 4) => {
                let ok = f[2] == "Healthy";
                let text = if ok {
                    "ok".to_string()
                } else {
                    // "Degraded", "Incomplete", "InService" (repairing), "Detached", ...;
                    // several at once arrive comma-joined.
                    match f[3].to_lowercase().as_str() {
                        "" => f[2].to_lowercase(),
                        s => s.replace("inservice", "repairing").replace(',', ", "),
                    }
                };
                out.entry(n).or_default().health = Some(RaidHealth { ok, text });
            }
            ("P", 10) => {
                let healthy = f[7] == "Healthy" && (f[8] == "OK" || f[8].is_empty());
                let state = match f[9] {
                    "Hot Spare" => "spare".to_string(),
                    "Retired" => "retired".to_string(),
                    _ if healthy => String::new(),
                    _ => f[8].to_lowercase(),
                };
                out.entry(n).or_default().members.push(Device {
                    // A member that dropped out of the pool has no DeviceId any more.
                    name: if f[2].is_empty() {
                        "missing".into()
                    } else {
                        format!("PhysicalDrive{}", f[2])
                    },
                    bus: spaces_bus(f[4]),
                    model: f[3].to_string(),
                    media: match f[5] {
                        "SSD" => "ssd",
                        "HDD" => "hdd",
                        _ => "",
                    }
                    .to_string(),
                    size: f[6].parse().unwrap_or(0),
                    state,
                });
            }
            _ => {}
        }
    }
    out
}

/// Storage-module `BusType` names in the same style as [`bus_name`].
fn spaces_bus(t: &str) -> String {
    match t {
        "File Backed Virtual" => "vhd".into(),
        "Fibre Channel" => "fibre".into(),
        "Storage Spaces" => "spaces".into(),
        t => t.to_lowercase().replace(' ', "-"),
    }
}

/// Replace Storage Spaces virtual disks with their pool members and attach health.
fn apply_spaces(fs: &mut FileSystem, spaces: &HashMap<u32, Space>) {
    let mut devices = Vec::new();
    for d in fs.devices.drain(..) {
        let space = (d.bus == "spaces")
            .then(|| d.name.strip_prefix("PhysicalDrive")?.parse::<u32>().ok())
            .flatten()
            .and_then(|n| spaces.get(&n));
        match space {
            Some(s) => {
                if fs.raid_health.as_ref().is_none_or(|h| h.ok) {
                    fs.raid_health = s.health.clone();
                }
                if s.members.is_empty() {
                    devices.push(d);
                }
                devices.extend(s.members.iter().cloned());
            }
            None => devices.push(d),
        }
    }
    fs.devices = devices;
}

/// The mount point (`C:\`, `D:\mnt\data\`, ...) of the file system containing `path`.
pub fn mount_of(path: &Path) -> std::io::Result<String> {
    let abs = std::path::absolute(path)?;
    std::fs::metadata(&abs)?; // report a missing path as "not found", not as some volume
    let w = wide(&abs.to_string_lossy());
    let mut buf = vec![0u16; abs.as_os_str().len() + 2];
    if unsafe { GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(from_wide(&buf))
}

fn volumes() -> Vec<(String, Vec<String>)> {
    let mut res = Vec::new();
    let mut buf = [0u16; 64]; // "\\?\Volume{GUID}\" is 49 chars
    unsafe {
        let h = FindFirstVolumeW(buf.as_mut_ptr(), buf.len() as u32);
        if h == INVALID_HANDLE_VALUE {
            return res;
        }
        loop {
            let guid = from_wide(&buf);
            let mounts = mount_paths(&guid);
            res.push((guid, mounts));
            if FindNextVolumeW(h, buf.as_mut_ptr(), buf.len() as u32) == 0 {
                break;
            }
        }
        FindVolumeClose(h);
    }
    res
}

fn mount_paths(guid: &str) -> Vec<String> {
    let w = wide(guid);
    let mut buf = vec![0u16; 256];
    loop {
        let mut needed = 0u32;
        let ok = unsafe {
            GetVolumePathNamesForVolumeNameW(
                w.as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut needed,
            )
        };
        if ok != 0 {
            return multi_sz(&buf);
        }
        if unsafe { GetLastError() } != ERROR_MORE_DATA || needed as usize <= buf.len() {
            return Vec::new();
        }
        buf.resize(needed as usize, 0);
    }
}

fn logical_drives() -> Vec<String> {
    let len = unsafe { GetLogicalDriveStringsW(0, std::ptr::null_mut()) };
    let mut buf = vec![0u16; len as usize + 1];
    let n = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) };
    if n == 0 || n as usize > buf.len() {
        return Vec::new();
    }
    multi_sz(&buf)
}

fn drive_kind(root: &str) -> Kind {
    match unsafe { GetDriveTypeW(wide(root).as_ptr()) } {
        2 => Kind::Removable, // DRIVE_REMOVABLE
        3 => Kind::Fixed,     // DRIVE_FIXED
        4 => Kind::Network,   // DRIVE_REMOTE
        5 => Kind::Cdrom,     // DRIVE_CDROM
        6 => Kind::Ram,       // DRIVE_RAMDISK
        _ => Kind::Unknown,
    }
}

struct VolumeInfo {
    label: String,
    fs_type: String,
    serial: u32,
    max_name: u32,
    flags: u32,
    total: u64,
    free: u64,
    avail: u64,
}

/// Label, type and sizes; `None` when the drive is not ready (empty reader, offline share).
fn volume_info(root: &str) -> Option<VolumeInfo> {
    let w = wide(root);
    let mut label = [0u16; 261];
    let mut fs_name = [0u16; 261];
    let (mut serial, mut max_name, mut flags) = (0u32, 0u32, 0u32);
    let ok = unsafe {
        GetVolumeInformationW(
            w.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            &mut serial,
            &mut max_name,
            &mut flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
    Some(VolumeInfo {
        label: from_wide(&label),
        fs_type: from_wide(&fs_name),
        serial,
        max_name,
        flags,
        total,
        free,
        avail,
    })
}

/// `info`: outer `None` = timed out, inner `None` = not ready.
fn build(
    root: &str,
    volume: Option<String>,
    mounts: Vec<String>,
    info: Option<Option<VolumeInfo>>,
) -> FileSystem {
    let timed_out = info.is_none();
    let info = info.flatten();
    let ready = info.is_some();
    let i = info.unwrap_or(VolumeInfo {
        label: String::new(),
        fs_type: String::new(),
        serial: 0,
        max_name: 0,
        flags: 0,
        total: 0,
        free: 0,
        avail: 0,
    });

    FileSystem {
        // A timed-out drive stays visible: the user needs to know it is hanging.
        hidden: (!ready && !timed_out) || mounts.is_empty(),
        kind: drive_kind(root),
        mounts,
        source: volume,
        label: i.label,
        fs_type: i.fs_type,
        ready,
        timed_out,
        total: i.total,
        free: i.free,
        avail: i.avail,
        id: ready.then(|| format!("{:04X}-{:04X}", i.serial >> 16, i.serial & 0xFFFF)),
        max_name: ready.then_some(i.max_name),
        options: if ready {
            flag_names(i.flags, FLAGS)
        } else {
            Vec::new()
        },
        raid: None,
        raid_health: None,
        devices: Vec::new(),
        pool: None,
        inodes: None, // NTFS/ReFS/FAT have no fixed inode table
        encryption: None,
    }
}

/// A disk plus, for Storage Spaces, its resiliency layout.
type DiskInfo = (Device, Option<String>);

/// The physical disks under a volume and its RAID layout.
fn volume_devices(
    volume: &str,
    cache: &mut HashMap<u32, DiskInfo>,
) -> (Vec<Device>, Option<String>) {
    let mut devices = Vec::new();
    let mut raid = None;
    for n in volume_disks(volume) {
        let (dev, resiliency) = cache.entry(n).or_insert_with(|| disk(n)).clone();
        if resiliency.is_some() {
            raid = resiliency;
        } else if dev.bus == "raid" {
            // A hardware RAID controller presents the array as one disk and hides the members.
            raid = Some("hardware".into());
        }
        devices.push(dev);
    }
    // Dynamic-disk spanned/striped/RAID-5 volumes: extents on several disks.
    let raid = raid.or_else(|| multi_disk(&devices));
    (devices, raid)
}

/// Disk numbers holding a volume's extents.
fn volume_disks(volume: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for (n, _, _) in volume_extents(volume) {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// (disk number, byte offset, length) of each piece of a volume.
fn volume_extents(volume: &str) -> Vec<(u32, u64, u64)> {
    let Some(h) = open(volume.trim_end_matches('\\')) else {
        return Vec::new();
    };
    let Some(buf) = ioctl::<()>(&h, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, None, 64) else {
        return Vec::new();
    };
    let b = bytes(&buf);
    let count = u32_at(b, offset_of!(VOLUME_DISK_EXTENTS, NumberOfDiskExtents)) as usize;
    let mut out = Vec::new();
    for i in 0..count {
        let e = offset_of!(VOLUME_DISK_EXTENTS, Extents) + i * size_of::<DISK_EXTENT>();
        if e + size_of::<DISK_EXTENT>() > b.len() {
            break;
        }
        out.push((
            u32_at(b, e + offset_of!(DISK_EXTENT, DiskNumber)),
            u64_at(b, e + offset_of!(DISK_EXTENT, StartingOffset)),
            u64_at(b, e + offset_of!(DISK_EXTENT, ExtentLength)),
        ));
    }
    out
}

/// `fsls disks`: every `\\.\PhysicalDriveN` with its partitions; volumes are placed on
/// partitions by where their extents start.
pub fn disks(list: &[FileSystem], _all: bool) -> Vec<Disk> {
    let extents: Vec<(u32, u64, &FileSystem)> = list
        .iter()
        .filter_map(|fs| Some((fs.source.as_deref()?, fs)))
        .flat_map(|(guid, fs)| {
            volume_extents(guid)
                .into_iter()
                .map(move |(n, start, _)| (n, start, fs))
        })
        .collect();

    let mut out = Vec::new();
    // Disk numbers can have gaps (a removed disk); opening a missing one fails instantly.
    for n in 0..128u32 {
        let Some(h) = open(&format!(r"\\.\PhysicalDrive{n}")) else {
            continue;
        };
        let (table, layout) = partitions(&h);
        drop(h);
        let volumes_in = |start: u64, len: u64| -> Vec<&FileSystem> {
            let mut v: Vec<&FileSystem> = Vec::new();
            for &(d, s, fs) in &extents {
                if d == n
                    && s >= start
                    && s < start.saturating_add(len)
                    && !v.iter().any(|x| std::ptr::eq(*x, fs))
                {
                    v.push(fs);
                }
            }
            v
        };
        let parts: Vec<Part> = layout
            .into_iter()
            .map(|(num, kind, start, len)| Part {
                name: format!("#{num}"),
                usage: usage_of(&volumes_in(start, len), &kind),
                kind,
                size: len,
            })
            .collect();
        let usage = if parts.is_empty() {
            usage_of(&volumes_in(0, u64::MAX), "")
        } else {
            Usage::default()
        };
        out.push(Disk {
            device: disk(n).0,
            table,
            usage,
            parts,
        });
    }
    out
}

fn usage_of(volumes: &[&FileSystem], kind: &str) -> Usage {
    let mut u = volumes.first().map(|fs| Usage::of(fs)).unwrap_or_default();
    for fs in volumes.iter().skip(1) {
        u.mounts.extend(fs.mounts.iter().cloned());
    }
    match kind {
        "Storage Spaces" => u.used_by.push("Storage Spaces pool".into()),
        "LDM data" if volumes.is_empty() => u.used_by.push("dynamic disk".into()),
        _ => {}
    }
    u
}

/// Partition table style and (number, type, offset, length) of each partition.
fn partitions(h: &OwnedHandle) -> (String, Vec<(u32, String, u64, u64)>) {
    let Some(buf) = ioctl::<()>(h, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, None, 4096) else {
        return (String::new(), Vec::new());
    };
    let b = bytes(&buf);
    let style = u32_at(b, offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionStyle));
    let count = u32_at(b, offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionCount)) as usize;
    let table = match style {
        0 => "mbr",
        1 => "gpt",
        _ => "",
    };
    let mut out = Vec::new();
    for i in 0..count {
        let e = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry)
            + i * size_of::<PARTITION_INFORMATION_EX>();
        if e + size_of::<PARTITION_INFORMATION_EX>() > b.len() {
            break;
        }
        let start = u64_at(b, e + offset_of!(PARTITION_INFORMATION_EX, StartingOffset));
        let len = u64_at(b, e + offset_of!(PARTITION_INFORMATION_EX, PartitionLength));
        let num = u32_at(b, e + offset_of!(PARTITION_INFORMATION_EX, PartitionNumber));
        let info = e + offset_of!(PARTITION_INFORMATION_EX, Anonymous); // MBR/GPT union
        if len == 0 {
            continue;
        }
        let kind = match style {
            0 => {
                let t = b[info + offset_of!(PARTITION_INFORMATION_MBR, PartitionType)];
                if t == 0 {
                    continue; // unused MBR slot
                }
                parttype::mbr(t).map_or_else(|| format!("0x{t:02x}"), String::from)
            }
            1 => {
                let guid = guid_at(
                    b,
                    info + offset_of!(PARTITION_INFORMATION_GPT, PartitionType),
                );
                match parttype::gpt(&guid) {
                    Some(k) => k.to_string(),
                    None => {
                        let at = info + offset_of!(PARTITION_INFORMATION_GPT, Name);
                        let name = utf16_at(b, at, 36);
                        if name.is_empty() { guid } else { name }
                    }
                }
            }
            _ => continue,
        };
        out.push((num, kind, start, len));
    }
    (table.into(), out)
}

/// A little-endian Windows GUID as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
fn guid_at(b: &[u8], off: usize) -> String {
    let Some(g) = b.get(off..off + 16) else {
        return String::new();
    };
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{}",
        u32::from_le_bytes(g[0..4].try_into().unwrap()),
        u16::from_le_bytes(g[4..6].try_into().unwrap()),
        u16::from_le_bytes(g[6..8].try_into().unwrap()),
        g[8],
        g[9],
        g[10..16]
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect::<String>()
    )
}

fn utf16_at(b: &[u8], off: usize, max: usize) -> String {
    let units: Vec<u16> = (0..max)
        .filter_map(|i| b.get(off + i * 2..off + i * 2 + 2))
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn disk(n: u32) -> DiskInfo {
    let mut dev = Device {
        name: format!("PhysicalDrive{n}"),
        ..Default::default()
    };
    let Some(h) = open(&format!(r"\\.\PhysicalDrive{n}")) else {
        return (dev, None);
    };

    if let Some(buf) = property(&h, StorageDeviceProperty) {
        let b = bytes(&buf);
        let at = |off| ascii_at(b, u32_at(b, off) as usize);
        let vendor = at(offset_of!(STORAGE_DEVICE_DESCRIPTOR, VendorIdOffset));
        let product = at(offset_of!(STORAGE_DEVICE_DESCRIPTOR, ProductIdOffset));
        dev.model = if vendor.is_empty() || product.starts_with(&vendor) {
            product
        } else {
            format!("{vendor} {product}")
        };
        dev.bus = bus_name(u32_at(b, offset_of!(STORAGE_DEVICE_DESCRIPTOR, BusType))).into();
    }
    if let Some(buf) = property(&h, StorageDeviceSeekPenaltyProperty) {
        let seeks = bytes(&buf)[offset_of!(DEVICE_SEEK_PENALTY_DESCRIPTOR, IncursSeekPenalty)] != 0;
        dev.media = if seeks { "hdd" } else { "ssd" }.into();
    }
    if let Some(buf) = ioctl::<()>(&h, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, None, 32) {
        dev.size = u64_at(bytes(&buf), offset_of!(DISK_GEOMETRY_EX, DiskSize));
    }

    // Storage Spaces virtual disks describe their layout: Simple / Mirror / Parity.
    let resiliency = (dev.bus == "spaces")
        .then(|| property(&h, StorageDeviceResiliencyProperty))
        .flatten()
        .map(|buf| {
            let b = bytes(&buf);
            let name = ascii_at(
                b,
                u32_at(
                    b,
                    offset_of!(STORAGE_DEVICE_RESILIENCY_DESCRIPTOR, NameOffset),
                ) as usize,
            );
            let copies = u32_at(
                b,
                offset_of!(STORAGE_DEVICE_RESILIENCY_DESCRIPTOR, NumberOfPhysicalCopies),
            );
            match (name.to_lowercase().as_str(), copies) {
                ("", _) => "spaces".to_string(),
                ("mirror", c) if c > 2 => format!("spaces mirror {c}-way"),
                (n, _) => format!("spaces {n}"),
            }
        });
    (dev, resiliency)
}

/// `STORAGE_BUS_TYPE` names.
fn bus_name(t: u32) -> &'static str {
    const NAMES: [&str; 21] = [
        "", "scsi", "atapi", "ata", "1394", "ssa", "fibre", "usb", "raid", "iscsi", "sas", "sata",
        "sd", "mmc", "virtual", "vhd", "spaces", "nvme", "scm", "ufs", "nvmeof",
    ];
    NAMES.get(t as usize).copied().unwrap_or("")
}

/// Open a device for IOCTLs only: no read/write access, so no admin rights needed.
fn open(path: &str) -> Option<OwnedHandle> {
    let w = wide(path);
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    (h != INVALID_HANDLE_VALUE).then(|| unsafe { OwnedHandle::from_raw_handle(h) })
}

fn property(h: &OwnedHandle, id: STORAGE_PROPERTY_ID) -> Option<Vec<u64>> {
    let q = STORAGE_PROPERTY_QUERY {
        PropertyId: id,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    ioctl(h, IOCTL_STORAGE_QUERY_PROPERTY, Some(&q), 512)
}

/// Run an IOCTL into an 8-byte aligned buffer of `words` u64s, growing it while the driver
/// says it is too small.
fn ioctl<T>(h: &OwnedHandle, code: u32, input: Option<&T>, mut words: usize) -> Option<Vec<u64>> {
    let (in_ptr, in_len) = input.map_or((null(), 0), |i| {
        ((i as *const T).cast(), size_of::<T>() as u32)
    });
    loop {
        let mut buf = vec![0u64; words];
        let mut got = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                h.as_raw_handle(),
                code,
                in_ptr,
                in_len,
                buf.as_mut_ptr().cast(),
                (words * 8) as u32,
                &mut got,
                null_mut(),
            )
        };
        if ok != 0 {
            return Some(buf);
        }
        let err = unsafe { GetLastError() };
        if (err != ERROR_MORE_DATA && err != ERROR_INSUFFICIENT_BUFFER) || words >= 1 << 16 {
            return None;
        }
        words *= 4;
    }
}

fn bytes(buf: &[u64]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), buf.len() * 8) }
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    b.get(off..off + 4)
        .map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()))
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    b.get(off..off + 8)
        .map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap()))
}

/// NUL-terminated ASCII at a byte offset (0 = absent), trimmed.
fn ascii_at(b: &[u8], off: usize) -> String {
    if off == 0 || off >= b.len() {
        return String::new();
    }
    let s = &b[off..];
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..end]).trim().to_string()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Parse a REG_MULTI_SZ style buffer: strings separated by NUL, ended by an empty string.
fn multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multi_sz() {
        let buf: Vec<u16> = "C:\\\0D:\\mnt\\\0\0garbage".encode_utf16().collect();
        assert_eq!(multi_sz(&buf), vec!["C:\\", "D:\\mnt\\"]);
        assert!(multi_sz(&[0, 0]).is_empty());
    }

    #[test]
    fn lists_the_system_drive() {
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()) + "\\";
        let all = list(std::time::Duration::from_secs(10));
        let fs = all
            .iter()
            .find(|f| f.mounts.iter().any(|m| m.eq_ignore_ascii_case(&sys)))
            .expect("system drive listed");
        assert!(fs.ready && fs.total > 0 && fs.source.is_some() && !fs.hidden);
        assert_eq!(fs.kind, Kind::Fixed);
        assert!(fs.id.as_deref().is_some_and(|id| id.len() == 9));
        assert!(fs.options.iter().any(|o| o == "acls"));
        let disk = fs.devices.first().expect("system drive has a disk");
        assert!(
            disk.name.starts_with("PhysicalDrive") && !disk.bus.is_empty() && disk.size > fs.total
        );
        assert!(
            mount_of(Path::new(&sys))
                .unwrap()
                .eq_ignore_ascii_case(&sys)
        );
    }

    #[test]
    fn mount_of_missing_path_fails() {
        assert!(mount_of(Path::new("Q:\\definitely\\not\\here\\fsls")).is_err());
    }

    #[test]
    fn formats_guids() {
        // EFI system partition type, as stored on disk (first three fields little-endian).
        let raw = [
            0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e,
            0xc9, 0x3b,
        ];
        assert_eq!(guid_at(&raw, 0), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
        assert_eq!(guid_at(&raw, 1), "");
    }

    /// Real disks: the system drive's disk has a GPT/MBR table and a partition mounted at C:\.
    #[test]
    fn disks_place_volumes_on_partitions() {
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()) + "\\";
        let list = list(std::time::Duration::from_secs(10));
        let disks = disks(&list, false);
        let (d, p) = disks
            .iter()
            .find_map(|d| {
                let p = d
                    .parts
                    .iter()
                    .find(|p| p.usage.mounts.iter().any(|m| m.eq_ignore_ascii_case(&sys)))?;
                Some((d, p))
            })
            .expect("a partition mounted at the system drive");
        assert!(d.table == "gpt" || d.table == "mbr", "{d:?}");
        assert!(d.device.name.starts_with("PhysicalDrive") && d.device.size >= p.size);
        assert_eq!(p.usage.fs_type, "NTFS");
        assert!(!p.kind.is_empty() && p.name.starts_with('#'));
    }

    const SPACES: &str = "V\t4\tWarning\tDegraded,Incomplete\r
P\t4\t1\tMsft Virtual Disk\tFile Backed Virtual\tUnspecified\t5368709120\tHealthy\tOK\tAuto-Select\r
P\t4\t2\tWDC WD40EFRX\tSATA\tHDD\t4000787030016\tWarning\tLost Communication\tAuto-Select\r
P\t4\t3\tSamsung SSD 870\tSATA\tSSD\t1000204886016\tHealthy\tOK\tHot Spare\r
P\t4\t\tWDC WD40EFRX\tSATA\tHDD\t4000787030016\tWarning\tLost Communication\tAuto-Select\r
V\t7\tHealthy\tOK\r
P\t7\t5\tKINGSTON SA400\tSATA\tSSD\t480103981056\tHealthy\tOK\tAuto-Select\r
V\tnotanumber\tHealthy\tOK\r
garbage\r
";

    #[test]
    fn spaces_parser_never_panics_on_malformed_input() {
        for s in crate::testutil::inputs(SPACES, 3000) {
            let spaces = parse_spaces(&s);
            let mut fs = crate::fs::sample("S:\\", "NTFS", 1, 1);
            fs.devices = (0..8)
                .map(|n| Device {
                    name: format!("PhysicalDrive{n}"),
                    bus: "spaces".into(),
                    ..Default::default()
                })
                .collect();
            apply_spaces(&mut fs, &spaces);
        }
    }

    #[test]
    fn parses_spaces_query_output() {
        let s = parse_spaces(SPACES);
        assert_eq!(s.len(), 2);
        let degraded = &s[&4];
        assert_eq!(
            degraded.health,
            Some(RaidHealth {
                ok: false,
                text: "degraded, incomplete".into()
            })
        );
        let states: Vec<(&str, &str, &str, &str)> = degraded
            .members
            .iter()
            .map(|d| {
                (
                    d.name.as_str(),
                    d.bus.as_str(),
                    d.media.as_str(),
                    d.state.as_str(),
                )
            })
            .collect();
        assert_eq!(
            states,
            [
                ("PhysicalDrive1", "vhd", "", ""),
                ("PhysicalDrive2", "sata", "hdd", "lost communication"),
                ("PhysicalDrive3", "sata", "ssd", "spare"),
                ("missing", "sata", "hdd", "lost communication"),
            ]
        );
        assert_eq!(s[&7].health.as_ref().map(|h| h.text.as_str()), Some("ok"));
    }

    #[test]
    fn expands_spaces_disk_into_members() {
        let spaces = parse_spaces(SPACES);
        let mut fs = crate::fs::sample("S:\\", "NTFS", 10, 5);
        fs.raid = Some("spaces mirror".into());
        fs.devices = vec![
            Device {
                name: "PhysicalDrive4".into(),
                bus: "spaces".into(),
                ..Default::default()
            },
            Device {
                name: "PhysicalDrive0".into(),
                bus: "nvme".into(),
                ..Default::default()
            },
        ];
        apply_spaces(&mut fs, &spaces);
        let names: Vec<&str> = fs.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "PhysicalDrive1",
                "PhysicalDrive2",
                "PhysicalDrive3",
                "missing",
                "PhysicalDrive0"
            ]
        );
        assert_eq!(fs.raid_health.unwrap().text, "degraded, incomplete");
    }

    /// Runs the real PowerShell query: catches script syntax errors and a missing Storage module.
    #[test]
    fn spaces_query_runs() {
        let out = std::process::Command::new(powershell())
            .args(["-NoProfile", "-NonInteractive", "-Command", SPACES_QUERY])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "");
        assert!(query_spaces().is_some());
    }
}
