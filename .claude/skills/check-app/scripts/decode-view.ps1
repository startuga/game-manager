# Opens every picture or icon file in a folder with Windows' own decoders (WPF's BitmapDecoder,
# the WIC codecs Explorer uses) and prints what Windows sees: frames, and the first frame's size
# and pixel format. For checking that Explorer and image viewers show what the app does.
#
#   .\decode-view.ps1 -Dir 'C:\...\icons'
param([Parameter(Mandatory)][string]$Dir)

Add-Type -AssemblyName PresentationCore
$failed = 0
Get-ChildItem $Dir -File | Sort-Object Name | ForEach-Object {
    $stream = [System.IO.File]::OpenRead($_.FullName)
    try {
        $decoder = [System.Windows.Media.Imaging.BitmapDecoder]::Create($stream,
            [System.Windows.Media.Imaging.BitmapCreateOptions]::PreservePixelFormat,
            [System.Windows.Media.Imaging.BitmapCacheOption]::OnLoad)
        $frame = $decoder.Frames[0]
        '{0,-56} {1,2} frame(s)  {2,4} x {3,-4} {4}' -f $_.Name, $decoder.Frames.Count, $frame.PixelWidth, $frame.PixelHeight, $frame.Format
    } catch {
        $failed++
        $reason = if ($_.Exception.InnerException) { $_.Exception.InnerException.Message } else { $_.Exception.Message }
        '{0,-56} FAILED: {1}' -f $_.Name, $reason
    } finally {
        $stream.Dispose()
    }
}
"failed: $failed"
