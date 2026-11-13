# Helpers for checking Game Manager's window from an agent session (Windows PowerShell 5.1).
# Dot-source it at the start of a PowerShell call, then use the functions:
#
#   . .\.claude\skills\check-app\scripts\gm.ps1
#   Start-GmApp; Select-GmGame 'Crysis Remastered'; Save-GmScreen 'page'; Stop-GmApp
#
# Each PowerShell call is a new process: in a later call, Connect-GmApp finds the running agent
# build again. Only the agent build (target\agent\debug\app.exe) is ever started or closed, so the
# user's own window is never touched. Outputs go to target\agent\check.

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
if (-not ('GmNative.User32' -as [type])) {
    Add-Type -Namespace GmNative -Name User32 -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool PostMessageW(System.IntPtr hwnd, uint msg, System.UIntPtr w, System.IntPtr l);
[DllImport("user32.dll")] public static extern bool ShowWindow(System.IntPtr hwnd, int cmd);
[DllImport("user32.dll")] public static extern bool ScreenToClient(System.IntPtr hwnd, ref POINT p);
[DllImport("user32.dll")] public static extern bool GetWindowRect(System.IntPtr hwnd, out RECT r);
[DllImport("user32.dll")] public static extern bool PrintWindow(System.IntPtr hwnd, System.IntPtr hdc, uint flags);
[DllImport("user32.dll")] public static extern uint GetDpiForWindow(System.IntPtr hwnd);
public struct POINT { public int X; public int Y; }
public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
'@
}

$GmRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..')).Path
$GmExe = Join-Path $GmRoot 'target\agent\debug\app.exe'
$GmOut = Join-Path $GmRoot 'target\agent\check'
# Where the app draws itself when asked (GAME_MANAGER_CAPTURE, set by Start-GmApp).
$GmDrawingFile = Join-Path $GmOut 'drawing.tmp.png'
$GmSettings = Join-Path $env:APPDATA 'Game Manager'
New-Item -ItemType Directory -Force $GmOut | Out-Null

$GmA = [System.Windows.Automation.AutomationElement]
$GmT = [System.Windows.Automation.TreeScope]

# --- The agent build's window -------------------------------------------------------------------

# The running agent build's processes (never the user's own app.exe: matched by full path).
function Get-GmProcess {
    @(Get-Process app -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $GmExe })
}

# Starts the agent build (closing an earlier one), maximized unless -Normal, and waits for its
# library to load. Build it first: CARGO_TARGET_DIR=target/agent cargo build -p app (Bash).
function Start-GmApp([int]$LoadSeconds = 6, [switch]$Normal) {
    if (-not (Test-Path $GmExe)) { throw "No agent build at $GmExe. Build it first: CARGO_TARGET_DIR=target/agent cargo build -p app" }
    Stop-GmApp
    $env:GAME_MANAGER_CAPTURE = $GmDrawingFile
    $process = Start-Process $GmExe -PassThru
    Connect-GmApp -ProcessId $process.Id
    if (-not $Normal) { [void][GmNative.User32]::ShowWindow($global:GmHwnd, 3) }
    Start-Sleep -Seconds $LoadSeconds
    "started the agent build: process $($process.Id), $([Math]::Round((Get-GmScale), 2)) px per DIP"
}

# Finds the agent build's window (in a later PowerShell call, or after Start-GmApp).
function Connect-GmApp([int]$ProcessId = 0, [int]$TimeoutSeconds = 10) {
    if ($ProcessId -eq 0) {
        $running = Get-GmProcess | Select-Object -First 1
        if (-not $running) { throw 'The agent build is not running: Start-GmApp' }
        $ProcessId = $running.Id
    }
    $condition = New-Object System.Windows.Automation.PropertyCondition($GmA::ProcessIdProperty, $ProcessId)
    $window = $null
    for ($i = 0; $i -lt $TimeoutSeconds * 4 -and -not $window; $i++) {
        $window = $GmA::RootElement.FindFirst($GmT::Children, $condition)
        if (-not $window) { Start-Sleep -Milliseconds 250 }
    }
    if (-not $window) { throw "No window for process $ProcessId" }
    $global:GmWindow = $window
    $global:GmHwnd = [System.IntPtr]$window.Current.NativeWindowHandle
    $global:GmProcessId = $ProcessId
}

