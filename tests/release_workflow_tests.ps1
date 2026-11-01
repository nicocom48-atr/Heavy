param()
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

Write-Host "==> Testing HeavyDrop Release Automation" -ForegroundColor Cyan

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$PrepareScript = Join-Path $RepoRoot '.github/scripts/prepare-release.ps1'

if (-not (Test-Path $PrepareScript)) {
    throw "prepare-release.ps1 script not found at $PrepareScript"
}

# 1. Test Cargo.toml and Cargo.lock parsing in real workspace
Write-Host "--> Verifying package manifest regexes in repository..."
$manifest = [System.IO.File]::ReadAllText((Join-Path $RepoRoot 'Cargo.toml'))
$packagePattern = '(?ms)(^\[package\]\r?\n.*?^version\s*=\s*")([^"\r\n]+)(")'
$packageMatch = [regex]::Match($manifest, $packagePattern)
if (-not $packageMatch.Success) {
    throw "Failed to extract package version from Cargo.toml"
}
$currentVersion = $packageMatch.Groups[2].Value
Write-Host "    Current Cargo.toml version: $currentVersion"

$lock = [System.IO.File]::ReadAllText((Join-Path $RepoRoot 'Cargo.lock'))
$lockPattern = '(?ms)(^\[\[package\]\]\r?\nname = "heavydrop"\r?\nversion = ")([^"\r\n]+)(")'
$lockMatches = [regex]::Matches($lock, $lockPattern)
if ($lockMatches.Count -ne 1) {
    throw "Expected exactly 1 heavydrop package in Cargo.lock, found $($lockMatches.Count)"
}
Write-Host "    Current Cargo.lock version: $($lockMatches[0].Groups[2].Value)"

# 2. Test version tag calculations
Write-Host "--> Testing version calculation and validation..."
$testTags = @('v0.2.5', 'v0.2.6', 'v0.2.7')
$versions = @([version]$currentVersion) + @($testTags | ForEach-Object { [version]$_.Substring(1) })
$latest = $versions | Sort-Object -Descending | Select-Object -First 1
$expectedNext = "v$($latest.Major).$($latest.Minor).$($latest.Build + 1)"
if ($expectedNext -notmatch '^v\d+\.\d+\.\d+$') {
    throw "Expected version format vMAJOR.MINOR.PATCH, got $expectedNext"
}
Write-Host "    Next automated tag from tags ($($testTags -join ', ')): $expectedNext"

# 3. Test prepare-release.ps1 in an isolated temporary repository
Write-Host "--> Testing prepare-release.ps1 in isolated mock repository..."
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("heavydrop_release_test_" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

try {
    $gitCmd = (Get-Command -CommandType Application git).Source
    function Run-Git([string[]]$Arguments) {
        $res = & $gitCmd @Arguments
        if ($LASTEXITCODE -ne 0) { throw "Git failed: $($Arguments -join ' ')" }
        return $res
    }

    Push-Location -LiteralPath $tempDir
    Run-Git @('init', '-b', 'main') | Out-Null
    Run-Git @('config', 'user.name', 'test-bot') | Out-Null
    Run-Git @('config', 'user.email', 'bot@example.com') | Out-Null

    # Create dummy Cargo.toml and Cargo.lock
    $dummyManifest = "[package]`nname = `"heavydrop`"`nversion = `"0.2.6`"`nedition = `"2021`"`n"
    $dummyLock = "[[package]]`nname = `"heavydrop`"`nversion = `"0.2.6`"`n`n[[package]]`nname = `"other`"`nversion = `"1.0.0`"`n"
    [System.IO.File]::WriteAllText((Join-Path $tempDir 'Cargo.toml'), $dummyManifest)
    [System.IO.File]::WriteAllText((Join-Path $tempDir 'Cargo.lock'), $dummyLock)

    Run-Git @('add', 'Cargo.toml', 'Cargo.lock') | Out-Null
    Run-Git @('commit', '-m', 'Initial commit') | Out-Null
    Run-Git @('tag', 'v0.2.6') | Out-Null

    # Make a new unreleased commit on main
    Run-Git @('commit', '--allow-empty', '-m', 'Feature to be released') | Out-Null
    $initialSha = (Run-Git @('rev-parse', 'HEAD')).Trim()

    $outputFile = Join-Path $tempDir 'output.env'
    $envFile = Join-Path $tempDir 'github.env'
    $env:GITHUB_OUTPUT = $outputFile
    $env:GITHUB_ENV = $envFile
    $env:GITHUB_EVENT_NAME = 'push'
    $env:GITHUB_REF = 'refs/heads/main'

    # Run prepare-release for mock repo
    & $PrepareScript -RepositoryRoot $tempDir
    if ($LASTEXITCODE -ne 0) { throw "prepare-release.ps1 failed in mock repo" }

    $outContent = Get-Content -Raw -LiteralPath $outputFile
    $envContent = Get-Content -Raw -LiteralPath $envFile

    if ($outContent -notmatch 'release_exists=false') {
        throw "Expected release_exists=false, got: $outContent"
    }
    if ($envContent -notmatch 'RELEASE_TAG=v0\.2\.7') {
        throw "Expected RELEASE_TAG=v0.2.7, got: $envContent"
    }

    # Verify updated files in mock repo
    $updatedManifest = [System.IO.File]::ReadAllText((Join-Path $tempDir 'Cargo.toml'))
    if ($updatedManifest -notmatch 'version = "0\.2\.7"') {
        throw "Cargo.toml in mock repo was not bumped to 0.2.7"
    }
    $updatedLock = [System.IO.File]::ReadAllText((Join-Path $tempDir 'Cargo.lock'))
    if ($updatedLock -notmatch 'name = "heavydrop"\r?\nversion = "0\.2\.7"') {
        throw "Cargo.lock in mock repo was not bumped to 0.2.7"
    }
    # Verify other package was untouched
    if ($updatedLock -notmatch 'name = "other"\r?\nversion = "1\.0\.0"') {
        throw "Unrelated package in Cargo.lock was unexpectedly modified"
    }

    # Second run: test idempotency on re-running on original push commit
    Run-Git @('checkout', $initialSha) | Out-Null
    Clear-Content -LiteralPath $outputFile
    Clear-Content -LiteralPath $envFile
    & $PrepareScript -RepositoryRoot $tempDir
    $secondOut = Get-Content -Raw -LiteralPath $outputFile
    if ($secondOut -notmatch 'release_exists=true') {
        throw "Expected idempotent release_exists=true on source commit, got: $secondOut"
    }

    # Third run: test on tagged commit directly
    Run-Git @('checkout', 'v0.2.7') | Out-Null
    Clear-Content -LiteralPath $outputFile
    Clear-Content -LiteralPath $envFile
    & $PrepareScript -RepositoryRoot $tempDir
    $thirdOut = Get-Content -Raw -LiteralPath $outputFile
    if ($thirdOut -notmatch 'release_exists=true') {
        throw "Expected idempotent release_exists=true on tagged commit, got: $thirdOut"
    }
    Write-Host "    Mock repo automatic bump and idempotency passed!"
} finally {
    Pop-Location
    Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    $env:GITHUB_OUTPUT = $null
    $env:GITHUB_ENV = $null
    $env:GITHUB_EVENT_NAME = $null
    $env:GITHUB_REF = $null
}

Write-Host "==> All release workflow tests passed successfully!" -ForegroundColor Green
