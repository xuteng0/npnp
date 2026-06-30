param(
    [Parameter(Mandatory = $true)]
    [string]$SchLibPath,

    [Parameter(Mandatory = $true)]
    [string]$PcbLibPath,

    [Parameter(Mandatory = $true)]
    [string]$CsvPath
)

$ErrorActionPreference = "Stop"

function Resolve-AbsolutePath {
    param([Parameter(Mandatory = $true)][string]$Path)
    return [System.IO.Path]::GetFullPath((Resolve-Path -LiteralPath $Path).Path)
}

function Get-LcscIdsFromCsv {
    param([Parameter(Mandatory = $true)][string]$Path)

    $rows = Import-Csv -LiteralPath $Path
    if (-not $rows) {
        throw "CSV is empty: $Path"
    }

    $firstRow = $rows[0].PSObject.Properties.Name
    $preferredColumns = @(
        "LCSC_ID",
        "LCSC",
        "LCSC ID",
        "Lcsc",
        "lcsc",
        "part_id",
        "part id",
        "id"
    )

    $column = $null
    foreach ($candidate in $preferredColumns) {
        if ($firstRow -contains $candidate) {
            $column = $candidate
            break
        }
    }

    if (-not $column) {
        throw "CSV must contain an LCSC ID column. Expected one of: $($preferredColumns -join ', ')"
    }

    return $rows | ForEach-Object {
        $value = $_.$column
        if ($null -eq $value) { return }
        $text = $value.ToString().Trim()
        if ($text) { $text }
    } | Select-Object -Unique
}

function Invoke-Npnp {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Args
    )

    $npnpCommand = Get-Command -Name "npnp" -ErrorAction SilentlyContinue
    if ($npnpCommand) {
        & $npnpCommand.Source @Args
        return $LASTEXITCODE
    }

    $repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
    Push-Location $repoRoot
    try {
        & cargo run --quiet -- @Args
        return $LASTEXITCODE
    }
    finally {
        Pop-Location
    }
}

$schLib = Resolve-AbsolutePath $SchLibPath
$pcbLib = Resolve-AbsolutePath $PcbLibPath
$csvFile = Resolve-AbsolutePath $CsvPath

if (-not (Test-Path -LiteralPath $schLib)) {
    throw "SchLib not found: $schLib"
}
if (-not (Test-Path -LiteralPath $pcbLib)) {
    throw "PcbLib not found: $pcbLib"
}
if (-not (Test-Path -LiteralPath $csvFile)) {
    throw "CSV not found: $csvFile"
}

$schDir = Split-Path -Parent $schLib
$pcbDir = Split-Path -Parent $pcbLib
if ($schDir -ne $pcbDir) {
    throw "SchLib and PcbLib must be in the same directory."
}

$schStem = [System.IO.Path]::GetFileNameWithoutExtension($schLib)
$pcbStem = [System.IO.Path]::GetFileNameWithoutExtension($pcbLib)
if ($schStem -ne $pcbStem) {
    throw "SchLib and PcbLib must share the same base name."
}

$ids = Get-LcscIdsFromCsv -Path $csvFile
if (-not $ids -or $ids.Count -eq 0) {
    throw "No LCSC IDs found in CSV: $csvFile"
}

$tempInput = Join-Path $env:TEMP ("npnp-merge-" + [guid]::NewGuid().ToString("N") + ".txt")
try {
    $ids | Set-Content -LiteralPath $tempInput -Encoding UTF8

    $exitCode = Invoke-Npnp -Args @(
        "batch",
        "--input", $tempInput,
        "--output", $schDir,
        "--merge",
        "--append",
        "--library-name", $schStem,
        "--full",
        "--continue-on-error",
        "--force",
        "--lcsc-english"
    )

    if ($exitCode -ne 0) {
        exit $exitCode
    }
}
finally {
    if (Test-Path -LiteralPath $tempInput) {
        Remove-Item -LiteralPath $tempInput -Force
    }
}
