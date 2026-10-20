& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $Releases = 'https://github.com/bahaa-io/bahaa-wallet/releases/latest/download'
    $Target = if ($env:BAHAA_INSTALL_DIR) { $env:BAHAA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\Bahaa Wallet' }
    $Asset = 'bahaa-wallet-tui-windows-x86_64.exe'
    $Key = "-----BEGIN PUBLIC KEY-----`nMCowBQYDK2VwAyEAyFaHNPy9LcROq4u7HSd1EBGntv5ao7eawZVJRn5X+yg=`n-----END PUBLIC KEY-----`n"
    $Work = Join-Path ([IO.Path]::GetTempPath()) ('bahaa-wallet-' + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $Work | Out-Null
    try {
        Write-Host "Downloading $Asset"
        foreach ($Name in @($Asset, 'SHA256SUMS', 'SHA256SUMS.sig')) {
            try {
                Invoke-WebRequest -UseBasicParsing -Uri "$Releases/$Name" -OutFile (Join-Path $Work $Name)
            } catch {
                throw "$Name couldn't be downloaded."
            }
        }
        $Line = Get-Content (Join-Path $Work 'SHA256SUMS') | Where-Object {
            $Parts = $_ -split '\s+', 2
            $Parts.Count -eq 2 -and $Parts[1].TrimStart('*') -eq $Asset
        } | Select-Object -First 1
        if (-not $Line) {
            throw "the release has no $Asset."
        }
        $Expected = ($Line -split '\s+')[0]
        $Actual = (Get-FileHash -Algorithm SHA256 (Join-Path $Work $Asset)).Hash
        if ($Actual -ne $Expected) {
            throw "the download doesn't match its checksum."
        }
        $Checked = 'checksum verified; checking the signature needs OpenSSL 3'
        $OpenSsl = Get-Command openssl -ErrorAction SilentlyContinue
        if ($OpenSsl) {
            $KeyFile = Join-Path $Work 'key.pem'
            [IO.File]::WriteAllText($KeyFile, $Key)
            $ErrorActionPreference = 'Continue'
            $Result = & $OpenSsl.Source pkeyutl -verify -pubin -inkey $KeyFile -rawin -in (Join-Path $Work 'SHA256SUMS') -sigfile (Join-Path $Work 'SHA256SUMS.sig') 2>&1 | Out-String
            $ErrorActionPreference = 'Stop'
            if ($Result -match 'Verified Successfully') {
                $Checked = 'checksum and signature verified'
            } elseif ($Result -match 'Verification Failure') {
                throw 'the release signature is not valid.'
            }
        }
        New-Item -ItemType Directory -Force -Path $Target | Out-Null
        $Program = Join-Path $Target 'bahaa-wallet.exe'
        Copy-Item -Force (Join-Path $Work $Asset) $Program
        Write-Host "Installed Bahaa Wallet to $Program ($Checked)."
        $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        if (-not $UserPath) {
            $UserPath = ''
        }
        if (($UserPath -split ';') -notcontains $Target) {
            [Environment]::SetEnvironmentVariable('Path', (($UserPath.TrimEnd(';') + ';' + $Target).TrimStart(';')), 'User')
            $env:Path = $env:Path + ';' + $Target
            Write-Host "Added $Target to your PATH. Open a new terminal, then run: bahaa-wallet"
        } else {
            Write-Host 'Run it with: bahaa-wallet'
        }
    } catch {
        Write-Host "Bahaa Wallet couldn't be installed: $($_.Exception.Message)" -ForegroundColor Red
    } finally {
        Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
    }
}
