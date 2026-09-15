param(
    [Parameter(Mandatory = $true)][string]$SourcePath
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$outputDir = Join-Path $PSScriptRoot '../assets/icon'
$source = [System.Drawing.Image]::FromFile((Resolve-Path -LiteralPath $SourcePath).Path)
function Render-Png([int]$size) {
    $bitmap = [System.Drawing.Bitmap]::new($size, $size)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $stream = [System.IO.MemoryStream]::new()
    try {
        $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.DrawImage($source, 0, 0, $size, $size)
        $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        return ,$stream.ToArray()
    } finally { $graphics.Dispose(); $bitmap.Dispose(); $stream.Dispose() }
}
function Write-BigEndian($writer, [int]$value) {
    $bytes = [BitConverter]::GetBytes($value)
    [Array]::Reverse($bytes)
    $writer.Write($bytes)
}
try {
    $pngs = @{}
    foreach ($size in @(16, 24, 32, 48, 64, 128, 256, 512, 1024)) {
        $pngs[$size] = Render-Png $size
    }
    [System.IO.File]::WriteAllBytes((Join-Path $outputDir 'icon_1024.png'), $pngs[1024])
    $icoSizes = @(16, 24, 32, 48, 64, 128, 256)
    $writer = [System.IO.BinaryWriter]::new([System.IO.File]::Create((Join-Path $outputDir 'app.ico')))
    try {
        $writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$icoSizes.Count)
        $offset = 6 + 16 * $icoSizes.Count
        foreach ($size in $icoSizes) {
            $dimension = if ($size -eq 256) { 0 } else { $size }
            $writer.Write([byte]$dimension); $writer.Write([byte]$dimension)
            $writer.Write([byte]0); $writer.Write([byte]0)
            $writer.Write([uint16]1); $writer.Write([uint16]32)
            $writer.Write([uint32]$pngs[$size].Length); $writer.Write([uint32]$offset)
            $offset += $pngs[$size].Length
        }
        foreach ($size in $icoSizes) { $writer.Write([byte[]]$pngs[$size]) }
    } finally { $writer.Dispose() }
    # Modern ICNS PNG representations, including Retina equivalents.
    $entries = @(@('icp4',16), @('icp5',32), @('icp6',64), @('ic07',128), @('ic08',256), @('ic09',512), @('ic10',1024), @('ic11',32), @('ic12',64), @('ic13',256), @('ic14',512))
    $length = 8
    foreach ($entry in $entries) { $length += 8 + $pngs[$entry[1]].Length }
    $writer = [System.IO.BinaryWriter]::new([System.IO.File]::Create((Join-Path $outputDir 'AppIcon.icns')))
    try {
        $writer.Write([System.Text.Encoding]::ASCII.GetBytes('icns')); Write-BigEndian $writer $length
        foreach ($entry in $entries) {
            $writer.Write([System.Text.Encoding]::ASCII.GetBytes($entry[0]))
            Write-BigEndian $writer (8 + $pngs[$entry[1]].Length)
            $writer.Write([byte[]]$pngs[$entry[1]])
        }
    } finally { $writer.Dispose() }
} finally { $source.Dispose() }
