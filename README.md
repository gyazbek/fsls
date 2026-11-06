# fsls

`ls` for file systems. One command on Windows, Linux and macOS that lists what is mounted,
how full it is, which physical disks it lives on, and whether the RAID under it is healthy.

```
$ fsls
MOUNT  LABEL       FS    KIND   BUS   SIZE  USED  AVAIL  USE%
C:\    Enterprise  NTFS  fixed  nvme  1.8T  1.8T   9.9G   99% [#########.]
D:\    Arrakis     NTFS  fixed  nvme  3.6T  2.3T   1.3T   64% [######....]
J:\    Jupiter     NTFS  fixed  nvme  931G  614G   318G   66% [#######...]
```

A Storage Spaces mirror that just lost one of its disks:

```
> fsls -d E:\
MOUNT               LABEL              FS    KIND                BUS  SIZE  USED  AVAIL  USE%               RAID
E:\                 fslsraid           NTFS  fixed               vhd  3.0G   17M   3.0G    1% [..........]  spaces mirror: degraded, incomplete
  > missing         Msft Virtual Disk  ssd   lost communication  vhd  5.0G
  > PhysicalDrive3  Msft Virtual Disk  ssd                       vhd  5.0G
fsls: warning: E:\: spaces mirror array is degraded, incomplete
```

With `-d`, the indented rows are the disks: model under LABEL, ssd/hdd under FS, and the
disk's RAID member state (faulty, rebuilding, spare, ...) under KIND.

## Install

