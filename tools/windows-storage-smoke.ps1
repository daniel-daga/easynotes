param(
    [string]$Executable = "$PSScriptRoot/../app/build/windows/x64/runner/Release/easynotes.exe"
)
$ErrorActionPreference = 'Stop'
$binary = (Resolve-Path -LiteralPath $Executable).Path
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$testDirectory = Join-Path $tempRoot ('easynotes-storage-smoke-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testDirectory | Out-Null

function Run-Phase([string]$phase) {
    $process = Start-Process -FilePath $binary -WorkingDirectory (Split-Path $binary) `
        -ArgumentList @($phase, ('"' + $testDirectory + '"')) -WindowStyle Hidden -PassThru
    if (-not $process.WaitForExit(20000)) {
        $process.Kill()
        throw "$phase timed out"
    }
    $process.Refresh()
    $result = Join-Path $testDirectory "result-$phase.txt"
    if ($process.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $result)) {
        $failure = Join-Path $testDirectory 'failure.txt'
        if (Test-Path -LiteralPath $failure) { Get-Content -LiteralPath $failure }
        throw "$phase failed (exit $($process.ExitCode))"
    }
    if ((Get-Content -LiteralPath $result -Raw) -ne 'ok') { throw "Invalid $phase result" }
    Write-Host "ok    $phase in a separate native Windows process"
}

try {
    Run-Phase 'write'
    Run-Phase 'read'
} finally {
    # Delete only our namespaced key; never deleteAll or the app's master key.
    Run-Phase 'cleanup'
    $resolved = (Resolve-Path -LiteralPath $testDirectory).Path
    if ((Split-Path $resolved -Parent) -ne $tempRoot.TrimEnd('\') -or
        (Split-Path $resolved -Leaf) -notmatch '^easynotes-storage-smoke-[a-f0-9]{32}$') {
        throw 'Refusing to remove a directory outside the temporary test root'
    }
    Remove-Item -LiteralPath $resolved -Recurse
}
