Get-PSDrive -PSProvider FileSystem | Where-Object { $_.Used -gt 0 -or $_.Free -gt 0 } | ForEach-Object {
    $freeGb = [math]::Round($_.Free / 1GB, 1)
    $usedGb = [math]::Round($_.Used / 1GB, 1)
    $totalGb = $freeGb + $usedGb
    "Drive $($_.Name):\ -> $freeGb GB free of $totalGb GB total"
} | Out-String