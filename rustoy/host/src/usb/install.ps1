Import-Module Storage
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Close Rustoy, then right-click the executable and choose Run as administrator.'
}
function Get-Target {
    $matches = @(Get-Disk | Where-Object {
        $_.UniqueId -ceq $env:RUSTOY_ID -and $_.Path -ceq $env:RUSTOY_PATH
    })
    if ($matches.Count -ne 1) { throw 'The selected USB identity is unavailable or ambiguous.' }
    $disk = $matches[0]
    if ($disk.Number -ne [uint32]$env:RUSTOY_NUMBER -or $disk.Size -ne [uint64]$env:RUSTOY_SIZE -or
        $disk.BusType -ne 'USB' -or $disk.IsBoot -or $disk.IsSystem -or $disk.IsOffline -or $disk.IsReadOnly -or $disk.IsClustered) {
        throw 'The selected disk is no longer an eligible USB drive.'
    }
    if (Get-Partition -DiskNumber $disk.Number -ErrorAction SilentlyContinue | Where-Object { $_.IsBoot -or $_.IsSystem }) {
        throw 'The selected disk contains a system or boot partition.'
    }
    return $disk
}
function Assert-LocalSource($path, $disk) {
    $file = Get-Item -LiteralPath $path
    if ($file.PSIsContainer) { throw 'Expected a regular source file.' }
    # Reject alternate path forms and reparse points, including mounted folders and junctions.
    if ($file.FullName -notmatch '^[A-Za-z]:\\') { throw 'Choose a source on a local drive.' }
    $item = $file
    while ($null -ne $item) {
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Move the source outside symbolic links and mounted folders.' }
        if ($item -is [IO.FileInfo]) { $item = $item.Directory } else { $item = $item.Parent }
    }
    $source = Get-Partition -DriveLetter $file.FullName[0]
    if ($source.DiskNumber -eq $disk.Number) { throw 'Move the source files off the USB drive before erasing it.' }
    return $file
}
$target = Get-Target
$efi = Assert-LocalSource $env:RUSTOY_EFI $target
$efiHash = (Get-FileHash -LiteralPath $efi.FullName).Hash
function Initialize-EmptyGpt {
    $disk = Get-Target
    if ($disk.PartitionStyle -eq 'GPT') {
        # Keep the existing GPT initialization and remove the old partitions.
        # Clear-Disk followed by Initialize-Disk can fail on an initialized disk.
        while ($disk.NumberOfPartitions -gt 0) {
            $partitions = @(Get-Partition -DiskNumber $disk.Number)
            if ($partitions.Count -ne $disk.NumberOfPartitions) {
                throw 'The selected USB partition list changed.'
            }
            $part = $partitions[0]
            if ($part.IsBoot -or $part.IsSystem) { throw 'The USB contains a system or boot partition.' }
            $before = $disk.NumberOfPartitions
            Remove-Partition -InputObject $part -Confirm:$false
            $disk = Get-Target
            if ($disk.NumberOfPartitions -ge $before) {
                throw 'Windows did not confirm removal of the USB partition.'
            }
        }
        return
    }
    if ($disk.PartitionStyle -eq 'MBR') {
        Clear-Disk -InputObject $disk -RemoveData -RemoveOEM -Confirm:$false
        $disk = Get-Target
    }
    if ($disk.PartitionStyle -eq 'RAW') {
        Initialize-Disk -InputObject $disk -PartitionStyle GPT
    }
    $disk = Get-Target
    if ($disk.PartitionStyle -ne 'GPT' -or $disk.NumberOfPartitions -ne 0) {
        throw 'Windows did not report an empty GPT layout on the selected USB.'
    }
}
function Assert-MountedPartition($part) {
    $disk = Get-Target
    if (!$part.DriveLetter) { throw 'Windows did not mount the new USB partition.' }
    $current = Get-Partition -DriveLetter $part.DriveLetter
    if ($current.DiskNumber -ne $disk.Number -or $current.Guid -ne $part.Guid -or
        $current.Offset -ne $part.Offset -or $current.Size -ne $part.Size) {
        throw 'A USB partition or its drive letter changed.'
    }
    return $current
}
try {
    Initialize-EmptyGpt
    $esp = New-Partition -InputObject (Get-Target) -Size 256MB -AssignDriveLetter
    $data = New-Partition -InputObject (Get-Target) -UseMaximumSize -AssignDriveLetter
    Format-Volume -Partition (Assert-MountedPartition $esp) -FileSystem FAT32 -NewFileSystemLabel RUSTOYEFI -Confirm:$false | Out-Null
    Format-Volume -Partition (Assert-MountedPartition $data) -FileSystem exFAT -NewFileSystemLabel RUSTOY -Confirm:$false | Out-Null
    $espRoot = "{0}:\" -f $esp.DriveLetter
    $null = Assert-MountedPartition $esp
    $boot = Join-Path $espRoot 'EFI\BOOT'
    [IO.Directory]::CreateDirectory($boot) | Out-Null
    $bootFile = Join-Path $boot 'BOOTX64.EFI'
    [IO.File]::Copy($efi.FullName, $bootFile, $false)
    $stream = [IO.File]::Open($bootFile, 'Open', 'Write', 'Read')
    try { $stream.Flush($true) } finally { $stream.Dispose() }
    if ((Get-FileHash -LiteralPath $bootFile).Hash -ne $efiHash) { throw 'File verification failed.' }
    $currentEsp = Assert-MountedPartition $esp
    Remove-PartitionAccessPath -InputObject $currentEsp -AccessPath $espRoot
    Set-Partition -InputObject $currentEsp -GptType '{c12a7328-f81f-11d2-ba4b-00a0c93ec93b}'
    $after = @(Get-Partition -DiskNumber (Get-Target).Number)
    $null = Assert-MountedPartition $data
    if (@($after | Where-Object { [guid]$_.GptType -eq [guid]'c12a7328-f81f-11d2-ba4b-00a0c93ec93b' }).Count -ne 1 -or
        @($after | Where-Object { [guid]$_.GptType -eq [guid]'ebd0a0a2-b9e5-4433-87c0-68b6b72699c7' }).Count -ne 1 -or
        @(Get-Volume -DriveLetter $data.DriveLetter | Where-Object FileSystem -eq 'exFAT').Count -ne 1) {
        throw 'The final USB partition layout could not be verified.'
    }
    [Console]::WriteLine('RUSTOY_INSTALL_VERIFIED')
} catch {
    throw ('USB setup did not finish. ' + $_.Exception.Message)
}
