# Packages target\release\jarvis.exe as JARVIS.exe in the project folder (next to models\ and
# runtime\), together with the DLLs it loads, so the app does not depend on anything installed
# system-wide. Called by build.bat.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$src = Join-Path $root 'target\release\jarvis.exe'
$dst = Join-Path $root 'JARVIS.exe'

if (Get-Process -Name 'JARVIS' -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $dst }) {
    throw 'J.A.R.V.I.S. is running. Quit it first ("Jarvis, shut down", or tray icon > Quit).'
}

# 1. The executable. It is built for the Windows GUI subsystem (windows_subsystem in main.rs),
#    so no console window ever opens; checked here so a regression cannot slip through.
$bytes = [IO.File]::ReadAllBytes($src)
$pe = [BitConverter]::ToInt32($bytes, 0x3C)
if ([BitConverter]::ToUInt32($bytes, $pe) -ne 0x00004550) { throw "$src is not a PE executable" }
$subsystem = [BitConverter]::ToUInt16($bytes, $pe + 4 + 20 + 68)   # "PE\0\0" + COFF header + Subsystem offset
if ($subsystem -ne 2) { throw "$src is a console program (subsystem $subsystem); it would open a terminal window" }
Copy-Item $src $dst -Force
Write-Host "JARVIS.exe        $([math]::Round($bytes.Length / 1MB, 1)) MB (native window, no console)"

# 2. DirectML (ONNX Runtime GPU provider) and the Visual C++ runtime, deployed app-locally.
Copy-Item (Join-Path $root 'target\release\DirectML.dll') $root -Force
$crt = Get-ChildItem "$env:ProgramFiles\Microsoft Visual Studio\*\*\VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT" -Directory -ErrorAction SilentlyContinue |
    Sort-Object FullName -Descending | Select-Object -First 1
if ($crt) {
    foreach ($dll in 'msvcp140.dll', 'msvcp140_1.dll', 'vcruntime140.dll', 'vcruntime140_1.dll') {
        Copy-Item (Join-Path $crt.FullName $dll) $root -Force
    }
    Write-Host "runtime DLLs      DirectML + Visual C++ ($($crt.Parent.Parent.Name))"
} else {
    Write-Warning 'Visual C++ redistributable files not found; JARVIS.exe will use the system-wide runtime.'
}

# 3. Built-in engines, copied once from local installs when missing.
$engine = Join-Path $root 'runtime\ollama'
if (-not (Test-Path (Join-Path $engine 'ollama.exe'))) {
    $installed = Join-Path $env:LOCALAPPDATA 'Programs\Ollama'
    if (Test-Path (Join-Path $installed 'ollama.exe')) {
        # Without the tray app, the uninstaller and the AMD-only ROCm libraries.
        robocopy $installed $engine /E /XF 'ollama app.exe' 'unins000.*' 'app.ico' /XD 'rocm*' /NFL /NDL /NJH /NJS /NP | Out-Null
        Write-Host "runtime\ollama    copied from $installed"
    } else {
        Write-Warning 'runtime\ollama is missing: extract an Ollama Windows release (ollama.exe + lib\ollama) there.'
    }
}
$espeak = Join-Path $root 'runtime\espeak-ng'
if (-not (Test-Path (Join-Path $espeak 'libespeak-ng.dll'))) {
    $installed = Join-Path $env:ProgramFiles 'eSpeak NG'
    if (Test-Path (Join-Path $installed 'libespeak-ng.dll')) {
        robocopy $installed $espeak /E /NFL /NDL /NJH /NJS /NP | Out-Null
        Write-Host "runtime\espeak-ng copied from $installed"
    } else {
        Write-Warning 'runtime\espeak-ng is missing: Jarvis falls back to its basic built-in phonemizer.'
    }
}
exit 0
