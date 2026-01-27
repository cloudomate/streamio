# PowerShell script to build standalone binary on Windows
# No GStreamer required - single binary output

Write-Host "Building Horizon Streamer (Standalone)" -ForegroundColor Cyan
Write-Host "======================================" -ForegroundColor Cyan

# Check Rust
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "Rust not found. Installing..." -ForegroundColor Yellow
    Invoke-WebRequest -Uri "https://win.rustup.rs" -OutFile "$env:TEMP\rustup-init.exe"
    Start-Process -Wait -FilePath "$env:TEMP\rustup-init.exe" -ArgumentList "-y"
    $env:PATH += ";$env:USERPROFILE\.cargo\bin"
}

Write-Host "`nRust version:" -ForegroundColor Green
cargo --version

# Switch to standalone config
Write-Host "`nSwitching to standalone build configuration..." -ForegroundColor Green
Copy-Item -Force "Cargo.toml" "Cargo.toml.gstreamer"
Copy-Item -Force "Cargo.toml.standalone" "Cargo.toml"

# Rename main files
if (Test-Path "src\main.rs") {
    Move-Item -Force "src\main.rs" "src\main_gstreamer.rs"
}
Copy-Item -Force "src\main_standalone.rs" "src\main.rs"

Write-Host "`nBuilding release binary..." -ForegroundColor Green
cargo build --release

if ($LASTEXITCODE -eq 0) {
    $size = (Get-Item "target\release\horizon-streamer.exe").Length / 1MB
    Write-Host "`nBuild successful!" -ForegroundColor Green
    Write-Host "Binary: target\release\horizon-streamer.exe" -ForegroundColor Cyan
    Write-Host "Size: $([math]::Round($size, 2)) MB" -ForegroundColor Cyan

    Write-Host "`nTo run:" -ForegroundColor Yellow
    Write-Host "  .\target\release\horizon-streamer.exe" -ForegroundColor White
    Write-Host "  Then open http://localhost:8123" -ForegroundColor White
} else {
    Write-Host "`nBuild failed!" -ForegroundColor Red
}

# Restore original config
Write-Host "`nRestoring original configuration..." -ForegroundColor Gray
Copy-Item -Force "Cargo.toml.gstreamer" "Cargo.toml"
if (Test-Path "src\main_gstreamer.rs") {
    Move-Item -Force "src\main_gstreamer.rs" "src\main.rs"
}
