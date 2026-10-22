# Install gitta on Windows: picks the build for this CPU, verifies its SHA-256 checksum, installs to
# %LOCALAPPDATA%\gitta\bin (or $env:GITTA_BIN_DIR) and adds that folder to the user PATH.
# The release workflow fills in RELEASES and VERSION. Run it with:  irm https://gitta.io/install.ps1 | iex
# Downloads go through https://gitta.io/dl, which counts them (via=install, no personal data), and fall back to RELEASES.
# GITTA_RELEASES skips gitta.io; GITTA_DL sets another download base.
# The script changes nothing outside the bin dir, except the user PATH.
& {
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch {}

$DL = ''; if (-not $env:GITTA_RELEASES) { $DL = if ($env:GITTA_DL) { $env:GITTA_DL } else { 'https://gitta.io/dl' } }
$Releases = if ($env:GITTA_RELEASES) { $env:GITTA_RELEASES } else { '@RELEASES@' }
$Version = if ($env:GITTA_VERSION) { $env:GITTA_VERSION } else { '@VERSION@' }
$BinDir = if ($env:GITTA_BIN_DIR) { $env:GITTA_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'gitta\bin' }
$Docs = 'https://gitta.io/docs'

$color = (-not $env:NO_COLOR) -and -not [Console]::IsOutputRedirected
function Say([string]$t = '', [string]$c = '') { if ($color -and $c) { Write-Host $t -ForegroundColor $c } else { Write-Host $t } }
function Warn([string]$t) { if ($color) { Write-Host 'Note: ' -ForegroundColor Yellow -NoNewline; Write-Host $t } else { Write-Host "Note: $t" } }
function Die([string]$t) { throw "gitta: $t" }
# Show paths under the profile folder as ~\..., so the lines stay short.
function Tilde([string]$p) { if ($env:USERPROFILE -and $p.StartsWith("$env:USERPROFILE\", 'OrdinalIgnoreCase')) { '~' + $p.Substring($env:USERPROFILE.Length) } else { $p } }

try {
  if ("$Version$Releases" -like '*@*') { Die 'this is the unreleased install script; set GITTA_RELEASES and GITTA_VERSION, or use the install.ps1 of a release' }
  $arch = $null
  try { $arch = [string][System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture } catch {}
  if (-not $arch) { $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE } }
  switch -regex ($arch) {
    '^(X64|AMD64)$' { $target = 'x86_64-pc-windows-msvc'; $what = 'Windows x86_64' }
    '^ARM64$' { $target = 'aarch64-pc-windows-msvc'; $what = 'Windows arm64' }
    default { Die "there is no build for Windows $arch yet. You can build it from source: $Docs/install.html" }
  }
  $bin = Tilde $BinDir
  $exe = Join-Path $BinDir 'gitta.exe'
  $old = ''
  if (Test-Path $exe) { try { $old = ((& $exe --version 2>$null) | Select-Object -First 1).Split(' ')[-1] } catch {} }
  if (-not $old) { Say "Installing gitta $Version for $what into $bin" }
  elseif ($old -eq $Version) { Say "Reinstalling gitta $Version for $what into $bin" }
  else { Say "Upgrading gitta from $old to $Version for $what in $bin" }

  $name = "gitta-$Version-$target"
  $tmp = Join-Path ([IO.Path]::GetTempPath()) ("gitta-install-" + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Force $tmp | Out-Null
  try {
    function Get([string]$url, [string]$out) {
      if ($url -like 'file://*') { Copy-Item -LiteralPath ([Uri]$url).LocalPath $out } else { Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $out }
    }
    # Fetch FILE from gitta.io first, then straight from the release.
    function Fetch([string]$f) {
      $out = Join-Path $tmp $f
      if ($DL) { try { Get "$DL/v$Version/$f`?via=install" $out; return } catch {} }
      try { Get "$Releases/$f" $out } catch { Die "cannot download $Releases/$f. Check the network connection and try again." }
    }
    Fetch "$name.zip"
    Fetch "$name.zip.sha256"
    $want = ((Get-Content -Raw (Join-Path $tmp "$name.zip.sha256")).Trim() -split '\s+')[0].ToLower()
    $have = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp "$name.zip")).Hash.ToLower()
    if ($want -ne $have) { Die 'the checksum does not match, so nothing was installed. Try again, and report it if it happens twice.' }
    Expand-Archive -LiteralPath (Join-Path $tmp "$name.zip") -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Force $BinDir | Out-Null
    # Remove what an earlier update moved aside, when nothing runs it any more.
    Get-ChildItem -LiteralPath $BinDir -Force -Filter '.*.old-*' -ErrorAction SilentlyContinue | ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }
    # Copy next to the old file, move a running gitta.exe aside (Windows cannot overwrite it), then rename into place.
    $files = Get-ChildItem -LiteralPath (Join-Path $tmp $name) -File
    foreach ($f in $files) {
      $dest = Join-Path $BinDir $f.Name
      $new = Join-Path $BinDir ".$($f.Name).new-$PID"
      Copy-Item -LiteralPath $f.FullName $new -Force
      $old = Join-Path $BinDir ".$($f.Name).old-$PID"
      if (Test-Path -LiteralPath $dest) { Move-Item -LiteralPath $dest $old -Force }
      Move-Item -LiteralPath $new $dest -Force
      # Gone at once unless a gitta is still running from it; then the next install or update removes it.
      if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue }
    }
    Say "Done. Checksum verified, $(($files | ForEach-Object Name) -join ' ') installed." 'Green'
  } finally { Remove-Item -Recurse -Force -LiteralPath $tmp -ErrorAction SilentlyContinue }

  # PATH: add the folder to the user PATH once (it keeps any %VARIABLES% in it), and to this window.
  $key = Get-Item -LiteralPath 'HKCU:\Environment'
  $userPath = [string]$key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
  $parts = @($userPath -split ';' | Where-Object { $_ })
  if (-not ($parts | Where-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -ieq $BinDir.TrimEnd('\') })) {
    $kind = if ($userPath -and $key.GetValueKind('Path') -eq 'String') { 'String' } else { 'ExpandString' }
    Set-ItemProperty -LiteralPath 'HKCU:\Environment' -Name Path -Value (($parts + $BinDir) -join ';') -Type $kind
    # Tell open programs that the environment changed, so new terminals see the PATH.
    [Environment]::SetEnvironmentVariable('GITTA_INSTALLER', '1', 'User'); [Environment]::SetEnvironmentVariable('GITTA_INSTALLER', $null, 'User')
    Say ''; Say "Added $bin to your user PATH. New terminals find gitta; this one does too."
  }
  if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $BinDir.TrimEnd('\') })) { $env:Path = "$BinDir;$env:Path" }

  # Prerequisites and agent tools.
  Say ''
  $git = Get-Command git -ErrorAction SilentlyContinue
  if (-not $git) { Warn 'git is not installed, and gitta needs it. Install Git for Windows 2.28 or later (https://git-scm.com/download/win), then continue below.' }
  else {
    $gv = ((& git --version) -split ' ')[2]
    $p = $gv -split '\.'
    if ([int]$p[0] -lt 2 -or ([int]$p[0] -eq 2 -and [int]$p[1] -lt 28)) {
      Warn "git $gv is older than 2.28. gitta still checks every merge with a fallback hook, but git 2.28 or later stops a failed fast-forward before it lands. Update git when you can."
    } else { Say "Found git $gv." }
  }
  foreach ($a in @(@('claude', '.claude', 'Claude Code'), @('cursor', '.cursor', 'Cursor'), @('codex', '.codex', 'Codex'))) {
    if ((Get-Command $a[0] -ErrorAction SilentlyContinue) -or ($env:USERPROFILE -and (Test-Path (Join-Path $env:USERPROFILE $a[1])))) { Say "Found $($a[2]). ``gitta init`` will connect it." }
  }

  Say ''
  Say 'Next steps' 'White'
  Say '  1. Go to your repository:      cd path\to\your\repo'
  Say '  2. Set it up with your tests:  gitta init --gate "<your test command>"'
  Say '     for example --gate "pytest", --gate "npm test", --gate "cargo test" or --gate "go test ./..."'
  Say '  3. See what gitta does:        gitta watch'
  Say "  Quick start: $Docs/quickstart.html"
} catch {
  # A message, not a stack trace, and never `exit`: this runs inside the user's own PowerShell window.
  Write-Host $_.Exception.Message -ForegroundColor Red
  $global:LASTEXITCODE = 1
  if ($env:GITTA_INSTALL_STRICT) { throw }
}
}
