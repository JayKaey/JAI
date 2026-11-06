Get-Process | Sort-Object WorkingSet64 -Descending | Select-Object -First 6 | ForEach-Object {
    $mb = [math]::Round($_.WorkingSet64 / 1MB, 1)
    "$($_.ProcessName) (PID $($_.Id)): $mb MB RAM"
} | Out-String