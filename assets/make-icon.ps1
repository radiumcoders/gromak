# Draws gromak's icon (a dwindle layout of tiles) and writes assets/gromak.ico
# with PNG-compressed images at every size Windows asks for.
#   powershell -NoProfile -File assets/make-icon.ps1
Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'

function RoundedRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = [Math]::Min($r * 2, [Math]::Min($w, $h))
    if ($d -le 0.5) { $p.AddRectangle((New-Object System.Drawing.RectangleF $x, $y, $w, $h)); return $p }
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    $p
}

function Draw([int]$s) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'; $g.PixelOffsetMode = 'HighQuality'; $g.Clear([System.Drawing.Color]::Transparent)
    $u = $s / 256.0
    # Background: a dark rounded square with a soft vertical gradient.
    $bg = RoundedRect (6 * $u) (6 * $u) (244 * $u) (244 * $u) (56 * $u)
    $rect = New-Object System.Drawing.RectangleF 0, 0, $s, $s
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, ([System.Drawing.Color]::FromArgb(255, 0x2a, 0x2d, 0x3e)), ([System.Drawing.Color]::FromArgb(255, 0x16, 0x17, 0x20)), 90
    $g.FillPath($grad, $bg)
    # Tiles: dwindle (left half, then the right half split again, ...).
    $pad = 40 * $u; $gap = [Math]::Max(1.0, 14 * $u); $r = [Math]::Max(1.0, 16 * $u)
    $x0 = $pad; $y0 = $pad; $w = $s - 2 * $pad; $h = $s - 2 * $pad
    $lw = ($w - $gap) / 2; $rh = ($h - $gap) / 2
    $tiles = @(
        @($x0, $y0, $lw, $h, 0xc6d6fa),
        @(($x0 + $lw + $gap), $y0, $lw, $rh, 0x8aadf4)
    )
    if ($s -ge 32) {
        $bw = ($lw - $gap) / 2
        $tiles += , @(($x0 + $lw + $gap), ($y0 + $rh + $gap), $bw, $rh, 0x7dc4e4)
        $tiles += , @(($x0 + $lw + $gap + $bw + $gap), ($y0 + $rh + $gap), $bw, $rh, 0xb7bdf8)
    } else {
        $tiles += , @(($x0 + $lw + $gap), ($y0 + $rh + $gap), $lw, $rh, 0x7dc4e4)
    }
    foreach ($t in $tiles) {
        $c = [System.Drawing.Color]::FromArgb(255, ($t[4] -shr 16) -band 255, ($t[4] -shr 8) -band 255, $t[4] -band 255)
        $b = New-Object System.Drawing.SolidBrush $c
        $g.FillPath($b, (RoundedRect $t[0] $t[1] $t[2] $t[3] $r))
    }
    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    if ($s -eq 256) { $bmp.Save((Join-Path $PSScriptRoot 'gromak.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
    $bmp.Dispose()
    , $ms.ToArray()
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 128, 256
$images = foreach ($s in $sizes) { , (Draw $s) }
$out = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter $out
$bw.Write([UInt16]0); $bw.Write([UInt16]1); $bw.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $len = $images[$i].Length
    $bw.Write([byte]($s % 256)); $bw.Write([byte]($s % 256)); $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([UInt16]1); $bw.Write([UInt16]32); $bw.Write([UInt32]$len); $bw.Write([UInt32]$offset)
    $offset += $len
}
foreach ($img in $images) { $bw.Write($img) }
[System.IO.File]::WriteAllBytes((Join-Path $PSScriptRoot 'gromak.ico'), $out.ToArray())
"wrote gromak.ico ($($sizes -join ', ') px)"