| Method | Command |
|---|---|
| Cargo (from source) | `cargo install fsls` |
| Cargo (prebuilt) | `cargo binstall fsls` |
| Download | Archives for Windows, Linux and macOS (x86_64 and arm64) on the [releases page](https://github.com/gyazbek/fsls/releases) |

Release binaries are not code-signed. Each release has a `SHA256SUMS` file, and every
archive carries a GitHub build attestation proving it was built from this repository by its
release workflow:

```
gh attestation verify fsls-v0.1.0-x86_64-unknown-linux-musl.tar.gz --repo gyazbek/fsls
```

Browsers mark downloaded files, so Windows SmartScreen may say "unknown publisher" and macOS
may refuse to open a quarantined binary. Installing with `cargo` avoids that, and so does
downloading from a terminal, e.g. on Apple Silicon:

```
curl -fsSL https://github.com/gyazbek/fsls/releases/download/v0.1.0/fsls-v0.1.0-aarch64-apple-darwin.tar.gz | tar xz
./fsls-v0.1.0-aarch64-apple-darwin/fsls
```

For a file you already downloaded in a browser, `xattr -d com.apple.quarantine fsls` clears the
macOS quarantine flag.

Each archive also contains shell completions (`completions/`) and a man page (`man/fsls.1`).
With a `cargo` install, generate them instead:

```
fsls completions bash > ~/.local/share/bash-completion/completions/fsls
fsls completions zsh  > ~/.zfunc/_fsls
fsls completions fish > ~/.config/fish/completions/fsls.fish
fsls completions powershell >> $PROFILE
fsls man > ~/.local/share/man/man1/fsls.1
```

Every release also attaches a ready Homebrew formula (`fsls.rb`) and Scoop manifest
(`fsls.json`) with the archives' hashes filled in.

## Usage

```
fsls                    # mounted file systems
fsls PATH...            # only the file systems holding these paths
fsls -d                 # plus the physical disks under each, and RAID layout/health
fsls -l                 # long listing: RAID, encryption, disks, UUID/serial, options, source
fsls -i                 # inode usage instead of space, like df -i
fsls -a                 # include hidden ones (proc/sysfs, system volumes, empty drives)
fsls -o mount,use,enc   # just these columns
fsls --json / --csv     # machine-readable output
fsls --warn[=PCT]       # exit 2 if anything is PCT% full (default 90), degraded or hung
fsls disks              # every disk and partition, including unmounted and blank ones
```

Filters and sorting: `-t ext4,ntfs`, `-x tmpfs`, `-k fixed,removable`, `--bus nvme,usb`,
`-s size|used|avail|use|bus|...`, `-r`. Sizes: `-b` (bytes), `--si` (powers of 1000).
`--total` adds a total row; volumes of one APFS container and datasets of one ZFS pool share
their free space, so it is counted once. Run `fsls --help` for everything.

Columns for `-o`: `mount`, `label`, `fs`, `kind`, `bus`, `size`, `used`, `avail`, `use`,
`inodes`, `iused`, `ifree`, `iuse`, `raid`, `enc`, `disks`, `id`, `maxname`, `options`,
`source`. `--csv` prints raw values (bytes, plain percentages) with a header row.

USE% is used / (used + available), the same as `df`: space reserved for root (5% on ext4)
counts as gone, so a file system you can no longer write to shows 100%. Rounding never
shows 100% (or a full bar) while space is left, nor 0% while anything is stored.

Output is coloured on a terminal: usage green / yellow (75%) / red (90%), unhealthy RAID in
red. `--color=never` or the `NO_COLOR` environment variable turns it off; `--color=always`
keeps it when piping.

Encryption (`ENC`) shows `bitlocker`, `luks` or `filevault`. On Windows and macOS it costs a
short system query, so it is only fetched when shown (`-l`, `-o ...,enc`, `--json`).

Problems are reported on stderr even in a plain listing: a degraded or failed array, and
any mount that did not answer within `--timeout` seconds (default 2). A dead NFS or SMB share
shows as `timed out` instead of hanging the whole listing.

### Disks

`fsls disks` lists physical disks rather than file systems, like `lsblk`: partition table,
each partition's type, file system, label and mount points, and what it is part of. Blank
disks show as `unused`, and partitions that belong to an md array, LVM, LUKS, an APFS
container, an AppleRAID set or a Storage Spaces pool show what they belong to, e.g.
`-> md0 (raid1) -> vg0-data (lvm): /srv`. `-a` adds loop devices and disk images.

```
> fsls disks
NAME            SIZE  TYPE         FS     LABEL       DETAILS
PhysicalDrive0  3.6T  gpt                             WD_BLACK SN850X 4000GB (nvme, ssd)
  #1             16M  MS reserved                     -
  #2            3.6T  Basic data   NTFS   Arrakis     D:\
PhysicalDrive2  1.8T  gpt                             Samsung SSD 990 PRO 2TB (nvme, ssd)
  #1            499M  Recovery     NTFS               not mounted
  #2            100M  EFI          FAT32              not mounted
  #3            128M  MS reserved                     -
  #4            1.8T  Basic data   NTFS   Enterprise  C:\
```

DETAILS is the device on disk rows and what is on the partition on partition rows.

### Monitoring

`--warn` (or `--warn=80`) prints a warning for every file system at least that full (by space
or by inodes) and exits with 2 if it found anything, including degraded arrays and timed-out
mounts. Read-only file systems and optical media are skipped, since they are always full. For
cron or a health check:

```
fsls --warn=85 > /dev/null || echo "disk attention needed"
```

Exit status: 0 ok, 1 a PATH argument could not be resolved, 2 `--warn` found a problem.

## What it detects

| | Windows | Linux | macOS |
|---|---|---|---|
| Mounts | Volumes, drive letters, folder mounts, network drives | `/proc/self/mountinfo` | `getmntinfo` |
| Disks under a volume | Volume extents → `PhysicalDriveN` | Resolves LVM, LUKS, md and partitions down to disks | APFS containers → physical stores |
| Bus / model / SSD-HDD | Storage device descriptor | sysfs | `diskutil` |
| Software RAID | Storage Spaces (members + health), dynamic disks (`multi-disk`) | md (level, health, rebuild %, member state), btrfs profiles | AppleRAID (members + health) |
| Encryption | BitLocker (Shell property, as Explorer shows it) | LUKS / dm-crypt in the device stack | FileVault (`diskutil info`) |
| Inodes | - (NTFS has no fixed inode table) | `statvfs` | `statfs` |
| `fsls disks` | Drive layout (GPT/MBR) + volume extents | sysfs + udev database | `diskutil list` |
| Needs admin/root | No | No | No |

Not covered yet: ZFS pool health and member disks (datasets are listed and totalled), the
RAID level of LVM-RAID volumes (shown as `multi-disk`),
the members of hardware RAID arrays (the controller hides them from the OS), and the layout
of Windows dynamic-disk volumes (span, stripe and mirror all show as `multi-disk`).

## What it does on your system

fsls only reads. It never writes to disks, changes settings or uses the network. Besides
operating-system calls it runs at most two external programs, always by absolute path:

- **Windows:** `powershell.exe` (Storage module), and only when a Storage Spaces disk exists,
  to read pool members and health. BitLocker status is read in-process through the Shell
  property system, the same value Explorer shows; no admin rights are needed.
- **macOS:** `/usr/sbin/diskutil`, to map volumes to disks, list partitions, and read AppleRAID
  and FileVault state.

Linux runs no external programs.

## JSON output

`fsls --json` prints an array with one object per file system: `mount`, `mounts`, `source`,
`label`, `fs_type`, `kind`, `ready`, `timed_out`, `*_bytes`, `use_percent`, `id`,
`max_name_length`, `inodes_total` / `inodes_used` / `inodes_free` (null where not
applicable), `encryption`, `options`, `raid`, `raid_health` (`{ok, text}`) and `devices`
(`{name, bus, model, media, size_bytes, state}`).

`fsls disks --json` prints one object per disk: `name`, `bus`, `model`, `media`, `size_bytes`,
`table`, `fs_type`, `label`, `mounts`, `used_by` and `partitions` (each with `name`, `type`,
`size_bytes`, `fs_type`, `label`, `mounts`, `used_by`).

The JSON output is a stable interface: fields may be added in any release, but removing or
renaming one is a breaking change (see [CHANGELOG.md](CHANGELOG.md)). Values that are prose,
such as `raid_health.text`, may change wording; use `raid_health.ok` in scripts.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: see [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual
licensed as above, without any additional terms or conditions.
