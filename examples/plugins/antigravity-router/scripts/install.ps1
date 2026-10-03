param([Parameter(Mandatory=$true)][string]$Destination)
$ErrorActionPreference='Stop'
$bundle=Split-Path $PSScriptRoot
$bundle=[System.IO.Path]::GetFullPath($bundle).TrimEnd([char]'\',[char]'/')
$Destination=[System.IO.Path]::GetFullPath($Destination).TrimEnd([char]'\',[char]'/')
if($Destination -eq $bundle -or $Destination.StartsWith($bundle+[System.IO.Path]::DirectorySeparatorChar,[System.StringComparison]::OrdinalIgnoreCase)){throw 'Destination must be outside the release bundle'}
if((Test-Path -LiteralPath $Destination) -and ((Get-Item -Force -LiteralPath $Destination).Attributes -band [System.IO.FileAttributes]::ReparsePoint)){throw 'Destination cannot be a link'}
if (Test-Path -LiteralPath $Destination) {if (@(Get-ChildItem -Force -LiteralPath $Destination).Count) {throw 'Destination must be a clean directory'}} else {New-Item -ItemType Directory -Path $Destination | Out-Null}
& (Join-Path $PSScriptRoot 'verify-bundle.ps1') -Bundle $bundle
Get-ChildItem -Force -LiteralPath $bundle | Copy-Item -Destination $Destination -Recurse
Write-Output "Portable files installed: $Destination"
Write-Output 'Next: import the .codey-plugin via Codey Plugins UI; import stays disabled until you explicitly enable. This script does not modify Codey state.'
