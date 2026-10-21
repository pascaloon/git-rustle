# Display-enabled native paint on a private desktop; never switches input desktops.
# ArgumentList uses Start-Process semantics (already quoted command fragments).
if (-not ('GitRustle.Bench.NativeDesktopProcess' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
namespace GitRustle.Bench {
public sealed class NativeDesktopProcess : IDisposable {
    [StructLayout(LayoutKind.Sequential)]
    struct SecurityAttributes { public int Length; public IntPtr Descriptor; public int Inherit; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct StartupInfo {
        public int Size; public string Reserved, Desktop, Title;
        public int X, Y, XSize, YSize, XCountChars, YCountChars, FillAttribute, Flags;
        public short ShowWindow, ReservedLength;
        public IntPtr ReservedBytes, StandardInput, StandardOutput, StandardError;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct ProcessInformation { public IntPtr Process, Thread; public int ProcessId, ThreadId; }
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr CreateDesktopW(string name, IntPtr device, IntPtr mode, int flags, uint access, IntPtr security);
    [DllImport("user32.dll", SetLastError=true)] static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr CreateFileW(string name, uint access, uint share, ref SecurityAttributes security, uint creation, uint attributes, IntPtr template);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool CreateProcessW(string application, StringBuilder command, IntPtr processSecurity, IntPtr threadSecurity, bool inherit, uint flags, IntPtr environment, string directory, ref StartupInfo startup, out ProcessInformation info);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool CloseHandle(IntPtr handle);
    IntPtr desktop;
    Process process;
    int? exitCode;
    public int Id { get; private set; }
    public string DesktopName { get; private set; }
    NativeDesktopProcess() {}
    static void Close(ref IntPtr handle) { if(handle!=IntPtr.Zero && handle!=new IntPtr(-1)) CloseHandle(handle); handle=IntPtr.Zero; }
    static IntPtr Stream(string path, uint access, uint creation, ref SecurityAttributes security) {
        IntPtr handle=CreateFileW(path,access,3,ref security,creation,0x80,IntPtr.Zero);
        if(handle==new IntPtr(-1)) throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot open benchmark stream: "+path);
        return handle;
    }
    public static NativeDesktopProcess Start(string executable,string arguments,string stderr,string directory) {
        var result=new NativeDesktopProcess();
        result.DesktopName="git-rustle-bench-"+Guid.NewGuid().ToString("N");
        result.desktop=CreateDesktopW(result.DesktopName,IntPtr.Zero,IntPtr.Zero,0,0x00FF,IntPtr.Zero);
        if(result.desktop==IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot create isolated benchmark desktop");
        IntPtr input=IntPtr.Zero,output=IntPtr.Zero,error=IntPtr.Zero;
        ProcessInformation info=new ProcessInformation();
        try {
            var security=new SecurityAttributes { Length=Marshal.SizeOf(typeof(SecurityAttributes)), Inherit=1 };
            input=Stream("NUL",0x80000000,3,ref security);
            output=Stream("NUL",0x40000000,3,ref security);
            error=Stream(stderr,0x40000000,2,ref security);
            var startup=new StartupInfo { Size=Marshal.SizeOf(typeof(StartupInfo)), Desktop=result.DesktopName,
                Flags=0x101, ShowWindow=5, StandardInput=input, StandardOutput=output, StandardError=error };
            var command=new StringBuilder("\""+executable+"\""+(String.IsNullOrEmpty(arguments)?"":" "+arguments));
            // Inherit the script's isolated environment, with no new console.
            if(!CreateProcessW(executable,command,IntPtr.Zero,IntPtr.Zero,true,0x08000000,IntPtr.Zero,directory,ref startup,out info))
                throw new Win32Exception(Marshal.GetLastWin32Error(),"Cannot launch isolated benchmark");
            result.Id=info.ProcessId;
            result.process=Process.GetProcessById(info.ProcessId);
            IntPtr stable=result.process.Handle;
            return result;
        } catch {
            if(info.ProcessId!=0) { try { using(var failed=Process.GetProcessById(info.ProcessId)) { if(!failed.HasExited) failed.Kill(true);failed.WaitForExit(); } } catch {} }
            result.Dispose(); throw;
        } finally { Close(ref info.Thread);Close(ref info.Process);Close(ref input);Close(ref output);Close(ref error); }
    }
    void ReleaseDesktop() { if(desktop!=IntPtr.Zero) { CloseDesktop(desktop);desktop=IntPtr.Zero; } }
    public bool WaitForExit(int milliseconds) {
        if(exitCode.HasValue) return true;
        if(!process.WaitForExit(milliseconds)) return false;
        exitCode=process.ExitCode;process.Dispose();process=null;ReleaseDesktop();return true;
    }
    public void Refresh() { if(exitCode.HasValue) return;process.Refresh();if(process.HasExited) { exitCode=process.ExitCode;process.Dispose();process=null;ReleaseDesktop(); } }
    public int ExitCode { get { Refresh();return exitCode.HasValue?exitCode.Value:process.ExitCode; } }
    public void Kill() {
        if(process!=null && !exitCode.HasValue) { process.Refresh();if(!process.HasExited) process.Kill(true);process.WaitForExit();exitCode=process.ExitCode;process.Dispose();process=null; }
        ReleaseDesktop();
    }
    public void Dispose() { Kill();if(process!=null) { process.Dispose();process=null; } ReleaseDesktop();GC.SuppressFinalize(this); }
    ~NativeDesktopProcess() { ReleaseDesktop(); }
}
}
'@
}
function Start-BenchmarkNativeProcess {
    param([Parameter(Mandatory)][string]$FilePath,[string[]]$ArgumentList=@(),[Parameter(Mandatory)][string]$RedirectStandardError)
    [GitRustle.Bench.NativeDesktopProcess]::Start((Resolve-Path -LiteralPath $FilePath).Path,($ArgumentList -join ' '),[IO.Path]::GetFullPath($RedirectStandardError),(Get-Location).Path)
}
