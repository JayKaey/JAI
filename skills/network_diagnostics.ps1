$ips = Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254*' } | Select-Object -ExpandProperty IPAddress
$ping = Test-Connection -ComputerName 1.1.1.1 -Count 1 -ErrorAction SilentlyContinue
$ms = if ($ping) { "$($ping.ResponseTime) ms" } else { "Offline" }
Write-Output "LAN IPv4: $($ips -join ', ') | Ping (1.1.1.1): $ms"