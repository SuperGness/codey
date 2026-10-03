param([Parameter(Mandatory=$true)][string]$Bundle)
$ErrorActionPreference='Stop'
$Bundle=(Resolve-Path -LiteralPath $Bundle).Path
$tempBase=[System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$root=Join-Path $tempBase ('codey-portable-'+[Guid]::NewGuid().ToString()+'-'+[char]0x4e2d+' space')
$installed=Join-Path $root 'installed'
$state=Join-Path $root 'runtime'
New-Item -ItemType Directory -Path $root | Out-Null
function Expect-Failure([scriptblock]$Action){$failed=$false;try{& $Action | Out-Null}catch{$failed=$true};if(!$failed){throw 'Expected refusal did not occur'}}
try {
 & (Join-Path $Bundle 'scripts/install.ps1') -Destination $installed
 Expect-Failure {& (Join-Path $Bundle 'scripts/install.ps1') -Destination $installed}
 Expect-Failure {& (Join-Path $Bundle 'scripts/install.ps1') -Destination (Join-Path $Bundle 'nested-install')}
 $auth=Join-Path $root 'auth.json'
 $fixture=@{antigravity=@{type='oauth';refresh='1/mock-refresh-only';access='ya29.mock-access-only';expires=([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()+86400000);accountId='portable-test';projectId='mock-project'}}
 [IO.File]::WriteAllText($auth,($fixture|ConvertTo-Json -Depth 4),(New-Object System.Text.UTF8Encoding($true)))
 $listener=New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback,0)
 $listener.Start();$port=$listener.LocalEndpoint.Port;$listener.Stop()
 & (Join-Path $installed 'start-proxy.ps1') -Port $port -AuthPath $auth -StateDir $state
 $first=Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $state 'proxy.json') | ConvertFrom-Json
 & (Join-Path $installed 'start-proxy.ps1') -Port $port -AuthPath $auth -StateDir $state
 $second=Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $state 'proxy.json') | ConvertFrom-Json
 if($first.pid -ne $second.pid){throw 'Repeated start changed PID'}
 & (Join-Path $installed 'stop-proxy.ps1') -StateDir $state
 if(Get-Process -Id $first.pid -ErrorAction SilentlyContinue){throw 'Proxy remained after stop'}
 & (Join-Path $installed 'stop-proxy.ps1') -StateDir $state
 $listener=New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback,0)
 try{$listener.Start();$port=$listener.LocalEndpoint.Port;Expect-Failure {& (Join-Path $installed 'start-proxy.ps1') -Port $port -AuthPath $auth -StateDir $state}}finally{$listener.Stop()}
 $wrong=@{pid=$PID;exePath=(Join-Path $installed 'bin/antigravity-proxy.exe');startTimeUtc=[DateTime]::UtcNow.ToString('o');port=$port}
 [IO.File]::WriteAllText((Join-Path $state 'proxy.json'),($wrong|ConvertTo-Json),(New-Object System.Text.UTF8Encoding($true)))
 Expect-Failure {& (Join-Path $installed 'stop-proxy.ps1') -StateDir $state}
 Remove-Item -LiteralPath (Join-Path $state 'proxy.json')
 Add-Content -LiteralPath (Join-Path $installed 'README.md') -Value 'tamper'
 Expect-Failure {& (Join-Path $installed 'scripts/verify-bundle.ps1')}
 Write-Output 'PASS portable: clean install / no overwrite / nested refusal / Unicode paths / idempotent start-stop / foreign port-PID / tamper refusal'
} finally {
 $recordPath=Join-Path $state 'proxy.json'
 if(Test-Path -LiteralPath $recordPath){& (Join-Path $installed 'stop-proxy.ps1') -StateDir $state}
 $resolved=[System.IO.Path]::GetFullPath($root)
 if(!$resolved.StartsWith($tempBase,[System.StringComparison]::OrdinalIgnoreCase) -or (Split-Path $resolved -Leaf) -notmatch '^codey-portable-[0-9a-f-]{36}-'){throw 'Unsafe temporary cleanup path'}
 Remove-Item -LiteralPath $resolved -Recurse -Force
}
