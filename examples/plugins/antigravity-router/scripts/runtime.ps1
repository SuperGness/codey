function ConvertTo-NativeArgument([string]$Value) {
 # Windows CommandLineToArgvW quoting, including trailing backslashes.
 '"'+[regex]::Replace([regex]::Replace($Value,'(\\*)"','$1$1\"'),'(\\+)$','$1$1')+'"'
}
function Get-RecordedProxy($Record) {
 $p=Get-Process -Id $Record.pid -ErrorAction SilentlyContinue
 if (!$p) {return $null}
 $recordTime=if($Record.startTimeUtc -is [DateTime]){$Record.startTimeUtc.ToUniversalTime()}else{[DateTime]::Parse($Record.startTimeUtc,[System.Globalization.CultureInfo]::InvariantCulture,[System.Globalization.DateTimeStyles]::RoundtripKind).ToUniversalTime()}
 if ($p.Path -ne $Record.exePath -or $p.StartTime.ToUniversalTime().Ticks -ne $recordTime.Ticks) {throw 'Recorded PID now belongs to another process; refusing operation'}
 return $p
}
function Get-ListeningProxyPids([int]$Port) {
 # Native Windows utility avoids optional PowerShell/CIM module dependencies.
 $rows=& (Join-Path $env:WINDIR 'System32/netstat.exe') -ano -p tcp
 if($LASTEXITCODE -ne 0){throw 'Could not inspect TCP port owners'}
 foreach($row in $rows) {
  $parts=$row.Trim() -split '\s+'
  if($parts.Count -ge 5 -and $parts[0] -eq 'TCP' -and $parts[3] -eq 'LISTENING' -and $parts[1] -match ':(\d+)$' -and [int]$Matches[1] -eq $Port){[int]$parts[4]}
 }
}
function Assert-ProxyIdentity([int]$Port,[int]$ExpectedPid) {
 $owners=@(Get-ListeningProxyPids $Port)
 if (!$owners.Count -or @($owners | Where-Object {$_ -ne $ExpectedPid}).Count) {throw 'Port owner does not match recorded proxy'}
 $req=[System.Net.HttpWebRequest]::Create("http://127.0.0.1:$Port/health")
 $req.Proxy=$null; $req.Timeout=2000
 $res=$req.GetResponse()
 try {$reader=New-Object System.IO.StreamReader($res.GetResponseStream());try{$health=$reader.ReadToEnd()|ConvertFrom-Json}finally{$reader.Dispose()}}finally{$res.Dispose()}
 if (!$health.ok -or $health.service -ne 'codey-antigravity-proxy') {throw 'Unexpected health service identity'}
}
