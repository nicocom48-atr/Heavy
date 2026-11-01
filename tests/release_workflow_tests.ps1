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
$savedEnvironment = @{}
foreach ($key in @('GITHUB_OUTPUT', 'GITHUB_ENV', 'GITHUB_EVENT_NAME', 'GITHUB_REF', 'GITHUB_REF_TYPE', 'INPUT_TAG')) {
    $savedEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
}

try {
    $gitCmd = (@(Get-Command -CommandType Application git)[0]).Source
    if (-not $gitCmd) { $gitCmd = 'git.exe' }
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
    if ($secondOut -notmatch 'release_exists=false') {
        throw "A source-commit retry must rebuild and publish its existing version: $secondOut"
    }
    if ((Run-Git @('tag', '--list', 'v*')).Count -ne 2) { throw 'Retry created an extra release tag' }
    if ((Run-Git @('rev-parse', 'HEAD')).Trim() -ne (Run-Git @('rev-parse', 'v0.2.7^{commit}')).Trim()) {
        throw 'Retry did not check out the prepared release commit'
    }
    if ((Get-Content -Raw -LiteralPath $envFile) -notmatch 'RELEASE_TAG=v0\.2\.7') { throw 'Retry changed the release version' }

    # Third run: test on tagged commit directly
    Run-Git @('checkout', 'v0.2.7') | Out-Null
    Clear-Content -LiteralPath $outputFile
    Clear-Content -LiteralPath $envFile
    & $PrepareScript -RepositoryRoot $tempDir
    $thirdOut = Get-Content -Raw -LiteralPath $outputFile
    if ($thirdOut -notmatch 'release_exists=false') {
        throw "A tagged-commit retry must rebuild and publish its existing version: $thirdOut"
    }
    # A different merge must receive the next version rather than reusing the previous one.
    Run-Git @('commit', '--allow-empty', '-m', 'Next feature') | Out-Null
    Clear-Content -LiteralPath $outputFile
    Clear-Content -LiteralPath $envFile
    & $PrepareScript -RepositoryRoot $tempDir
    if ((Get-Content -Raw -LiteralPath $envFile) -notmatch 'RELEASE_TAG=v0\.2\.8') {
        throw 'A new source commit did not receive the next version'
    }

    # Manual releases can use an already-updated manifest without a staged version diff.
    (Get-Content -Raw -LiteralPath (Join-Path $tempDir 'Cargo.toml')).Replace('0.2.8', '0.2.9') |
        Set-Content -LiteralPath (Join-Path $tempDir 'Cargo.toml')
    (Get-Content -Raw -LiteralPath (Join-Path $tempDir 'Cargo.lock')).Replace('0.2.8', '0.2.9') |
        Set-Content -LiteralPath (Join-Path $tempDir 'Cargo.lock')
    Run-Git @('add', 'Cargo.toml', 'Cargo.lock') | Out-Null
    Run-Git @('commit', '-m', 'Set explicit release version') | Out-Null
    $manualSha = (Run-Git @('rev-parse', 'HEAD')).Trim()
    $env:GITHUB_EVENT_NAME = 'workflow_dispatch'
    $env:GITHUB_REF_TYPE = 'branch'
    $env:INPUT_TAG = 'v0.2.9'
    & $PrepareScript -RepositoryRoot $tempDir
    if ((Run-Git @('rev-parse', 'v0.2.9^{commit}')).Trim() -ne $manualSha) {
        throw 'Manual release unexpectedly changed a matching manifest'
    }
    Write-Host "    Automatic increments, same-version retries and manual releases passed!"
} finally {
    Pop-Location
    $cleanupPath = [System.IO.Path]::GetFullPath($tempDir)
    $cleanupRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
    if (-not $cleanupPath.StartsWith($cleanupRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
        [System.IO.Path]::GetFileName($cleanupPath) -notmatch '^heavydrop_release_test_[a-f0-9]{32}$') {
        throw 'Refusing cleanup outside the owned temporary test directory'
    }
    Remove-Item -LiteralPath $cleanupPath -Recurse -Force -ErrorAction SilentlyContinue
    foreach ($key in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($key, $savedEnvironment[$key], 'Process')
    }
}

Write-Host "==> All release workflow tests passed successfully!" -ForegroundColor Green
