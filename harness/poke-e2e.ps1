param(
  [int]$Pokes = 6,
  [int]$GapMs = 3500,
  [string]$Label = "poke",
  [ValidateSet("on","off")][string]$Switch = "on",
  [string]$PreCmd = "",
  [ValidateSet("","poke","silent")][string]$Expect = "",
  [long]$GroupId = 123456789
)
$ErrorActionPreference = "Stop"
$pluginRoot = "G:\rustprojects\petpet-arona-plugin"
$harness = "$pluginRoot\harness\fake-onebot\target\release\fake-onebot.exe"
$runDir = "$pluginRoot\target\e2e\run"
$outDir = "$pluginRoot\target\e2e\artifacts\$Label"
$fwExe = "$(Split-Path $pluginRoot)\Arona-rs\target\release\arona-rs.exe"
$enc = New-Object System.Text.UTF8Encoding($false)

New-Item -ItemType Directory -Force -Path $outDir | Out-Null
Get-ChildItem "$outDir" -Include "batch*","poke*" -ErrorAction SilentlyContinue | ForEach-Object { [System.IO.File]::Delete($_.FullName) }

# 插件的戳一戳配置：开启 + 概率 1.0 + 冷却清零，保证一戳就回
$ppCfg = "$runDir\config\petpet\arona.yml"
$pp = @"
petpet:
  command: pet
  cooldown_time: 0
  user_cooldown_time: 0
  in_cooldown_message: ''
  nudge_enabled: true
  nudge_probability: 1.0
  global_cooldown_time: 0
  avatar_size: 640
  max_frames: 120
  max_side: 1200
  cache_entries: 8
  cache_bytes: 16777216
  font_path: 'C:\\WINDOWS\\Fonts\\taile.ttf'
"@
[System.IO.File]::WriteAllText($ppCfg, ($pp -replace "`r`n","`n"), $enc)

# 框架配置：Switch=off 时把本群 petpet.nudge 关掉
$fwCfg = "$runDir\config\arona.yml"
$fc = [System.IO.File]::ReadAllText($fwCfg, $enc) -replace "`r`n","`n"
$block = if ($Switch -eq "off") {
"group_settings:`n  `"$GroupId`":`n    disabled_features: [petpet.nudge]"
} else {
"group_settings: {}"
}
if ($fc -notmatch '(?m)^group_settings:.*$') { throw "arona.yml missing group_settings line" }
$fc = [regex]::Replace($fc, '(?m)^group_settings:.*$', { param($m) $block })
[System.IO.File]::WriteAllText($fwCfg, $fc, $enc)

$dll = "$runDir\plugins\petpet\petpet_plugin.dll"
$dllOff = "$runDir\plugins\petpet\petpet_plugin.dll.off"
if (Test-Path $dllOff) { Move-Item -LiteralPath $dllOff -Destination $dll -Force }

$env:ROUNDS1 = "$Pokes"
$env:BATCHES = "1"
$env:IMG_SIZE = "320"
$env:IMAGE_MODE = "base64"
$env:WAIT = "0"
$env:SLEEP_MS = "$GapMs"
$env:POKE = "1"
$env:PRECMD = $PreCmd
$env:TEXT = ""
$env:OUTDIR = $outDir
$env:ARONA_NO_ELEVATE = "1"

$hp = Start-Process -FilePath $harness -WorkingDirectory (Split-Path $harness) -WindowStyle Hidden -PassThru -RedirectStandardOutput "$outDir\harness.log" -RedirectStandardError "$outDir\harness.err"
Start-Sleep -Seconds 1
$fp = Start-Process -FilePath $fwExe -ArgumentList "--nogui" -WorkingDirectory $runDir -WindowStyle Hidden -PassThru -RedirectStandardOutput "$outDir\fw.log" -RedirectStandardError "$outDir\fw.err"

$deadline = (Get-Date).AddSeconds(40)
while ((Get-Date) -lt $deadline) {
  if ((Test-Path "$outDir\harness.log") -and ((Get-Content "$outDir\harness.log" -Raw) -match "\[ws\]")) { break }
  Start-Sleep -Milliseconds 500
}
Start-Sleep -Seconds 8
New-Item -ItemType File -Force -Path "$outDir\batch0.go" | Out-Null

$total = [int](($Pokes * $GapMs) / 1000) + 40
$dl = (Get-Date).AddSeconds($total)
$finalR = 0; $finalA = 0; $baseR = 0; $log = ""
while ((Get-Date) -lt $dl) {
  $log = if (Test-Path "$outDir\harness.log") { Get-Content "$outDir\harness.log" -Raw } else { "" }
  if ($log -match "\[stat\] replies=(\d+) actions=(\d+)") { $finalR = [int]$Matches[1]; $finalA = [int]$Matches[2]; break }
  Start-Sleep -Seconds 1
}
if ($log -match "\[base\] replies=(\d+)") { $baseR = [int]$Matches[1] }
Start-Sleep -Seconds 2
Stop-Process -Id $fp.Id -Force -ErrorAction SilentlyContinue
Stop-Process -Id $hp.Id -Force -ErrorAction SilentlyContinue

$pokeReplies = $finalR - $baseR
$want = if ($Expect) { $Expect } elseif ($Switch -eq "off") { "silent" } else { "poke" }
"[$Label] switch=$Switch precmd='$PreCmd' expect=$want pokes=$Pokes baseReplies=$baseR totalReplies=$finalR pokeReplies=$pokeReplies actions=$finalA"
if ($want -eq "poke" -and $pokeReplies -gt 0) { "POKE E2E OK (=> replied)"; exit 0 }
if ($want -eq "silent" -and $pokeReplies -eq 0) { "POKE E2E OK (=> silent)"; exit 0 }
"POKE E2E FAILED"; exit 1