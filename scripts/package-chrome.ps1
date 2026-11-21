param([string]$Output = (Join-Path $PSScriptRoot '../release/chrome'))

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$source = Join-Path $root 'firefox'
$destination = (New-Item -ItemType Directory -Force -Path $Output).FullName
foreach ($name in @('_locales','background.js','content.js','icon.svg','popup.css','popup.html','popup.js')) {
    Copy-Item -LiteralPath (Join-Path $source $name) -Destination $destination -Recurse -Force
}

$publicKey = 'MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEArlGRBecQsOH7vyn5hs8t0l8omejjIJSGFjflqu8Vc09W7bSVDiRFQlN0g667Bq2yEiNNi6syqoYNfP43kd4MmdYTPRAwHhX62wXbrLLX0Xh3F6JirLt7AN2EPmG9L7mS4NvZTiPmka8hA14t+ctOb1X8T1FndSawiF4BLqxKJmSVTL9Yil3KJDabVenHRvvOcsd1ReHqIPvJ8mAAwg0YIDPxzjAfE4mkRvaRmDs7IhmeyUWNDiG7qDEqmRWeZ04ztvqLo7JhAbuD1BtwnkRDpWSCXxXRAhl7OrbdF07tAE9PGmEwVH1HRXJaruZUIcXZHX3Wzd7jMzw1xX0Il9BhZQIDAQAB'
$hex = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Convert]::FromBase64String($publicKey)))
$id = -join ($hex.Substring(0, 32).ToCharArray() | ForEach-Object { [char](97 + [Convert]::ToInt32([string]$_,16)) })
$bridge = Get-Content -Raw -LiteralPath (Join-Path $root 'src/firefox.rs')
if (!$bridge.Contains("chrome-extension://$id/")) { throw 'Chrome extension ID differs from the native host' }

$manifest = Get-Content -Raw -LiteralPath (Join-Path $source 'manifest.json') | ConvertFrom-Json -AsHashtable
$manifest.Remove('browser_specific_settings')
$manifest.key = $publicKey
$manifest.background = @{ service_worker = 'background.js' }
$manifest.icons = [ordered]@{}
$manifest.action.default_icon = [ordered]@{}
Add-Type -AssemblyName System.Drawing
$original = [System.Drawing.Icon]::new((Join-Path $root 'icons/icon.ico'), 128, 128)
try {
    $image = $original.ToBitmap()
    try {
        foreach ($size in @(16,32,48,128)) {
            $name = "icon-$size.png"
            $bitmap = [System.Drawing.Bitmap]::new($image, $size, $size)
            try { $bitmap.Save((Join-Path $destination $name), [System.Drawing.Imaging.ImageFormat]::Png) }
            finally { $bitmap.Dispose() }
            $manifest.icons[[string]$size] = $name
            if ($size -le 48) { $manifest.action.default_icon[[string]$size] = $name }
        }
    } finally { $image.Dispose() }
} finally { $original.Dispose() }
$manifest | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $destination 'manifest.json') -Encoding utf8

foreach ($locale in @('en','uk','ru')) {
    $file = Join-Path $destination "_locales/$locale/messages.json"
    $content = [IO.File]::ReadAllText($file).Replace('Firefox / Zen', 'Chrome').Replace('Firefox', 'Chrome')
    [IO.File]::WriteAllText($file, $content)
}
Write-Output "Chrome extension $id built in $destination"
