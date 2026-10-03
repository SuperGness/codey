param([ValidateRange(1,65535)][int]$Port=8787,[string]$AuthPath=(Join-Path $env:USERPROFILE '.pi/agent/auth.json'),[string]$StateDir=(Join-Path $PSScriptRoot '.runtime'),[string]$ExePath='', [switch]$UseSystemProxy)
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'scripts/runtime.ps1')
if (!$ExePath) { $ExePath=Join-Path $PSScriptRoot 'bin/antigravity-proxy.exe'; if (!(Test-Path -LiteralPath $ExePath)) {$ExePath=Join-Path $PSScriptRoot 'proxy-rust/target/release/antigravity-proxy.exe'} }
$ExePath=(Resolve-Path -LiteralPath $ExePath).Path
$AuthPath=(Resolve-Path -LiteralPath $AuthPath).Path
if ((& $ExePath --version) -ne 'antigravity-proxy 0.9.0') {throw 'Unexpected proxy version'}
if ($LASTEXITCODE -ne 0) {throw 'Proxy version check failed'}
New-Item -ItemType Directory -Force -Path $StateDir | Out-Null
$StateDir=(Resolve-Path -LiteralPath $StateDir).Path
$recordPath=Join-Path $StateDir 'proxy.json'
if (Test-Path -LiteralPath $recordPath) {
 $record=Get-Content -Raw -Encoding UTF8 -LiteralPath $recordPath | ConvertFrom-Json
 $proc=Get-RecordedProxy $record
 if ($proc) {if ($record.port -ne $Port -or $record.exePath -ne $ExePath -or $record.authPath -ne $AuthPath) {throw 'Existing recorded proxy has different settings'}; Assert-ProxyIdentity $Port $proc.Id; Write-Output "Already running PID=$($proc.Id) port=$Port"; return}
 Remove-Item -LiteralPath $recordPath
}
if (@(Get-ListeningProxyPids $Port).Count) {throw "Port $Port is occupied by another process; nothing was stopped"}
# ProcessStartInfo explicitly inherits the calling process environment on PS5/PS7.
$info=New-Object System.Diagnostics.ProcessStartInfo
$info.FileName=$ExePath
$info.UseShellExecute=$false
$info.CreateNoWindow=$true
$info.WindowStyle=[System.Diagnostics.ProcessWindowStyle]::Hidden
$info.WorkingDirectory=$StateDir
$info.Arguments='serve --port '+$Port+' --auth '+(ConvertTo-NativeArgument $AuthPath)
$info.EnvironmentVariables['PI_CODING_AGENT_DIR']=Split-Path $AuthPath
$info.EnvironmentVariables['AG_IMAGE_DIR']=Join-Path $StateDir 'images'
foreach($name in @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY')) {
 $value=[Environment]::GetEnvironmentVariable($name)
 if($value){$info.EnvironmentVariables[$name]=$value}
}
if($UseSystemProxy -and !$info.EnvironmentVariables['HTTPS_PROXY']) {
 $settings=Get-ItemProperty -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings'
 if($settings.ProxyEnable -eq 1 -and $settings.ProxyServer) {
  $proxy=[string]$settings.ProxyServer
  if($proxy -match '(^|;)https=([^;]+)'){$proxy=$Matches[2]}
  elseif($proxy -match '(^|;)http=([^;]+)'){$proxy=$Matches[2]}
  elseif($proxy.Contains('=')){throw 'System proxy format is unsupported; set HTTPS_PROXY explicitly'}
  if($proxy -notmatch '^https?://'){$proxy='http://'+$proxy}
  $uri=[uri]$proxy
  if(!$uri.IsAbsoluteUri -or $uri.Scheme -notin @('http','https') -or !$uri.Host -or $uri.UserInfo){throw 'Invalid system proxy; set HTTPS_PROXY explicitly'}
  $info.EnvironmentVariables['HTTPS_PROXY']=$proxy
  if(!$info.EnvironmentVariables['HTTP_PROXY']){$info.EnvironmentVariables['HTTP_PROXY']=$proxy}
 }
}
$bypass=$info.EnvironmentVariables['NO_PROXY']
$info.EnvironmentVariables['NO_PROXY']=(@($bypass,'127.0.0.1','localhost','::1') | Where-Object {$_}) -join ','
$proc=[System.Diagnostics.Process]::Start($info)
$record=@{pid=$proc.Id;exePath=$ExePath;authPath=$AuthPath;startTimeUtc=$proc.StartTime.ToUniversalTime().ToString('o');port=$Port}
try {
 $ready=$false
 for ($i=0;$i -lt 50;$i++) {if ($proc.HasExited) {throw 'Proxy exited during startup'}; try {Assert-ProxyIdentity $Port $proc.Id; $ready=$true;break} catch {Start-Sleep -Milliseconds 200}}
 if (!$ready) {throw 'Proxy health identity check timed out (check Pi Google login)'}
 [System.IO.File]::WriteAllText($recordPath,($record | ConvertTo-Json),(New-Object System.Text.UTF8Encoding($true)))
 Write-Output "Started verified Antigravity proxy PID=$($proc.Id) port=$Port"
} catch { $owned=Get-RecordedProxy ([pscustomobject]$record);if ($owned){Stop-Process -Id $owned.Id; $owned.WaitForExit(5000)|Out-Null};throw }
