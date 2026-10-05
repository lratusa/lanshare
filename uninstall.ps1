# 局域网快传 LanShare 卸载脚本（安装时会复制到安装目录，“设置 → 应用 → 卸载”调用的就是它）
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File uninstall.ps1 [-CurrentUser]
# 正在运行的 LanShare 会被自动结束。共享文件夹（下载\LanShare）里的文件不会被删除。
# 本文件必须保存为 UTF-8 带 BOM，否则 Windows PowerShell 5.1 会按本地代码页读错中文。
param([switch]$CurrentUser)
$ErrorActionPreference = "Stop"

$AppName     = "LanShare"
$DisplayName = "局域网快传 LanShare"
if ($CurrentUser) {
    $DefaultDir   = Join-Path $env:LOCALAPPDATA "Programs\$AppName"
    $StartMenu    = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
    $UninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$AppName"
} else {
    $DefaultDir   = Join-Path $env:ProgramFiles $AppName
    $StartMenu    = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs"
    $UninstallKey = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$AppName"
}
# 安装目录以安装时登记的 InstallLocation 为准，不按脚本自己所在的位置猜：
# 这个脚本可能被拷到别处运行（比如被传进了同样叫 LanShare 的共享文件夹，独立安全审查 M-1）。
$InstallDir = $DefaultDir
$registered = (Get-ItemProperty -Path $UninstallKey -ErrorAction SilentlyContinue).InstallLocation
if ($registered -and (Split-Path $registered -Leaf) -eq $AppName) { $InstallDir = $registered }
# 安装时放进安装目录的全部文件：卸载只删这些，然后只在目录空了时删目录
$InstalledFiles = "LanShare.exe", "uninstall.ps1", "README.md"
$Exe      = Join-Path $InstallDir "LanShare.exe"
$Shortcut = Join-Path $StartMenu "$DisplayName.lnk"
$Pinned   = Join-Path $env:APPDATA "Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\$DisplayName.lnk"
$Log      = Join-Path $env:TEMP "LanShare-uninstall.log"

function Test-Admin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($identity)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Log($message) {
    Write-Host $message
    Add-Content -Path $Log -Value $message -Encoding UTF8
}

function Show-Failure($message) {
    try { (New-Object -ComObject WScript.Shell).Popup($message, 30, $DisplayName, 0x10) | Out-Null } catch { }
}

# 目录空了才删，返回 $true 表示目录里还有别的东西、保留了下来
function Remove-IfEmpty($dir) {
    if (-not (Test-Path -LiteralPath $dir)) { return $false }
    if (Get-ChildItem -LiteralPath $dir -Force | Select-Object -First 1) { return $true }
    Remove-Item -LiteralPath $dir -Force
    return $false
}

# 只删指向本次卸载的 exe 的快捷方式：同名的可能属于另一份安装（比如所有用户版 + 当前用户版并存）
function Remove-OwnShortcut($path) {
    if (-not (Test-Path $path)) { return }
    $target = (New-Object -ComObject WScript.Shell).CreateShortcut($path).TargetPath
    if ($target -eq $Exe) { Remove-Item $path -Force } else { Log "保留 $path（它指向 $target）" }
}

if (-not $CurrentUser -and -not (Test-Admin)) {
    Remove-Item $Log -ErrorAction SilentlyContinue
    $proc = Start-Process powershell.exe -Verb RunAs -Wait -PassThru `
        -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    if (Test-Path $Log) { Get-Content $Log -Encoding UTF8 }
    exit $proc.ExitCode
}

Remove-Item $Log -ErrorAction SilentlyContinue
try {
    # 防止误删：目录名必须是 LanShare
    if ((Split-Path $InstallDir -Leaf) -ne $AppName) { throw "安装目录看起来不对：$InstallDir" }

    $procs = @(Get-Process -Name $AppName -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $Exe })
    foreach ($p in $procs) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
    foreach ($p in $procs) { [void]$p.WaitForExit(10000) }
    if ($procs.Count -gt 0) { Log "已结束正在运行的 LanShare（$($procs.Count) 个进程）" }

    Remove-OwnShortcut $Shortcut
    Remove-OwnShortcut $Pinned
    if (-not $CurrentUser) {
        Get-NetFirewallRule -Group $AppName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    }

    # 当前目录在安装目录里时目录删不掉，先挪开
    Set-Location $env:TEMP
    [Environment]::CurrentDirectory = $env:TEMP
    # 绝不递归删除：只删安装时放进去的文件（杀毒软件扫描时可能占着，重试几次）
    foreach ($name in $InstalledFiles) {
        $file = Join-Path $InstallDir $name
        for ($i = 1; Test-Path -LiteralPath $file; $i++) {
            try { Remove-Item -LiteralPath $file -Force }
            catch { if ($i -ge 10) { throw }; Start-Sleep -Milliseconds 500 }
        }
    }
    $kept = Remove-IfEmpty $InstallDir
    # 日志和单实例记录，同样只删认识的文件
    $DataDir = Join-Path $env:LOCALAPPDATA $AppName
    foreach ($name in "lanshare.log", "instance.json", "instance.json.tmp") {
        Remove-Item -LiteralPath (Join-Path $DataDir $name) -Force -ErrorAction SilentlyContinue
    }
    [void](Remove-IfEmpty $DataDir)

    # 注册表项最后删：前面任何一步失败，“设置 → 应用”里还留着入口，可以再点一次卸载
    Remove-Item $UninstallKey -Recurse -Force -ErrorAction SilentlyContinue
    if ($kept) { Log "安装目录里还有不是安装程序放进去的文件，已保留：$InstallDir" }
    Log "已卸载 $DisplayName。共享文件夹（下载\LanShare）里的文件没有动。"
    exit 0
} catch {
    $message = "卸载失败：$($_.Exception.Message)"
    Log $message
    Show-Failure $message
    exit 1
}
