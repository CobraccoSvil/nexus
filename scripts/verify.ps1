# Porting Windows di scripts/verify.sh (gate qualita').
# Esegue: turbo typecheck/lint/test (TS) + cargo check/clippy/test (Rust MSVC).
#   .\verify.ps1                gate completo
#   .\verify.ps1 -SkipTs        salta TypeScript
#   .\verify.ps1 -SkipRust      salta Rust
param([switch]$SkipRust, [switch]$SkipTs)
$ErrorActionPreference = 'Continue'
$ROOT = 'D:\IDEAI'
$fail = 0

function Initialize-Msvc {
  $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
  $vsPath  = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
  $vcvars  = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
  cmd /c "`"$vcvars`" && set" | ForEach-Object { if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($matches[1])" -Value $matches[2] } }
}

if (-not $SkipTs) {
  Write-Host '== TS: turbo typecheck lint test ==' -ForegroundColor Cyan
  Set-Location $ROOT
  pnpm exec turbo run typecheck lint test --continue
  if ($LASTEXITCODE -ne 0) { $fail++; Write-Host 'TS gate FALLITO' -ForegroundColor Red }
}
if (-not $SkipRust) {
  Write-Host '== Rust: check / clippy / test (MSVC) ==' -ForegroundColor Cyan
  Initialize-Msvc
  Set-Location $ROOT
  cargo check --workspace;                               if ($LASTEXITCODE -ne 0) { $fail++ }
  cargo clippy --workspace --all-targets -- -D warnings; if ($LASTEXITCODE -ne 0) { $fail++ }
  cargo test  --workspace --no-fail-fast;                if ($LASTEXITCODE -ne 0) { $fail++ }
}
if ($fail -gt 0) { Write-Host "VERIFY FALLITO ($fail step)" -ForegroundColor Red; exit 1 }
Write-Host 'VERIFY OK' -ForegroundColor Green
