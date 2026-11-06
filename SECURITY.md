# Security policy

## Reporting a vulnerability

Please do not open a public issue. Report it privately through GitHub:
**Security → Report a vulnerability** on
[github.com/gyazbek/fsls](https://github.com/gyazbek/fsls/security/advisories/new).

Include the fsls version (`fsls --version`), your OS and steps to reproduce. You should get a
reply within a week. Fixes are released as a patch version and credited in the advisory
unless you prefer otherwise.

## Supported versions

Only the latest release receives fixes.

## Scope

fsls only reads system state and runs no network code, so the main risks are in how it
starts external programs (`powershell.exe` on Windows, `diskutil` on macOS) and how it
parses their output and kernel-provided data. Reports about those are especially welcome.
