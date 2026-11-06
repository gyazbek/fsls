//! Human names for GPT partition-type GUIDs and MBR partition-type bytes.

/// `ebd0a0a2-b9e5-4433-87c0-68b6b72699c7` -> `Basic data`. Case-insensitive.
pub fn gpt(guid: &str) -> Option<&'static str> {
    let g = guid.to_ascii_lowercase();
    Some(match g.as_str() {
        "c12a7328-f81f-11d2-ba4b-00a0c93ec93b" => "EFI",
        "21686148-6449-6e6f-744e-656564454649" => "BIOS boot",
        "e3c9e316-0b5c-4db8-817d-f92df00215ae" => "MS reserved",
        "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7" => "Basic data",
        "de94bba4-06d1-4d40-a16a-bfd50179d6ac" => "Recovery",
        "5808c8aa-7e8f-42e0-85d2-e1e90434cfb3" => "LDM metadata",
        "af9b60a0-1431-4f62-bc68-3311714a69ad" => "LDM data",
        "e75caf8f-f680-4cee-afa3-b001e56efc2d" => "Storage Spaces",
        "0fc63daf-8483-4772-8e79-3d69d8477de4" => "Linux",
        "4f68bce3-e8cd-4db1-96e7-fbcaf984b709" => "Linux root",
        "b921b045-1df0-41c3-af44-4c6f280d3fae" => "Linux root",
        "933ac7e1-2eb4-4f13-b844-0e14e2aef915" => "Linux home",
        "bc13c2ff-59e6-4262-a352-b275fd6f7172" => "Linux boot",
        "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f" => "Linux swap",
        "e6d6d379-f507-44c2-a23c-238f2a3df928" => "Linux LVM",
        "a19d880f-05fc-4d3b-a006-743f0f84911e" => "Linux RAID",
        "ca7d7ccb-63ed-4c53-861c-1742536059cc" => "LUKS",
        "6a898cc3-1dd2-11b2-99a6-080020736631" => "ZFS",
        "7c3457ef-0000-11aa-aa11-00306543ecac" => "APFS",
        "48465300-0000-11aa-aa11-00306543ecac" => "HFS+",
        "52414944-0000-11aa-aa11-00306543ecac" => "AppleRAID",
        _ => return None,
    })
}

/// MBR partition-type byte -> name.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn mbr(t: u8) -> Option<&'static str> {
    Some(match t {
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0e => "FAT16",
        0x07 => "NTFS/exFAT",
        0x0b | 0x0c => "FAT32",
        0x05 | 0x0f | 0x85 => "Extended",
        0x27 => "Recovery",
        0x42 => "LDM",
        0x82 => "Linux swap",
        0x83 => "Linux",
        0x8e => "Linux LVM",
        0xa5 => "FreeBSD",
        0xaf => "HFS+",
        0xee => "GPT protective",
        0xef => "EFI",
        0xfd => "Linux RAID",
        _ => return None,
    })
}

/// udev's `ID_PART_ENTRY_TYPE`: a GPT GUID, or `0x83` for MBR. Unknown types are returned as-is.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn name(id: &str) -> String {
    let known = match id.strip_prefix("0x") {
        Some(hex) => u8::from_str_radix(hex, 16).ok().and_then(mbr),
        None => gpt(id),
    };
    known.map_or_else(|| id.to_string(), String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            gpt("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"),
            Some("Basic data")
        );
        assert_eq!(gpt("00000000-0000-0000-0000-000000000000"), None);
        assert_eq!(mbr(0x83), Some("Linux"));
        assert_eq!(name("0xfd"), "Linux RAID");
        assert_eq!(name("c12a7328-f81f-11d2-ba4b-00a0c93ec93b"), "EFI");
        assert_eq!(name("0x99"), "0x99");
    }
}
