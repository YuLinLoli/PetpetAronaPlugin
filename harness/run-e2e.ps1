$ErrorActionPreference = "Stop"
$pluginRoot = "G:\rustprojects\petpet-arona-plugin"
$harness = "$pluginRoot\harness\fake-onebot\target\release\fake-onebot.exe"
$runDir = "$pluginRoot\target\e2e\run"
$outDir = "$pluginRoot\target\e2e\artifacts"
$fwExe = "$(Split-Path $pluginRoot)\Arona-rs\target\release\arona-rs.exe"
$rounds = if ($env:ROUNDS) { $env:ROUNDS } else { "100" }
$batches = if ($env:BATCHES) { $env:BATCHES } else { "3" }
$imgSize = if ($env:IMG_SIZE) { $env:IMG_SIZE } else { "600" }
$template = if ($env:TEMPLATE) { $env:TEMPLATE } else { "memtest" }

New-Item -ItemType Directory -Force -Path $outDir | Out-Null
Get-ChildItem "$outDir" -Filter "batch*.done" -ErrorAction SilentlyContinue | Remove-Item -Force
Get-ChildItem "$outDir" -Filter "batch*.go" -ErrorAction SilentlyContinue | Remove-Item -Force

$env:ROUNDS1 = $rounds
$env:BATCHES = $batches
$env:IMG_SIZE = $imgSize
$env:TEMPLATE = $template
$env:OUTDIR = $outDir
$env:ARONA_NO_ELEVATE = "1"

$hp = Start-Process -FilePath $harness -WorkingDirectory (Split-Path $harness) -WindowStyle Hidden -PassThru -RedirectStandardOutput "$outDir\harness.log" -RedirectStandardError "$outDir\harness.err"
Start-Sleep -Seconds 1
$fp = Start-Process -FilePath $fwExe -ArgumentList "--nogui" -WorkingDirectory $runDir -WindowStyle Hidden -PassThru -RedirectStandardOutput "$outDir\fw.log" -RedirectStandardError "$outDir\fw.err"

function Sample([string]$tag) {
  $proc = Get-Process -Id $fp.Id -ErrorAction SilentlyContinue
  if (-not $proc) { Write-Output "$tag : 框架进程已退出！"; return }
  $ws = New-Object System.Collections.Generic.List[double]
  $pm = New-Object System.Collections.Generic.List[double]
  for ($i = 0; $i -lt 8; $i++) {
    $p = Get-Process -Id $fp.Id
    $ws.Add($p.WorkingSet64 / 1MB)
    $pm.Add($p.PrivateMemorySize64 / 1MB)
    Start-Sleep -Milliseconds 400
  }
  $wsS = ($ws | Sort-Object)
  $pmS = ($pm | Sort-Object)
  $wsMed = [math]::Round($wsS[[int]($wsS.Count/2)],1)
  $pmMed = [math]::Round($pmS[[int]($pmS.Count/2)],1)
  Write-Output ("{0,-9} WorkingSet 中位 {1,7} MB (min {2,7} / max {3,7})   Private 中位 {4,7} MB (min {5,7} / max {6,7})" -f `
    $tag, $wsMed, [math]::Round($wsS[0],1), [math]::Round($wsS[$wsS.Count-1],1), `
    $pmMed, [math]::Round($pmS[0],1), [math]::Round($pmS[$pmS.Count-1],1))
}

Write-Output "等待框架连接并稳定…"
Start-Sleep -Seconds 10
Sample "baseline"

for ($b = 1; $b -le [int]$batches; $b++) {
  $deadline = (Get-Date).AddMinutes(20)
  while (-not (Test-Path "$outDir\batch$b.done")) {
    if ((Get-Date) -gt $deadline) { Write-Output "batch$b 超时"; break }
    Start-Sleep -Seconds 2
  }
  Start-Sleep -Seconds 6
  Sample "batch$b"
  New-Item -ItemType File -Force -Path "$outDir\batch$b.go" | Out-Null
}

Stop-Process -Id $fp.Id -Force -ErrorAction SilentlyContinue
Stop-Process -Id $hp.Id -Force -ErrorAction SilentlyContinue
Write-Output "已结束"