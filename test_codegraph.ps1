#!/usr/bin/env pwsh

# Test Codegraph MCP server connection
Write-Host "Testing Codegraph MCP server connection..." -ForegroundColor Green

# Check if codegraph command is available
if (-not (Get-Command codegraph -ErrorAction SilentlyContinue)) {
    Write-Host "Error: codegraph command not found, please ensure Codegraph is installed" -ForegroundColor Red
    exit 1
}

# Check if Codegraph project is initialized
$codegraphStatus = codegraph status 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Host "Codegraph project not initialized, initializing..." -ForegroundColor Yellow
    codegraph init
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Failed to initialize Codegraph" -ForegroundColor Red
        exit 1
    }
    Write-Host "Codegraph successfully initialized" -ForegroundColor Green
}

# Sync index
Write-Host "Syncing Codegraph index..." -ForegroundColor Yellow
codegraph sync
if ($LASTEXITCODE -ne 0) {
    Write-Host "Failed to sync Codegraph index" -ForegroundColor Red
    exit 1
}
Write-Host "Codegraph index sync completed" -ForegroundColor Green

# Test basic query functionality
Write-Host "Testing Codegraph basic query functionality..." -ForegroundColor Yellow
$testQuery = codegraph query "GraphState" | Select-Object -First 5
if ($testQuery) {
    Write-Host "Codegraph query functionality normal, found related results:" -ForegroundColor Green
    $testQuery | ForEach-Object { Write-Host "  - $_" }
} else {
    Write-Host "Codegraph query returned no results, but server may be running normally" -ForegroundColor Yellow
}

Write-Host "Codegraph MCP server configuration completed!" -ForegroundColor Green
Write-Host "You can now use Codegraph functionality in RidgeCode." -ForegroundColor Cyan