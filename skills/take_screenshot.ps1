Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bmp = New-Object System.Drawing.Bitmap($bounds.Width, $bounds.Height)
$graphics = [System.Drawing.Graphics]::FromImage($bmp)
$graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
$desktop = [Environment]::GetFolderPath('Desktop')
$ts = Get-Date -Format 'yyyyMMdd_HHmmss'
$file = Join-Path $desktop "jarvis_screenshot_$ts.png"
$bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
$graphics.Dispose()
$bmp.Dispose()
Write-Output "Saved full-resolution desktop screenshot to $file"