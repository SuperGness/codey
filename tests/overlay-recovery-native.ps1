# Windows PowerShell 5.1: compiles the shipped native helper without changing windows.
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition ([IO.File]::ReadAllText((Join-Path $PSScriptRoot '../backend/resources/overlay-recovery/PetWindowAgent.cs')))
$flags = [Reflection.BindingFlags]'NonPublic,Static'
$windowType = [PetWindowAgent].GetNestedType('WindowInfo', [Reflection.BindingFlags]::NonPublic)
$stable = [PetWindowAgent].GetMethod('Stable', $flags)
$a = [Activator]::CreateInstance($windowType, $true)
$b = [Activator]::CreateInstance($windowType, $true)
if (-not $stable.Invoke($null, @($a, $b))) { throw 'Identical snapshots must be stable' }
foreach ($name in @('Handle', 'Pid', 'Start', 'Style', 'X', 'Y', 'Width', 'Height')) {
    $field = $windowType.GetField($name)
    $field.SetValue($b, [Convert]::ChangeType(1, $field.FieldType))
    if ($stable.Invoke($null, @($a, $b))) { throw "Changed $name must prevent repair" }
    $field.SetValue($b, [Convert]::ChangeType(0, $field.FieldType))
}
Write-Output 'Native helper compiles; window identity and geometry guards passed.'
