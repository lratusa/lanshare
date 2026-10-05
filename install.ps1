# 局域网快传 LanShare 安装 / 更新脚本（重复运行即覆盖更新；正在运行的会被自动结束）
#
#   默认：装到 C:\Program Files\LanShare（需要管理员，会弹 UAC），
#         在所有用户的开始菜单里加快捷方式，登记到“设置 → 应用”，放行入站防火墙（专用和公用网络）。
#   -CurrentUser：装到 %LOCALAPPDATA%\Programs\LanShare，不需要管理员，不改防火墙。
#
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1 [-CurrentUser]
# 本文件必须保存为 UTF-8 带 BOM，否则 Windows PowerShell 5.1 会按本地代码页读错中文。
param([switch]$CurrentUser)
$ErrorActionPreference = "Stop"

$AppName     = "LanShare"
$DisplayName = "局域网快传 LanShare"
$Version     = "2.0.0"
if ($CurrentUser) {
    $InstallDir   = Join-Path $env:LOCALAPPDATA "Programs\$AppName"
    $StartMenu    = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
    $UninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$AppName"
} else {
    $InstallDir   = Join-Path $env:ProgramFiles $AppName
    $StartMenu    = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs"
    $UninstallKey = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$AppName"
}
$Exe      = Join-Path $InstallDir "LanShare.exe"
$Shortcut = Join-Path $StartMenu "$DisplayName.lnk"
$Log      = Join-Path $env:TEMP "LanShare-install.log"

function Test-Admin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($identity)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Log($message) {
    Write-Host $message
    Add-Content -Path $Log -Value $message -Encoding UTF8
}

# 提权后的窗口一闪就关，从“设置”里调用时也没有控制台：出错一律弹窗（30 秒后自动关闭）
function Show-Failure($message) {
    try { (New-Object -ComObject WScript.Shell).Popup($message, 30, $DisplayName, 0x10) | Out-Null } catch { }
}

# 结束从安装目录启动的 LanShare（v1、v2 都算），等它真正退出，exe 才能被覆盖
function Stop-Running {
    $procs = @(Get-Process -Name $AppName -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $Exe })
    foreach ($p in $procs) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
    foreach ($p in $procs) { [void]$p.WaitForExit(10000) }
    if ($procs.Count -gt 0) { Log "已结束正在运行的 LanShare（$($procs.Count) 个进程）" }
}

# 杀毒软件可能还在扫刚释放的文件，覆盖失败时重试几次
function Copy-WithRetry($from, $to) {
    for ($i = 1; ; $i++) {
        try { Copy-Item $from $to -Force; return }
        catch { if ($i -ge 10) { throw }; Start-Sleep -Milliseconds 500 }
    }
}

if (-not $CurrentUser -and -not (Test-Admin)) {
    # 提权后在新窗口里执行，结果写进日志，再在这里显示出来
    Remove-Item $Log -ErrorAction SilentlyContinue
    $proc = Start-Process powershell.exe -Verb RunAs -Wait -PassThru `
        -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    if (Test-Path $Log) { Get-Content $Log -Encoding UTF8 }
    exit $proc.ExitCode
}

Remove-Item $Log -ErrorAction SilentlyContinue
try {
    $source = Join-Path $PSScriptRoot "dist\LanShare.exe"
    if (-not (Test-Path $source)) { throw "找不到 $source，请先运行 build.ps1 打包" }
    Stop-Running

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-WithRetry $source $Exe
    foreach ($name in "uninstall.ps1", "README.md") {
        Copy-WithRetry (Join-Path $PSScriptRoot $name) (Join-Path $InstallDir $name)
    }
    Log "已复制到 $InstallDir"

    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($Shortcut)
    $link.TargetPath = $Exe
    $link.WorkingDirectory = $InstallDir
    $link.IconLocation = "$Exe,0"
    $link.Description = "手机和电脑用浏览器互传文件和文字（端到端加密）"
    $link.Save()
    Log "开始菜单：$Shortcut"

    $uninstallCmd = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$InstallDir\uninstall.ps1`""
    if ($CurrentUser) { $uninstallCmd += " -CurrentUser" }
    $sizeKB = [int]((Get-ChildItem $InstallDir -File | Measure-Object -Property Length -Sum).Sum / 1KB)
    New-Item -Path $UninstallKey -Force | Out-Null
    $strings = [ordered]@{
        DisplayName     = $DisplayName
        DisplayVersion  = $Version
        Publisher       = $AppName
        DisplayIcon     = "$Exe,0"
        InstallLocation = $InstallDir
        UninstallString = $uninstallCmd
    }
    foreach ($key in $strings.Keys) {
        New-ItemProperty -Path $UninstallKey -Name $key -Value $strings[$key] -PropertyType String -Force | Out-Null
    }
    foreach ($pair in @(@("NoModify", 1), @("NoRepair", 1), @("EstimatedSize", $sizeKB))) {
        New-ItemProperty -Path $UninstallKey -Name $pair[0] -Value $pair[1] -PropertyType DWord -Force | Out-Null
    }
    Log "已登记到 设置 → 应用"

    if (-not $CurrentUser) {
        Get-NetFirewallRule -Group $AppName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
        # 所有网络类型都放行：Windows 常把家里的 WiFi 默认归为“公用网络”，只放行“专用”会让手机连不上。
        # 而且只要程序已有任何一条规则，Windows 就不再弹窗询问，连接会被静默拦截。
        New-NetFirewallRule -DisplayName $DisplayName -Group $AppName -Direction Inbound -Action Allow `
            -Program $Exe -Protocol TCP -Profile Any | Out-Null
        Log "防火墙：已放行入站连接（专用和公用网络）"
    }
    Log "安装完成：$DisplayName $Version（从开始菜单或任务栏启动）"
    exit 0
} catch {
    $message = "安装失败：$($_.Exception.Message)"
    Log $message
    Show-Failure $message
    exit 1
}