# Closes the agent build's window, as its close button would (nothing else is ever closed).
function Stop-GmApp {
    foreach ($process in Get-GmProcess) {
        $null = $process.CloseMainWindow()
        if (-not $process.WaitForExit(5000)) { Write-Warning "The agent build ($($process.Id)) did not close" }
    }
}

# Physical pixels per DIP of the window's monitor (2.25 at 225 %).
function Get-GmScale { [GmNative.User32]::GetDpiForWindow($global:GmHwnd) / 96.0 }

# --- UI Automation --------------------------------------------------------------------------------

function Get-GmElements {
    $global:GmWindow.FindAll($GmT::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
}

# The element named -Name (or matching -Like), of control type -Type (Button, ListItem, Edit,
# MenuItem, TabItem, CheckBox, Text, ...); -Index picks among repeats (names repeat: "ReShade" is a
# tab, a pill and a card). $null when there is none.
function Find-GmElement([string]$Name, [string]$Type, [string]$Like, [int]$Index = 0) {
    $found = @(Get-GmElements | Where-Object {
            (-not $Name -or $_.Current.Name -eq $Name) -and
            (-not $Like -or $_.Current.Name -like $Like) -and
            (-not $Type -or $_.Current.ControlType.ProgrammaticName -eq "ControlType.$Type")
        })
    if ($found.Count -le $Index) { return $null }
    $found[$Index]
}

# Every element (or those of one -Type): type, name, automation id, screen rectangle, disabled.
function Show-GmTree([string]$Type) {
    Get-GmElements | Where-Object { -not $Type -or $_.Current.ControlType.ProgrammaticName -eq "ControlType.$Type" } | ForEach-Object {
        $c = $_.Current
        $r = $c.BoundingRectangle
        $state = if ($c.IsEnabled) { '' } else { ' (disabled)' }
        '{0,-10} {1,-44} id={2} at {3:0},{4:0} {5:0}x{6:0}{7}' -f $c.ControlType.ProgrammaticName.Substring(12), $c.Name, $c.AutomationId, $r.X, $r.Y, $r.Width, $r.Height, $state
    }
}

function Get-GmPattern($Element, [string]$Pattern) {
    if (-not $Element) { throw "No element for the $Pattern pattern" }
    $Element.GetCurrentPattern(("System.Windows.Automation.${Pattern}Pattern" -as [type])::Pattern)
}

# Patterns act like clicks, typing and dragging (they reach the app through WM_UIA_ACTION).
function Invoke-GmElement($Element) { (Get-GmPattern $Element Invoke).Invoke(); Start-Sleep -Milliseconds 400 }
function Select-GmElement($Element) { (Get-GmPattern $Element SelectionItem).Select(); Start-Sleep -Milliseconds 400 }
function Switch-GmElement($Element) { (Get-GmPattern $Element Toggle).Toggle(); Start-Sleep -Milliseconds 400 }
function Expand-GmElement($Element) { (Get-GmPattern $Element ExpandCollapse).Expand(); Start-Sleep -Milliseconds 400 }
function Get-GmValue($Element) { (Get-GmPattern $Element Value).Current.Value }
function Set-GmValue($Element, [string]$Text) { (Get-GmPattern $Element Value).SetValue($Text); Start-Sleep -Milliseconds 400 }

# Selects a game's row in the sidebar, which opens its page.
function Select-GmGame([string]$Name) {
    $row = Find-GmElement -Name $Name -Type ListItem
    if (-not $row) { throw "No game row named '$Name'" }
    Select-GmElement $row
    Start-Sleep -Milliseconds 1100
}

# The centre of an element, in screen pixels (for Send-GmClick).
function Get-GmCenter($Element) {
    $r = $Element.Current.BoundingRectangle
    , @(($r.X + $r.Width / 2), ($r.Y + $r.Height / 2))
}

# --- Posted input (no input desktop: these reach the window as messages) --------------------------

function Send-GmMessage([int]$Message, [uint64]$WParam, [System.IntPtr]$LParam) {
    [void][GmNative.User32]::PostMessageW($global:GmHwnd, [uint32]$Message, [System.UIntPtr]::new($WParam), $LParam)
}

# Screen pixels (as UI Automation's rectangles give them) to a mouse message's lParam.
function ConvertTo-GmPoint([double]$X, [double]$Y) {
    $point = New-Object GmNative.User32+POINT
    $point.X = [int]$X; $point.Y = [int]$Y
    [void][GmNative.User32]::ScreenToClient($global:GmHwnd, [ref]$point)
    [System.IntPtr](($point.Y -shl 16) -bor ($point.X -band 0xFFFF))
}

# A left click (or -Right, -Double) at screen pixels X, Y.
function Send-GmClick([double]$X, [double]$Y, [switch]$Right, [switch]$Double) {
    $at = ConvertTo-GmPoint $X $Y
    $times = if ($Double) { 2 } else { 1 }
    for ($i = 0; $i -lt $times; $i++) {
        if ($Right) { Send-GmMessage 0x0204 2 $at; Send-GmMessage 0x0205 0 $at }
        else { Send-GmMessage 0x0201 1 $at; Send-GmMessage 0x0202 0 $at }
    }
    Start-Sleep -Milliseconds 400
}

# A drag with the left button held, in screen pixels.
function Send-GmDrag([double]$FromX, [double]$FromY, [double]$ToX, [double]$ToY, [int]$Steps = 10) {
    Send-GmMessage 0x0201 1 (ConvertTo-GmPoint $FromX $FromY)
    for ($s = 1; $s -le $Steps; $s++) {
        $x = $FromX + ($ToX - $FromX) * $s / $Steps
        $y = $FromY + ($ToY - $FromY) * $s / $Steps
        Send-GmMessage 0x0200 1 (ConvertTo-GmPoint $x $y)
        Start-Sleep -Milliseconds 40
    }
    Send-GmMessage 0x0202 0 (ConvertTo-GmPoint $ToX $ToY)
    Start-Sleep -Milliseconds 600
}

# Types text into the focused text box (WM_CHAR for each UTF-16 unit).
function Send-GmText([string]$Text) {
    foreach ($unit in $Text.ToCharArray()) { Send-GmMessage 0x0102 ([int]$unit) ([System.IntPtr]::Zero) }
    Start-Sleep -Milliseconds 400
}

# Presses a key (a virtual-key code: 0x08 Backspace, 0x0D Enter, 0x1B Escape, 0x09 Tab, 0x24 Home,
# 0x23 End, 0x25-0x28 arrows, 0x2E Delete, 0x70 F1). Ctrl and Shift can't be held this way: the app
# reads the real keyboard's state.
function Send-GmKey([int]$VirtualKey, [int]$Repeat = 1) {
    for ($i = 0; $i -lt $Repeat; $i++) {
        Send-GmMessage 0x0100 $VirtualKey ([System.IntPtr]::Zero)
        Send-GmMessage 0x0101 $VirtualKey ([System.IntPtr]::Zero)
    }
    Start-Sleep -Milliseconds 300
}

# The Menu key: WM_CONTEXTMENU from the keyboard (lParam -1), for the focused element.
function Send-GmMenuKey {
    Send-GmMessage 0x007B ([uint64]$global:GmHwnd.ToInt64()) ([System.IntPtr]::new(-1))
    Start-Sleep -Milliseconds 400
}

# --- Seeing the window ----------------------------------------------------------------------------

# The app draws its window into a PNG at one pixel per DIP (GAME_MANAGER_CAPTURE + WM_APP + 9):
# layout, text and colours, at any size; not the display's scaling. Returns the file's path.
function Save-GmDrawing([string]$Name) {
    Remove-Item $GmDrawingFile -ErrorAction SilentlyContinue
    Send-GmMessage (0x8000 + 9) 0 ([System.IntPtr]::Zero)
    for ($i = 0; $i -lt 40 -and -not (Test-Path $GmDrawingFile); $i++) { Start-Sleep -Milliseconds 200 }
    if (-not (Test-Path $GmDrawingFile)) { throw 'The app drew nothing: was it started by Start-GmApp (GAME_MANAGER_CAPTURE)?' }
    Start-Sleep -Milliseconds 300
    $file = Join-Path $GmOut "$Name.png"
    Move-Item $GmDrawingFile $file -Force
    $file
}

# The window's real pixels as Windows shows them, at the display's scaling (PrintWindow with
# PW_RENDERFULLCONTENT): for sharpness, blur and pixel-exact checks. -Crop x, y, width, height in
# DIPs from the window's top-left also saves "<Name>-crop.png". Returns the file paths.
function Save-GmScreen([string]$Name, [double[]]$Crop) {
    $rect = New-Object GmNative.User32+RECT
    [void][GmNative.User32]::GetWindowRect($global:GmHwnd, [ref]$rect)
    $bitmap = New-Object System.Drawing.Bitmap ($rect.Right - $rect.Left), ($rect.Bottom - $rect.Top)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $hdc = $graphics.GetHdc()
    $printed = [GmNative.User32]::PrintWindow($global:GmHwnd, $hdc, 2)
    $graphics.ReleaseHdc($hdc); $graphics.Dispose()
    if (-not $printed) { $bitmap.Dispose(); throw 'PrintWindow failed' }
    $file = Join-Path $GmOut "$Name.png"
    $bitmap.Save($file)
    $files = @($file)
    if ($Crop) {
        $scale = Get-GmScale
        $area = New-Object System.Drawing.Rectangle ([int]($Crop[0] * $scale)), ([int]($Crop[1] * $scale)), ([int]($Crop[2] * $scale)), ([int]($Crop[3] * $scale))
        $area.Intersect((New-Object System.Drawing.Rectangle 0, 0, $bitmap.Width, $bitmap.Height))
        $part = $bitmap.Clone($area, $bitmap.PixelFormat)
        $cropFile = Join-Path $GmOut "$Name-crop.png"
        $part.Save($cropFile); $part.Dispose()
        $files += $cropFile
    }
    $bitmap.Dispose()
    $files
}

# --- The user's settings and clipboard ------------------------------------------------------------

# Hashes of %APPDATA%\Game Manager's JSON files and own covers, as this process sees them (the
# packaged view: the agent build's writes land in the Claude package's LocalCache).
function Get-GmSettingsHashes {
    if (-not (Test-Path $GmSettings)) { return @() }
    Get-ChildItem $GmSettings -Recurse -File | Where-Object { $_.Extension -eq '.json' -or $_.DirectoryName -like '*\covers' } | Sort-Object FullName | ForEach-Object {
        '{0} {1}' -f (Get-FileHash $_.FullName -Algorithm SHA1).Hash, $_.FullName.Substring($GmSettings.Length + 1)
    }
}

# Copies the settings (packaged view) to target\agent\check\settings-backup before a check.
function Backup-GmSettings {
    $backup = Join-Path $GmOut 'settings-backup'
    Remove-Item $backup -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $backup | Out-Null
    Copy-Item (Join-Path $GmSettings '*') $backup -Recurse -Force
    Get-GmSettingsHashes | Set-Content (Join-Path $GmOut 'settings-backup.sha1') -Encoding UTF8
    "backed up $(@(Get-GmSettingsHashes).Count) settings files"
}

# Whether the settings (packaged view) are as backed up; lists what changed.
function Compare-GmSettings {
    $before = @(Get-Content (Join-Path $GmOut 'settings-backup.sha1') -Encoding UTF8)
    $after = @(Get-GmSettingsHashes)
    $changes = @(Compare-Object $before $after)
    if ($changes.Count -eq 0) { 'settings unchanged (packaged view)'; return }
    'settings CHANGED (packaged view):'
    $changes | ForEach-Object { '  {0} {1}' -f $(if ($_.SideIndicator -eq '=>') { 'now ' } else { 'was ' }), $_.InputObject }
}

# Puts the backed-up settings back (packaged view), after checking what changed.
function Restore-GmSettings {
    Copy-Item (Join-Path $GmOut 'settings-backup\*') $GmSettings -Recurse -Force
    Compare-GmSettings
}

# The clipboard is the user's: save its text before a Cut, Copy or Paste check, put it back after.
function Save-GmClipboard {
    $text = [System.Windows.Forms.Clipboard]::GetText()
    [System.IO.File]::WriteAllText((Join-Path $GmOut 'clipboard.txt'), $text, [System.Text.Encoding]::UTF8)
    "saved the clipboard's text ($($text.Length) characters)"
}

function Restore-GmClipboard {
    $text = [System.IO.File]::ReadAllText((Join-Path $GmOut 'clipboard.txt'), [System.Text.Encoding]::UTF8)
    if ($text.Length -gt 0) { [System.Windows.Forms.Clipboard]::SetText($text) } else { [System.Windows.Forms.Clipboard]::Clear() }
    "clipboard put back: $([System.Windows.Forms.Clipboard]::GetText() -eq $text)"
}
