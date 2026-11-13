# Lists an .ico file's directory (size, colour count, bit count) beside what each image's own
# header says (PNG: size, bit depth and colour type; bitmap: size and bits a pixel). Read only.
#
#   .\ico-file.ps1 -File 'C:\...\game.ico'
param([Parameter(Mandatory)][string]$File)

$bytes = [System.IO.File]::ReadAllBytes($File)
if ([BitConverter]::ToUInt16($bytes, 0) -ne 0 -or [BitConverter]::ToUInt16($bytes, 2) -ne 1) { throw "$File is not an .ico file" }
$count = [BitConverter]::ToUInt16($bytes, 4)
"$File : $count images"
for ($i = 0; $i -lt $count; $i++) {
    $entry = 6 + 16 * $i
    $side = $bytes[$entry]; if ($side -eq 0) { $side = 256 }
    $colours = $bytes[$entry + 2]
    $bits = [BitConverter]::ToUInt16($bytes, $entry + 6)
    $size = [BitConverter]::ToUInt32($bytes, $entry + 8)
    $at = [BitConverter]::ToUInt32($bytes, $entry + 12)
    if ($bytes[$at] -eq 0x89 -and $bytes[$at + 1] -eq 0x50) {
        $width = [int]$bytes[$at + 18] * 256 + $bytes[$at + 19]
        $height = [int]$bytes[$at + 22] * 256 + $bytes[$at + 23]
        $own = 'PNG {0} x {1}, depth {2}, colour type {3}' -f $width, $height, $bytes[$at + 24], $bytes[$at + 25]
    } else {
        $own = 'bitmap {0} x {1}, {2} bits' -f [BitConverter]::ToInt32($bytes, $at + 4), ([BitConverter]::ToInt32($bytes, $at + 8) / 2), [BitConverter]::ToUInt16($bytes, $at + 14)
    }
    '{0,2}: listed {1,3} px, {2,3} colours, {3,2} bits | {4} ({5} bytes)' -f $i, $side, $colours, $bits, $own, $size
}
