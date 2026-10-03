param([string]$Bundle=(Split-Path $PSScriptRoot))
$ErrorActionPreference='Stop'
$root=(Resolve-Path -LiteralPath $Bundle).Path.TrimEnd([char]'\',[char]'/')
$prefix=$root+[System.IO.Path]::DirectorySeparatorChar
$sumFile=Join-Path $root 'SHA256SUMS'
$listed=@{}
foreach($line in Get-Content -Encoding UTF8 -LiteralPath $sumFile) {
 if($line -notmatch '^([a-f0-9]{64})  (.+)$'){throw 'Malformed checksum file'}
 $expected=$Matches[1];$relative=$Matches[2]
 if($relative -match '(^/|[\\:]|(^|/)\.\.(/|$)|(^|/)\.(/|$))' -or $relative -eq 'SHA256SUMS' -or $listed.ContainsKey($relative)){throw 'Unsafe or duplicate checksum path'}
 $p=[System.IO.Path]::GetFullPath((Join-Path $root $relative))
 if(!$p.StartsWith($prefix,[System.StringComparison]::OrdinalIgnoreCase)){throw 'Checksum path escapes bundle'}
 $listed[$relative]=$true
 $algorithm=[System.Security.Cryptography.SHA256]::Create();$stream=[System.IO.File]::OpenRead($p)
 try {$actual=[BitConverter]::ToString($algorithm.ComputeHash($stream)).Replace('-','').ToLowerInvariant()} finally {$stream.Dispose();$algorithm.Dispose()}
 if($actual -ne $expected){throw "Checksum mismatch: $relative"}
}
$files=@(Get-ChildItem -LiteralPath $root -Recurse -Force)
foreach($file in $files) {
 if($file.Attributes -band [System.IO.FileAttributes]::ReparsePoint){throw 'Links are not allowed in a release bundle'}
 if(!$file.PSIsContainer) {
  $relative=$file.FullName.Substring($prefix.Length).Replace('\','/')
  if($relative -ne 'SHA256SUMS' -and !$listed.ContainsKey($relative)){throw "Unlisted bundle file: $relative"}
 }
}
if ((& (Join-Path $root 'bin/antigravity-proxy.exe') --version) -ne 'antigravity-proxy 0.9.0'){throw 'Unexpected executable version'}
if($LASTEXITCODE -ne 0){throw 'Proxy version check failed'}
Write-Output 'Portable bundle exact file list, SHA256 and executable version verified'
