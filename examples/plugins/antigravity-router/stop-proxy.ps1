param([string]$StateDir=(Join-Path $PSScriptRoot '.runtime'))
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'scripts/runtime.ps1')
$recordPath=Join-Path $StateDir 'proxy.json'
if (!(Test-Path -LiteralPath $recordPath)) {Write-Output 'No recorded proxy to stop';return}
$record=Get-Content -Raw -Encoding UTF8 -LiteralPath $recordPath | ConvertFrom-Json
$proc=Get-RecordedProxy $record
if (!$proc) {Remove-Item -LiteralPath $recordPath;Write-Output 'Recorded process has exited; stale record cleared';return}
Assert-ProxyIdentity $record.port $proc.Id
Stop-Process -Id $proc.Id
if (!$proc.WaitForExit(5000)) {throw 'Proxy did not exit'}
Remove-Item -LiteralPath $recordPath
Write-Output "Stopped verified Antigravity proxy PID=$($proc.Id)"
