param([string]$RepositoryRoot = (Join-Path $PSScriptRoot '../..'))
$ErrorActionPreference = 'Stop'
Push-Location -LiteralPath $RepositoryRoot
try {
    $gitCmd = (Get-Command -CommandType Application git).Source
    function Invoke-Git([string[]]$Arguments) {
        $result = & $gitCmd @Arguments
        if ($LASTEXITCODE -ne 0) { throw "Git failed: $($Arguments[0])" }
        return $result
    }
    $sourceSha = (Invoke-Git @('rev-parse', 'HEAD')).Trim()
    $automatic = $env:GITHUB_EVENT_NAME -eq 'push' -and $env:GITHUB_REF -eq 'refs/heads/main'
    $tags = @(Invoke-Git @('tag', '--list', 'v*')) | Where-Object { $_ -match '^v\d+\.\d+\.\d+$' }
    if ($automatic) {
        foreach ($existing in $tags) {
            $message = (Invoke-Git @('log', '-1', '--format=%B', $existing)) -join "`n"
            $tagSha = (Invoke-Git @('rev-parse', "$existing^{commit}")).Trim()
            if ($message -match "(?m)^Release-Source: $sourceSha$" -or $tagSha -eq $sourceSha) {
                if ($env:GITHUB_OUTPUT) { "release_exists=true" | Add-Content -LiteralPath $env:GITHUB_OUTPUT }
                if ($env:GITHUB_ENV) { "RELEASE_TAG=$existing" | Add-Content -LiteralPath $env:GITHUB_ENV }
                Write-Output "Release already prepared for $sourceSha : $existing"
                return
            }
        }
    }
    $manifestPath = Join-Path $PWD 'Cargo.toml'
    $lockPath = Join-Path $PWD 'Cargo.lock'
    $manifest = [System.IO.File]::ReadAllText($manifestPath)
    $packagePattern = '(?ms)(^\[package\]\r?\n.*?^version\s*=\s*")([^"\r\n]+)(")'
    $packageMatch = [regex]::Match($manifest, $packagePattern)
    if (-not $packageMatch.Success) { throw 'Package version missing from Cargo.toml' }
    if ($automatic) {
        $versions = @([version]$packageMatch.Groups[2].Value) + @($tags | ForEach-Object { [version]$_.Substring(1) })
        $latest = $versions | Sort-Object -Descending | Select-Object -First 1
        $tag = "v$($latest.Major).$($latest.Minor).$($latest.Build + 1)"
    } elseif ($env:GITHUB_REF_TYPE -eq 'tag') {
        $tag = $env:GITHUB_REF_NAME
    } else {
        $tag = $env:INPUT_TAG
    }
    if ($tag -notmatch '^v\d+\.\d+\.\d+$') { throw 'Expected a release tag in vMAJOR.MINOR.PATCH format' }
    if ($tags -contains $tag) {
        $tagSha = (Invoke-Git @('rev-parse', "$tag^{commit}")).Trim()
        if ($tagSha -ne $sourceSha) { throw 'Existing tag points to a different commit; refusing to replace it' }
    } else {
        $version = $tag.Substring(1)
        $manifest = [regex]::Replace($manifest, $packagePattern, { param($m) $m.Groups[1].Value + $version + $m.Groups[3].Value })
        $lock = [System.IO.File]::ReadAllText($lockPath)
        $lockPattern = '(?ms)(^\[\[package\]\]\r?\nname = "heavydrop"\r?\nversion = ")([^"\r\n]+)(")'
        if ([regex]::Matches($lock, $lockPattern).Count -ne 1) { throw 'Expected one heavydrop package in Cargo.lock' }
        $lock = [regex]::Replace($lock, $lockPattern, { param($m) $m.Groups[1].Value + $version + $m.Groups[3].Value })
        [System.IO.File]::WriteAllText($manifestPath, $manifest)
        [System.IO.File]::WriteAllText($lockPath, $lock)
        Invoke-Git @('config', 'user.name', 'github-actions[bot]') | Out-Null
        Invoke-Git @('config', 'user.email', '41898282+github-actions[bot]@users.noreply.github.com') | Out-Null
        Invoke-Git @('add', '--', 'Cargo.toml', 'Cargo.lock') | Out-Null
        & $gitCmd diff --cached --quiet
        if ($LASTEXITCODE -eq 1) {
            Invoke-Git @('commit', '-m', "Release $tag", '-m', "Release-Source: $sourceSha") | Out-Null
        } elseif ($LASTEXITCODE -ne 0) { throw 'Cannot inspect staged version changes' }
        Invoke-Git @('tag', '-a', $tag, '-m', "HeavyDrop $tag") | Out-Null
    }
    # Only the release tag is pushed after tests/build succeed. main is never modified by the bot.
    if ($env:GITHUB_ENV) { "RELEASE_TAG=$tag" | Add-Content -LiteralPath $env:GITHUB_ENV }
    if ($env:GITHUB_OUTPUT) { "release_exists=false" | Add-Content -LiteralPath $env:GITHUB_OUTPUT }
    Write-Output "Prepared $tag from $sourceSha"
} finally { Pop-Location }
