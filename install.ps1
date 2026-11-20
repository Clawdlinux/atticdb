# Installs atticdb.exe from a GitHub release, on Windows x86_64 or arm64.
#
#   irm https://github.com/Clawdlinux/atticdb/releases/latest/download/install.ps1 | iex
#
# Settings, all optional:
#   ATTICDB_VERSION        a release such as 0.4.0 or v0.4.0. Default: latest.
#   ATTICDB_INSTALL_DIR    where atticdb.exe goes. Default: %LOCALAPPDATA%\Programs\atticdb.
#   ATTICDB_DOWNLOAD_BASE  the releases URL. Default: https://github.com/Clawdlinux/atticdb/releases.
#
# The zip is checked against the release's SHA256SUMS before anything is installed.
# It needs no admin rights. It adds the install folder to your user PATH if it isn't there.
# Runs on Windows PowerShell 5.1 and PowerShell 7.

function Install-AtticDB {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    # Windows PowerShell 5.1 may default to TLS 1.0, which GitHub refuses.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    function Say($text) { Write-Host "atticdb install: $text" }

    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
    if ($arch -ne 'AMD64' -and $arch -ne 'ARM64') {
        throw "atticdb install: no prebuilt binary for $arch. Build from source: cargo install atticdb --locked"
    }
    $file = 'atticdb-x86_64-pc-windows-msvc.zip'

    $base = $env:ATTICDB_DOWNLOAD_BASE
    if (-not $base) { $base = 'https://github.com/Clawdlinux/atticdb/releases' }
    $base = $base.TrimEnd('/')
    $version = $env:ATTICDB_VERSION
    if (-not $version -or $version -eq 'latest') {
        $url = "$base/latest/download"
    } else {
        $version = $version -replace '^v', ''
        if ($version -notmatch '^[0-9A-Za-z.+-]+$') {
            throw "atticdb install: ATTICDB_VERSION is not a version: $env:ATTICDB_VERSION"
        }
        $url = "$base/download/v$version"
    }

    $dir = $env:ATTICDB_INSTALL_DIR
    if (-not $dir) { $dir = Join-Path $env:LOCALAPPDATA 'Programs\atticdb' }
    if (-not [IO.Path]::IsPathRooted($dir)) {
        throw "atticdb install: ATTICDB_INSTALL_DIR must be an absolute path: $dir"
    }
    $dir = $dir.TrimEnd('\')

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('atticdb-install-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    $part = $null
    try {
        Say "downloading $url/$file"
        Invoke-WebRequest -UseBasicParsing -Uri "$url/$file" -OutFile (Join-Path $tmp $file)
        Invoke-WebRequest -UseBasicParsing -Uri "$url/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS')

        $want = $null
        foreach ($line in Get-Content (Join-Path $tmp 'SHA256SUMS')) {
            $fields = $line -split '\s+'
            if ($fields.Count -ge 2 -and ($fields[1] -eq $file -or $fields[1] -eq "*$file")) {
                $want = $fields[0].ToLower()
                break
            }
        }
        if (-not $want) { throw "atticdb install: SHA256SUMS has no entry for $file. Nothing was installed." }
        $got = (Get-FileHash (Join-Path $tmp $file) -Algorithm SHA256).Hash.ToLower()
        if ($got -ne $want) {
            throw "atticdb install: checksum mismatch for ${file}: expected $want, got $got. Nothing was installed."
        }

        $x = Join-Path $tmp 'x'
        Expand-Archive -Path (Join-Path $tmp $file) -DestinationPath $x
        $exe = Join-Path $x 'atticdb.exe'
        if (-not (Test-Path -PathType Leaf $exe)) { throw "atticdb install: $file has no atticdb.exe" }

        # Copy beside the target, check it runs, then move it over the old one.
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        $dest = Join-Path $dir 'atticdb.exe'
        if (Test-Path -PathType Container $dest) {
            throw "atticdb install: $dest is a directory. Move it away and run the installer again. Nothing was installed."
        }
        $part = Join-Path $dir ('.atticdb.part.' + [Guid]::NewGuid().ToString('N') + '.exe')
        Copy-Item $exe $part
        & $part --version | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "atticdb install: the downloaded atticdb.exe did not run. Nothing was installed." }
        try {
            Move-Item -Force $part $dest
        } catch {
            throw "atticdb install: cannot replace $dest. It may be running: stop it with 'atticdb stop', then run the installer again. Nothing was installed."
        }
        $part = $null

        $installed = & $dest --version
        Say "installed $installed at $dest"

        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @()
        if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ } }
        if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $dir })) {
            [Environment]::SetEnvironmentVariable('Path', (($entries + $dir) -join ';'), 'User')
            Say "added $dir to your user PATH. New terminals pick it up."
        }
        if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $dir })) {
            $env:Path = "$env:Path;$dir"
        }
        $found = (Get-Command atticdb -ErrorAction SilentlyContinue | Select-Object -First 1).Source
        if ($found -and $found -ne $dest) {
            Say "warning: $found comes first on PATH, so atticdb runs that one."
        }

        Write-Host ''
        Write-Host 'Next steps:'
        Write-Host '  atticdb setup --dry-run   # shows what it would change, writes nothing'
        Write-Host '  atticdb setup             # asks which apps, then wires them'
    } finally {
        if ($part -and (Test-Path $part)) { Remove-Item -Force $part }
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

Install-AtticDB
