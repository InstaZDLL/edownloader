<#
.SYNOPSIS
  Creates a self-signed Authenticode certificate for the release workflow and
  prints what goes into the two GitHub secrets.

.DESCRIPTION
  Only needed when edownloader does not reuse the certificate WaveFlow,
  QuickPlay and Resona already sign with (docs/RELEASING.md). The PFX and its
  base64 form are written to secrets/ (ignored by git); the certificate is
  removed from the user's store afterwards, so the PFX is its only copy.

  A self-signed certificate proves the files were not changed after signing,
  not who made them: SmartScreen keeps warning until a certificate from a CA in
  Microsoft's Trusted Root Program replaces it, in the same two secrets.

.EXAMPLE
  pwsh scripts/release/new-signing-cert.ps1 -Subject "CN=InstaZDLL" -Years 5
#>
param(
  [string] $Subject = "CN=edownloader",
  [int] $Years = 5,
  [string] $OutDir = (Join-Path $PSScriptRoot "..\..\secrets")
)
$ErrorActionPreference = "Stop"

New-Item -ItemType Directory -Force $OutDir | Out-Null
$pfx = Join-Path $OutDir "edownloader-signing.pfx"
$b64 = Join-Path $OutDir "edownloader-signing.pfx.base64.txt"
if (Test-Path $pfx) { throw "$pfx already exists: move it away first, a release key is never overwritten" }

$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $Subject `
  -CertStoreLocation Cert:\CurrentUser\My -KeyAlgorithm RSA -KeyLength 3072 `
  -HashAlgorithm SHA256 -KeyExportPolicy Exportable -NotAfter (Get-Date).AddYears($Years)
try {
  # 32 random bytes, base64: the password of the PFX, shown once.
  $bytes = New-Object byte[] 32
  [Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
  $plain = [Convert]::ToBase64String($bytes)
  $password = ConvertTo-SecureString $plain -AsPlainText -Force
  Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $password | Out-Null
  [IO.File]::WriteAllText($b64, [Convert]::ToBase64String([IO.File]::ReadAllBytes($pfx)))
} finally {
  Remove-Item "Cert:\CurrentUser\My\$($cert.Thumbprint)"
}

Write-Host "Certificate $Subject ($($cert.Thumbprint)), valid until $($cert.NotAfter)"
Write-Host "PFX:     $pfx"
Write-Host ""
Write-Host "GitHub secrets (Settings > Secrets and variables > Actions):"
Write-Host "  SIGNTOOL_PFX_BASE64   = contents of $b64"
Write-Host "  SIGNTOOL_PFX_PASSWORD = $plain"
Write-Host ""
Write-Host "Or with the GitHub CLI:"
Write-Host "  gh secret set SIGNTOOL_PFX_BASE64 < `"$b64`""
Write-Host "  gh secret set SIGNTOOL_PFX_PASSWORD --body '<the password above>'"
