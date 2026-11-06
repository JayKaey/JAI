$q = [uri]::EscapeDataString("$env:JARVIS_PARAM_QUERY")
$eng = "$env:JARVIS_PARAM_ENGINE".ToLower()
$url = switch ($eng) {
    "youtube" { "https://www.youtube.com/results?search_query=$q" }
    "github" { "https://github.com/search?q=$q" }
    "wikipedia" { "https://en.wikipedia.org/w/index.php?search=$q" }
    default { "https://www.google.com/search?q=$q" }
}
Start-Process $url
Write-Output "Opened search for '$env:JARVIS_PARAM_QUERY' on $url"