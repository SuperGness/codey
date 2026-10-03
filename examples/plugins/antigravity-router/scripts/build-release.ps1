param([string]$Cargo='cargo',[string]$Python='python',[ValidateSet('x86_64-pc-windows-gnu','x86_64-pc-windows-msvc')][string]$Target='x86_64-pc-windows-msvc',[string]$OutputDir=(Join-Path (Split-Path $PSScriptRoot) 'dist/windows-x64'))
$ErrorActionPreference='Stop'
$example=Split-Path $PSScriptRoot
$repo=Split-Path (Split-Path (Split-Path $example))
if([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or ![Environment]::Is64BitProcess){throw 'Portable release requires Windows x64'}
if(Test-Path -LiteralPath $OutputDir){throw 'Output directory already exists; select a new directory'}
function Run-Checked($Executable,$Arguments){& $Executable @Arguments;if($LASTEXITCODE -ne 0){throw "$Executable failed with exit $LASTEXITCODE"}}
$native=Join-Path $example 'Cargo.toml'
$proxy=Join-Path $example 'proxy-rust/Cargo.toml'
Run-Checked $Cargo @('fmt','--manifest-path',$native,'-p','codey-plugin-antigravity-router','--','--check')
Run-Checked $Cargo @('fmt','--manifest-path',$proxy,'--','--check')
$previousFlags=$env:RUSTFLAGS
try {
 if($Target -eq 'x86_64-pc-windows-gnu'){$env:RUSTFLAGS=($previousFlags+' -C link-arg=-static-libgcc -C link-arg=-static').Trim()}
 Run-Checked $Cargo @('build','--manifest-path',$native,'-p','codey-plugin-antigravity-router','--target',$Target,'--release','--locked','--target-dir',(Join-Path $repo 'target'))
 Run-Checked $Cargo @('build','--manifest-path',$proxy,'--target',$Target,'--release','--locked','--target-dir',(Join-Path $example 'proxy-rust/target'))
 Run-Checked $Python @((Join-Path $PSScriptRoot 'package-release.py'),$example,$OutputDir,'--cargo',$Cargo,'--target',$Target)
} finally {$env:RUSTFLAGS=$previousFlags}
Write-Output "Portable release built: $OutputDir"
