#Requires -RunAsAdministrator
<#
Real Storage Spaces check for fsls: creates two VHDX files, pools them, builds a two-way
mirror, checks fsls shows its members and "ok", detaches one member, checks "degraded",
then removes everything. Only touches the VHDX files it creates.

Usage (elevated):  scripts\test-storage-spaces.ps1 target\release\fsls.exe
#>
param([Parameter(Mandatory)][string]$Fsls)

$ErrorActionPreference = 'Stop'
$Fsls = (Resolve-Path $Fsls).Path
$dir = Join-Path $env:TEMP 'fsls-spaces-test'
$pool = 'fsls-test-pool'
$vdName = 'fsls-test-mirror'
$vhds = 1..2 | ForEach-Object { Join-Path $dir "member$_.vhdx" }

$script:out = ''
function Run([string[]]$fslsArgs) {
    # fsls prints warnings on stderr. Windows PowerShell 5.1 turns redirected native stderr
    # into error records, which 'Stop' would make fatal, so relax it and stringify them.
    $ErrorActionPreference = 'Continue'
    $script:out = (& $Fsls @fslsArgs 2>&1 | ForEach-Object { "$_" } | Out-String)
    Write-Host $script:out
}
function Expect([string]$re) {
    if ($script:out -notmatch $re) { throw "FAIL: expected /$re/" }
}

function Cleanup {
    $ErrorActionPreference = 'Continue'
    # Re-attach a detached member first so the pool can be removed cleanly.
    foreach ($v in $vhds) {
        if ((Test-Path $v) -and -not (Get-DiskImage -ImagePath $v).Attached) {
            Mount-DiskImage -ImagePath $v -NoDriveLetter | Out-Null
        }
    }
    Get-VirtualDisk -FriendlyName $vdName -ErrorAction SilentlyContinue | Remove-VirtualDisk -Confirm:$false
    Get-StoragePool -FriendlyName $pool -ErrorAction SilentlyContinue | Remove-StoragePool -Confirm:$false
    foreach ($v in $vhds) { if (Test-Path $v) { Dismount-DiskImage -ImagePath $v | Out-Null } }
    Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
}

try {
    New-Item -ItemType Directory $dir -Force | Out-Null
    foreach ($v in $vhds) {
        # diskpart creates a VHDX without needing the Hyper-V module.
        "create vdisk file=`"$v`" maximum=5120 type=expandable" | Set-Content "$dir\create.txt" -Encoding ascii
        diskpart /s "$dir\create.txt" | Out-Null
        if (-not (Test-Path $v)) { throw "diskpart did not create $v" }
        Mount-DiskImage -ImagePath $v -NoDriveLetter | Out-Null
    }
    # Exactly our two disks, found through their image files.
    $members = foreach ($v in $vhds) {
        $n = (Get-DiskImage -ImagePath $v | Get-Disk).Number
        Get-PhysicalDisk | Where-Object { $_.DeviceId -eq "$n" -and $_.CanPool }
    }
    if (@($members).Count -ne 2) { throw "expected 2 poolable VHDX disks, got $(@($members).Count)" }

    $subsystem = (Get-StorageSubSystem -FriendlyName 'Windows Storage*').FriendlyName
    New-StoragePool -FriendlyName $pool -StorageSubSystemFriendlyName $subsystem -PhysicalDisks $members | Out-Null
    $vd = New-VirtualDisk -StoragePoolFriendlyName $pool -FriendlyName $vdName -ResiliencySettingName Mirror `
        -NumberOfDataCopies 2 -ProvisioningType Thin -Size 3GB
    $vol = $vd | Get-Disk | Initialize-Disk -PartitionStyle GPT -PassThru |
        New-Partition -AssignDriveLetter -UseMaximumSize |
        Format-Volume -FileSystem NTFS -NewFileSystemLabel fslsraid -Confirm:$false
    $drive = "$($vol.DriveLetter):\"

    Write-Host "`n=== healthy two-way mirror on $drive"
    Run @('-d', $drive)
    Expect 'spaces mirror: ok'
    Expect '(?m)^\s+> PhysicalDrive\d+\s.*\svhd\s'
    if (([regex]::Matches($script:out, '(?m)^\s+> PhysicalDrive')).Count -ne 2) { throw 'FAIL: expected 2 member disks' }

    Write-Host "`n=== member $($vhds[1]) detached"
    Dismount-DiskImage -ImagePath $vhds[1] | Out-Null
    Start-Sleep -Seconds 5
    Run @('-d', $drive)
    Expect 'spaces mirror: (degraded|incomplete|warning)'
    Expect 'warning: .*array is'

    Write-Host "`nPASS"
}
finally {
    Cleanup
}
