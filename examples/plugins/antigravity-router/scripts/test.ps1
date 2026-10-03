param([string]$Cargo='cargo',[string]$Python='python',[string]$Node='node')
$ErrorActionPreference='Stop'
$example=Split-Path $PSScriptRoot
$repo=Split-Path (Split-Path (Split-Path $example))
function Run-Checked($Executable,$Arguments){& $Executable @Arguments;if($LASTEXITCODE -ne 0){throw "$Executable failed with exit $LASTEXITCODE"}}
Run-Checked $Cargo @('build','--manifest-path',(Join-Path $example 'Cargo.toml'),'-p','codey-plugin-antigravity-router','--locked','--target-dir',(Join-Path $repo 'target'))
Run-Checked $Cargo @('test','--manifest-path',(Join-Path $example 'Cargo.toml'),'-p','codey-plugin-antigravity-router','--locked','--target-dir',(Join-Path $repo 'target'))
Run-Checked $Cargo @('test','--manifest-path',(Join-Path $example 'proxy-rust/Cargo.toml'),'--locked')
Run-Checked $Cargo @('build','--manifest-path',(Join-Path $example 'proxy-rust/Cargo.toml'),'--release','--locked')
Run-Checked $Node @((Join-Path $example 'tests/e2e-rust-proxy.test.mjs'))
