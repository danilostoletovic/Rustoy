Import-Module Storage
Get-Disk | Where-Object {
    $_.BusType -eq 'USB' -and !$_.IsBoot -and !$_.IsSystem -and !$_.IsOffline -and
    !$_.IsReadOnly -and !$_.IsClustered -and $_.UniqueId -and $_.Path -and $_.Size -ge 512MB
} | ForEach-Object {
    $parts = Get-Partition -DiskNumber $_.Number -ErrorAction SilentlyContinue
    if ($parts | Where-Object { $_.IsBoot -or $_.IsSystem }) { return }
    $fields = @($_.Number, $_.Size, $_.UniqueId, $_.Path, ($_.FriendlyName -replace '[\x00-\x1f]', ' '))
    if (!( ($fields[2..3] -join '') -match '[\x00-\x1f]' )) {
        [Console]::WriteLine(($fields -join "`t"))
    }
}
