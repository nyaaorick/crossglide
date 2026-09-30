# Builds the Crossglide virtual touchpad driver from source, signs it with a local test
# certificate, and installs or updates it. Run from an elevated PowerShell on the PC:
#
#   powershell -ExecutionPolicy Bypass -File drivers\touchpad\build.ps1            # build + install
#   powershell -ExecutionPolicy Bypass -File drivers\touchpad\build.ps1 -NoInstall
#   powershell -ExecutionPolicy Bypass -File drivers\touchpad\build.ps1 -Uninstall
#
# Needs the Visual Studio C++ Build Tools (the Rust toolchain already does). The WDK comes from
# its NuGet package, unpacked into ~\wdk the first time; nothing is installed system-wide except
# the test certificate and the driver itself.

param(
    [switch]$NoInstall,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$WdkVersion = '10.0.26100.6584'
$SdkVersion = '10.0.26100.0'
$Umdf = '2.15'
$HardwareId = 'Root\CrossglideTouchpad'
$CertSubject = 'CN=Crossglide Test Signing'

$here = $PSScriptRoot
$repo = Resolve-Path "$here\..\.."
$out = Join-Path $repo 'target\touchpad'
$wdk = Join-Path $HOME 'wdk'
$devcon = "$wdk\c\tools\$SdkVersion\x64\devcon.exe"

function Invoke-Checked([string]$what, [scriptblock]$command) {
    & $command
    if ($LASTEXITCODE -ne 0) { throw "$what failed (exit code $LASTEXITCODE)" }
}

function Get-Wdk {
    if (Test-Path "$wdk\c\Include\wdf\umdf\$Umdf\wdf.h") { return }
    $zip = "$HOME\wdk-$WdkVersion.zip"
    if (-not (Test-Path $zip)) {
        Write-Host "Downloading the WDK $WdkVersion (110 MB)..."
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest "https://api.nuget.org/v3-flatcontainer/microsoft.windows.wdk.x64/$WdkVersion/microsoft.windows.wdk.x64.$WdkVersion.nupkg" -OutFile $zip
    }
    Expand-Archive $zip -DestinationPath $wdk -Force
}

# Loads the MSVC x64 environment (cl, link, signtool) into this PowerShell session.
function Enter-VsEnvironment {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vs) { throw 'The Visual Studio C++ Build Tools are not installed' }
    $vars = & cmd /c "`"$vs\VC\Auxiliary\Build\vcvars64.bat`" >nul && set"
    foreach ($line in $vars) {
        if ($line -match '^([^=]+)=(.*)$') { Set-Item "env:$($Matches[1])" $Matches[2] }
    }
}

# The test certificate, created once and trusted on this machine only.
function Get-TestCertificate {
    $cert = Get-ChildItem Cert:\LocalMachine\My | Where-Object Subject -eq $CertSubject | Select-Object -First 1
    if (-not $cert) {
        Write-Host "Creating the test certificate '$CertSubject'"
        $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $CertSubject `
            -CertStoreLocation Cert:\LocalMachine\My -NotAfter (Get-Date).AddYears(10)
    }
    foreach ($storeName in 'Root', 'TrustedPublisher') {
        $store = New-Object Security.Cryptography.X509Certificates.X509Store($storeName, 'LocalMachine')
        $store.Open('ReadWrite')
        if (-not ($store.Certificates | Where-Object Thumbprint -eq $cert.Thumbprint)) {
            $store.Add($cert)
        }
        $store.Close()
    }
    $cert
}

function Test-Installed {
    $found = & $devcon hwids $HardwareId 2>$null
    return ($found -match [regex]::Escape($HardwareId))
}

if ($Uninstall) {
    if (Test-Installed) { Invoke-Checked 'devcon remove' { & $devcon remove $HardwareId } }
    Get-WindowsDriver -Online | Where-Object OriginalFileName -like '*crossglide-touchpad.inf' | ForEach-Object {
        Invoke-Checked 'pnputil /delete-driver' { pnputil /delete-driver $_.Driver /uninstall /force }
    }
    Write-Host 'Uninstalled the Crossglide touchpad.'
    return
}

Get-Wdk
Enter-VsEnvironment

New-Item -ItemType Directory -Force "$out\package", "$out\include" | Out-Null
# hidport.h lives among the kernel headers; copy it alone so they don't shadow user-mode ones.
Copy-Item "$wdk\c\Include\$SdkVersion\km\hidport.h" "$out\include\"

$major, $minor = $Umdf.Split('.')
Invoke-Checked 'cl' {
    # C4324: padding in the WDK's own headers.
    cl /nologo /c /W4 /WX /wd4324 /O2 /MT /Zi /DUNICODE /D_UNICODE `
        "/DUMDF_VERSION_MAJOR=$major" "/DUMDF_VERSION_MINOR=$minor" `
        "/I$wdk\c\Include\wdf\umdf\$Umdf" "/I$out\include" `
        "/Fo$out\touchpad.obj" "/Fd$out\touchpad.pdb" "$here\touchpad.c"
}
Invoke-Checked 'link' {
    link /nologo /DLL /DEBUG "/PDB:$out\crossglide-touchpad.pdb" "/OUT:$out\package\crossglide-touchpad.dll" `
        "/IMPLIB:$out\crossglide-touchpad.lib" "$out\touchpad.obj" `
        "$wdk\c\Lib\wdf\umdf\x64\$Umdf\WdfDriverStubUm.lib" kernel32.lib ntdll.lib
}
Copy-Item "$here\crossglide-touchpad.inf" "$out\package\"

$cert = Get-TestCertificate
Invoke-Checked 'signtool (dll)' { signtool sign /q /fd sha256 /sm /s My /sha1 $cert.Thumbprint "$out\package\crossglide-touchpad.dll" }
Invoke-Checked 'inf2cat' { & "$wdk\c\bin\$SdkVersion\x86\Inf2Cat.exe" "/driver:$out\package" /os:10_X64 }
Invoke-Checked 'signtool (cat)' { signtool sign /q /fd sha256 /sm /s My /sha1 $cert.Thumbprint "$out\package\crossglide-touchpad.cat" }
Write-Host "Built and signed $out\package"

if ($NoInstall) { return }
$inf = "$out\package\crossglide-touchpad.inf"
if (Test-Installed) {
    Invoke-Checked 'devcon update' { & $devcon update $inf $HardwareId }
} else {
    Invoke-Checked 'devcon install' { & $devcon install $inf $HardwareId }
}
Get-PnpDevice -PresentOnly | Where-Object { $_.FriendlyName -like '*Crossglide*' -or $_.InstanceId -like 'HID\VID_1209&PID_C6D0*' } |
    Format-Table Status, Class, FriendlyName, InstanceId -AutoSize
