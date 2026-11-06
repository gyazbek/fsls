# Changelog

All notable changes are listed here. The project follows [Semantic Versioning](https://semver.org);
the `--json` output is part of the public interface.

## Unreleased

## 0.1.0

First release.

- List mounted file systems on Windows, Linux and macOS with size, usage, type, kind and bus.
  Use% is computed like `df` (used / (used + available)) and only reads 100% when full.
- Resolve each file system to its physical disks (model, SSD/HDD, bus, size) with `-d`.
- Software RAID layout and health: Linux md and btrfs, Windows Storage Spaces, macOS AppleRAID.
  Degraded arrays are reported on stderr.
- Per-mount `--timeout` so dead network mounts cannot hang the listing.
- Filters (`-t`, `-x`, `-k`, `--bus`), sorting, `--total`, and `--json` output. `--total`
  counts shared storage once: an APFS container, a ZFS pool's free space.
- `fsls disks`: every physical disk and partition, including blank and unmounted ones, with
  partition type, file system, mounts and what each partition belongs to (md, LVM, LUKS, APFS
  container, AppleRAID, Storage Spaces).
- `--warn[=PCT]`: warn about file systems at least PCT% full and exit with 2 when anything
  needs attention (also degraded arrays and timed-out mounts).
- Encryption status: BitLocker, LUKS and FileVault (`ENC` column, `encryption` in JSON).
- Inode usage with `-i` (Linux, macOS), and in `--warn`.
- `-o/--columns` to choose columns, and `--csv` output.
- Colour on terminals (`--color`, honours `NO_COLOR`).
- Windows mapped network drives show their `\\server\share`, `subst` drives their folder.
- `fsls completions <shell>` and `fsls man`; release archives include both, and releases
  attach a Homebrew formula and a Scoop manifest.
- Windows binaries need no Visual C++ runtime and load system DLLs only from System32.
