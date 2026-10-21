# Internal bounded worker. Use Save-BenchmarkNativeScreenshot, not arbitrary HWNDs.
param([Parameter(Mandatory)][string]$RequestPath)
$ErrorActionPreference = 'Stop'
try {
    $request = Get-Content -LiteralPath $RequestPath -Raw | ConvertFrom-Json
    if ($request.desktop -cnotmatch '^git-rustle-bench-[0-9a-f]{32}$') { throw 'Not a private benchmark desktop' }
    $outputPath = [IO.Path]::GetFullPath($request.outputPath)
    $metadataPath = [IO.Path]::ChangeExtension($outputPath, '.json')
    if ((Test-Path -LiteralPath $outputPath) -or (Test-Path -LiteralPath $metadataPath)) { throw 'Screenshot output already exists' }
    Add-Type @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
namespace GitRustle.Bench {
public static class WindowCapture {
    public delegate bool EnumProc(IntPtr window, IntPtr param);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr OpenDesktopW(string name,uint flags,bool inherit,uint access);
    [DllImport("user32.dll", SetLastError=true)] static extern IntPtr OpenInputDesktop(uint flags,bool inherit,uint access);
    [DllImport("user32.dll")] static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool GetUserObjectInformationW(IntPtr handle,int index,StringBuilder value,uint length,out uint needed);
    [DllImport("user32.dll", SetLastError=true)] static extern bool EnumDesktopWindows(IntPtr desktop,EnumProc callback,IntPtr param);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window,out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr window,out Rect rect);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool PrintWindow(IntPtr window,IntPtr dc,uint flags);
    [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32.dll", SetLastError=true)] static extern bool PostMessageW(IntPtr window,uint message,IntPtr wparam,IntPtr lparam);
    public static IntPtr OpenPrivateDesktop(string name) {
        if(InputDesktop()==name) throw new InvalidOperationException("Refusing input desktop");
        var handle=OpenDesktopW(name,0,false,0x0041);
        if(handle==IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot open private desktop");
        return handle;
    }
    public static void ReleasePrivateDesktop(IntPtr handle) {CloseDesktop(handle);}
    static string Name(IntPtr desktop) {
        var text=new StringBuilder(256); uint needed;
        if(desktop==IntPtr.Zero || !GetUserObjectInformationW(desktop,2,text,512,out needed))
            throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot verify desktop identity");
        return text.ToString();
    }
    public static string InputDesktop() {
        var handle=OpenInputDesktop(0,false,1);
        if(handle==IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot verify input desktop");
        try {return Name(handle);} finally {CloseDesktop(handle);}
    }
    public static void Verify(IntPtr window,int pid,string desktop) {
        uint owner; var thread=GetWindowThreadProcessId(window,out owner);
        if(thread==0 || owner!=pid || InputDesktop()==desktop)
            throw new InvalidOperationException("Capture target is not the owned non-current desktop window");
        var handle=OpenDesktopW(desktop,0,false,0x0041);
        if(handle==IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot verify private desktop");
        try {
            bool found=false;
            EnumProc callback=(candidate,param)=>{if(candidate==window) found=true;return true;};
            if(!EnumDesktopWindows(handle,callback,IntPtr.Zero) || !found)
                throw new InvalidOperationException("Owned window is not on the expected private desktop");
        } finally {CloseDesktop(handle);}
    }
    public static IntPtr Find(string desktopName,int pid) {
        if(InputDesktop()==desktopName) throw new InvalidOperationException("Refusing capture on input desktop");
        var desktop=OpenDesktopW(desktopName,0,false,0x0041);
        if(desktop==IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot open private desktop");
        try {
            IntPtr found=IntPtr.Zero; int count=0;
            EnumProc callback=(window,param)=> {
                uint owner; GetWindowThreadProcessId(window,out owner); Rect rect;
                if(owner==pid && IsWindowVisible(window) && GetClientRect(window,out rect) && rect.Right>=64 && rect.Bottom>=64) {found=window;count++;}
                return true;
            };
            if(!EnumDesktopWindows(desktop,callback,IntPtr.Zero)) throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot enumerate private windows");
            if(count!=1) throw new InvalidOperationException("Expected exactly one owned application window; found "+count);
            Verify(found,pid,desktopName);return found;
        } finally {CloseDesktop(desktop);}
    }
    public static void CloseApplication(IntPtr window,int pid,string desktop) {
        Verify(window,pid,desktop);
        // Lifecycle only: no mouse, keyboard, foreground activation or desktop switch.
        if(!PostMessageW(window,0x0010,IntPtr.Zero,IntPtr.Zero)) throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot close captured application");
    }
}
}
'@
    $appProcess = [Diagnostics.Process]::GetProcessById([int]$request.processId)
    $bitmap = $null; $graphics = $null; $oldDpi = [IntPtr]::Zero; $desktopHandle = [IntPtr]::Zero
    try {
        if ($appProcess.StartTime.ToUniversalTime().Ticks.ToString() -cne $request.processStartTimeUtcTicks) { throw 'Capture process identity changed' }
        # Prevent DPI virtualization from allocating a bitmap smaller than actual pixels.
        $oldDpi = [GitRustle.Bench.WindowCapture]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
        if ($oldDpi -eq [IntPtr]::Zero) { throw 'Cannot enable per-monitor screenshot DPI awareness' }
        # Keep access to the foreign desktop alive until all HWND operations finish.
        $desktopHandle = [GitRustle.Bench.WindowCapture]::OpenPrivateDesktop($request.desktop)
        $window = [GitRustle.Bench.WindowCapture]::Find($request.desktop, $request.processId)
        $rect = [GitRustle.Bench.WindowCapture+Rect]::new()
        if (![GitRustle.Bench.WindowCapture]::GetClientRect($window,[ref]$rect)) { throw 'Cannot read application client bounds' }
        $width = $rect.Right; $height = $rect.Bottom
        if ($width -lt 64 -or $height -lt 64 -or $width -gt 8192 -or $height -gt 8192 -or [long]$width * $height -gt 32000000) { throw 'Invalid or excessive screenshot dimensions' }
        $bitmap = [Drawing.Bitmap]::new($width,$height,[Drawing.Imaging.PixelFormat]::Format24bppRgb)
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        $sentinel = [Drawing.Color]::FromArgb(255,3,253,171)
        $graphics.Clear($sentinel)
        $clock = [Diagnostics.Stopwatch]::StartNew()
        $dc = $graphics.GetHdc()
        try {
            [GitRustle.Bench.WindowCapture]::Verify($window,$request.processId,$request.desktop)
            # PW_CLIENTONLY | PW_RENDERFULLCONTENT; never a screen DC or fallback.
            if (![GitRustle.Bench.WindowCapture]::PrintWindow($window,$dc,3)) { throw 'PrintWindow failed; no desktop-capture fallback' }
        } finally { $graphics.ReleaseHdc($dc) }
        $clock.Stop()
        [GitRustle.Bench.WindowCapture]::Verify($window,$request.processId,$request.desktop)
        $after = [GitRustle.Bench.WindowCapture+Rect]::new()
        if (![GitRustle.Bench.WindowCapture]::GetClientRect($window,[ref]$after) -or $after.Right -ne $width -or $after.Bottom -ne $height) { throw 'Application resized during capture' }
        $colors = @{}; $sentinelSamples = 0; $sampleCount = 0
        # Includes all edges. A sanity check, not proof of complete/beautiful rendering.
        foreach ($row in 0..63) {
            foreach ($col in 0..95) {
                $pixel = $bitmap.GetPixel([int][Math]::Floor($col * ($width-1) / 95),[int][Math]::Floor($row * ($height-1) / 63)).ToArgb()
                if ($pixel -eq $sentinel.ToArgb()) { $sentinelSamples++ }
                if (!$colors.ContainsKey($pixel)) { $colors[$pixel] = 0 }
                $colors[$pixel]++; $sampleCount++
            }
        }
        $dominantFraction = ($colors.Values | Measure-Object -Maximum).Maximum / $sampleCount
        if ($sentinelSamples -gt 0 -or $colors.Count -lt 16 -or $dominantFraction -gt 0.995) {
            throw "Blank/incomplete screenshot: colors=$($colors.Count), sentinel=$sentinelSamples, dominant=$dominantFraction"
        }
        $stream = [IO.File]::Open($outputPath,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
        try { $bitmap.Save($stream,[Drawing.Imaging.ImageFormat]::Png) } finally { $stream.Dispose() }
        $metadata = [ordered]@{
            schemaVersion = 1; method = 'PrintWindow-client-private-desktop'; capturedUtc = [DateTime]::UtcNow.ToString('o')
            desktop = $request.desktop; inputDesktop = [GitRustle.Bench.WindowCapture]::InputDesktop()
            processId = $request.processId; processStartTimeUtcTicks = $request.processStartTimeUtcTicks
            windowHandle = $window.ToInt64().ToString(); width = $width; height = $height
            dpi = [GitRustle.Bench.WindowCapture]::GetDpiForWindow($window); captureMs = $clock.Elapsed.TotalMilliseconds
            pixelValidation = @{ passed = $true; sampledPixels = $sampleCount; uniqueColors = $colors.Count; sentinelSamples = $sentinelSamples; dominantFraction = $dominantFraction }
            sha256 = (Get-FileHash -LiteralPath $outputPath -Algorithm SHA256).Hash
            closeRequested = [bool]$request.closeAfterCapture
            limitations = 'Client pixels only; inspect the PNG. Excludes physical input, desktop composition, display scanout. Not a performance sample.'
        }
        $metadata | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $metadataPath -Encoding utf8
        if ($request.closeAfterCapture) { [GitRustle.Bench.WindowCapture]::CloseApplication($window,$request.processId,$request.desktop) }
    } finally {
        if ($null -ne $graphics) { $graphics.Dispose() }
        if ($null -ne $bitmap) { $bitmap.Dispose() }
        if ($oldDpi -ne [IntPtr]::Zero) { [GitRustle.Bench.WindowCapture]::SetThreadDpiAwarenessContext($oldDpi) | Out-Null }
        if ($desktopHandle -ne [IntPtr]::Zero) { [GitRustle.Bench.WindowCapture]::ReleasePrivateDesktop($desktopHandle) }
        $appProcess.Dispose()
    }
} catch {
    [Console]::Error.WriteLine($_.ToString())
    exit 1
}
