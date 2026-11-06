# Contributing

Bug reports, platform quirks and pull requests are welcome. For anything larger than a fix,
please open an issue first so we can agree on the approach.

## Build and test

```
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo deny check          # advisories, licenses, sources (cargo install cargo-deny)
```

The minimum supported Rust version is 1.88 (`rust-version` in `Cargo.toml`); CI checks it.

The code is split by platform: `src/windows.rs`, `src/linux.rs` and `src/macos.rs` each
provide `list()`, `mount_of()`, `same_path()` and `disks()`. Parsers for external formats
(`mountinfo.rs`, `plist.rs`, `appleraid.rs`, `macdisks.rs`, the Storage Spaces parser in
`windows.rs`) are plain functions with unit tests that run on every platform. Each also has a
`never_panics_on_malformed_input` test fed by `testutil::inputs`: every prefix of a real
fixture, random edits of it, and random syntax noise. Add one for any new parser.

Everything printed comes from the system and may be hostile (a USB stick's label, a
directory name), so table cells and stderr messages go through `output::clean`, and JSON
escapes all control characters. Keep it that way for new output.

## Real-array tests

Unit tests use fixtures; these scripts build real arrays, degrade them, check fsls output
and clean up. CI runs all of them. They need root or admin and only touch devices they
create (loop devices, RAM disks or VHDX files):

| Script | Platform | Needs |
|---|---|---|
| `sudo bash scripts/test-md-raid.sh target/release/fsls` | Linux | root, `mdadm` |
| `sudo bash scripts/test-hung-mount.sh target/release/fsls` | Linux | root, `python3`, FUSE |
| `sudo bash scripts/test-zfs.sh target/release/fsls` | Linux | root, `zfsutils-linux`, zfs module |
| `sudo bash scripts/test-apple-raid.sh target/release/fsls` | macOS | root |
| `scripts\test-storage-spaces.ps1 target\release\fsls.exe` | Windows | elevated PowerShell |

WSL2 works for the Linux scripts (`wsl -u root`). Each prints `PASS` or a `FAIL: expected …`
line.

## Pull requests

- Keep `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test` clean.
- Add a test with the fix. For parsing bugs, add the real output to the parser's tests.
- Add a line under "Unreleased" in `CHANGELOG.md` for user-visible changes. Renaming or
  removing a JSON field is a breaking change.

## Releasing (maintainers)

1. Move the "Unreleased" notes in `CHANGELOG.md` under the new version and bump `version` in
   `Cargo.toml`.
2. Commit, then `git tag vX.Y.Z && git push origin vX.Y.Z`. The release workflow builds and
   attests the binaries and creates a draft GitHub release.
3. Review and publish the draft, then `cargo publish`.
