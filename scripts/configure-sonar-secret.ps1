$ErrorActionPreference = "Stop"

Write-Host "Local SonarQube token setup for RidgeCode"
Write-Host "Open http://localhost:9000/account/security, create a user token, then paste it below. Input is hidden."

$secure = Read-Host "SONAR_TOKEN" -AsSecureString
$ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
try {
    $plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
    if ([string]::IsNullOrWhiteSpace($plain)) {
        throw "Empty token"
    }

    [Environment]::SetEnvironmentVariable('SONAR_TOKEN', $plain, 'User')
    [Environment]::SetEnvironmentVariable('SONAR_HOST_URL', 'http://localhost:9000', 'User')
    Write-Host "Local SONAR_TOKEN configured successfully." -ForegroundColor Green
}
finally {
    if ($ptr -ne [IntPtr]::Zero) {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr)
    }
    $plain = $null
}

Read-Host "Press Enter to close"
