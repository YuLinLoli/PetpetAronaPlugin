param(
  [int]$Rounds = 100,
  [int]$Batches = 2,
  [int]$ImgSize = 600,
  [string]$ImageMode = "base64",
  [string]$Text = "",
  [int]$Wait = 1,
  [int]$SleepMs = 60,
  [string]$Template = "memtest",
  [string]$Label = "run",
  [string]$Plugin = "on"
)
$ErrorActionPreference = "Stop"
$pluginRoot = "G:\rustprojects\petpet-arona-plugin"
$harness = "$pluginRoot\harness\fake-onebot\target\release\fake-onebot.exe"
$runDir = "$pluginRoot\target\e2e\run"
$outDir = "$pluginRoot\target\e2e\artifacts\$Label"
$fwExe = "C:\Users\cheng\RustroverProjects\Arona-rs\target\release\arona-rs.exe"

New-Item -ItemType Directory -Force -Path $outDir | Out-Null
Get-ChildItem "$outDir" -Include "batch*" -ErrorAction SilentlyContinue | ForEach-Object { [System.IO.File]::Delete($_.FullName) }

$dll = "$runDir\plugins\petpet\petpet_plugin.dll"
$dllOff = "$runDir\plugins\petpet\petpet_plugin.dll.off"
if ($Plugin -eq "off") {
  if (Test-Path $dll) { Move-Item -LiteralPath $dll -Destination $dllOff -Force }
} else {
  if (Test-Path $dllOff) { Move-Item -LiteralPath $dllOff -Destination $dll -Force }
}

$env:ROUNDS1 = "$Rounds"
$env:BATCHES = "$Batches"
$env:IMG_SIZE = "$ImgSize"
$env:TEMPLATE = $Template
$env:IMAGE_MODE = $ImageMode
$env:TEXT = $Text
$env:WAIT = "$Wait"
$env:SLEEP_MS = "$SleepMs"
$env:OUTDIR = $outDir
$env:ARONA_NO_ELEVATE = "1"

$hp = Start-Process -FilePath $harness -WorkingDirectory (Split-Path $harness) -WindowStyle Hidden -PassThru -RedirectStandardOutput "$outDir\harness.log" -RedirectStandardError "$outDir\harness.err"
Start-Sleep -Seconds 1
$fp = Start-Process -FilePath $fwExe -ArgumentList "--nogui" -WorkingDirectory $runDir -WindowStyle Hidden -PassThru -RedirectStandardOutput "$outDir\fw.log" -RedirectStandardError "$outDir\fw.err"
"pid=$($fp.Id) harness=$($hp.Id)" | Set-Content "$outDir\pids.txt"

function Sample([string]$tag) {
  $proc = Get-Process -Id $fp.Id -ErrorAction SilentlyContinue
  if (-not $proc) { Write-Output "$tag : FRAMEWORK EXITED"; return }
  $ws = New-Object System.Collections.Generic.List[double]
  $pm = New-Object System.Collections.Generic.List[double]
  for ($i = 0; $i -lt 8; $i++) {
    $p = Get-Process -Id $fp.Id
    $ws.Add($p.WorkingSet64 / 1MB)
    $pm.Add($p.PrivateMemorySize64 / 1MB)
    Start-Sleep -Milliseconds 350
  }
  $wsS = ($ws | Sort-Object); $pmS = ($pm | Sort-Object)
  Write-Output ("[{0}] {1,-9} WS med {2,7} MB (min {3,7}/max {4,7})  Priv med {5,7} MB" -f `
    $Label, $tag, [math]::Round($wsS[[int]($wsS.Count/2)],1), [math]::Round($wsS[0],1), [math]::Round($wsS[$wsS.Count-1],1), [math]::Round($pmS[[int]($pmS.Count/2)],1))
}

$deadline = (Get-Date).AddSeconds(30)
while ((Get-Date) -lt $deadline) {
  if ((Test-Path "$outDir\harness.log") -and ((Get-Content "$outDir\harness.log" -Raw) -match "已连接")) { break }
  Start-Sleep -Milliseconds 500
}
Start-Sleep -Seconds 8
Sample "baseline"
New-Item -ItemType File -Force -Path "$outDir\batch0.go" | Out-Null

for ($b = 1; $b -le $Batches; $b++) {
  $dl = (Get-Date).AddMinutes(20)
  while (-not (Test-Path "$outDir\batch$b.done")) {
    if ((Get-Date) -gt $dl) { Write-Output "batch$b TIMEOUT"; break }
    Start-Sleep -Seconds 2
  }
  Start-Sleep -Seconds 6
  Sample "batch$b"
  New-Item -ItemType File -Force -Path "$outDir\batch$b.go" | Out-Null
}

Stop-Process -Id $fp.Id -Force -ErrorAction SilentlyContinue
Stop-Process -Id $hp.Id -Force -ErrorAction SilentlyContinue
Write-Output "done $Label"