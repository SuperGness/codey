// Adapted from BaoZiFly-233/codex-tweaks-pet-drag-recovery (MIT).
// See licenses/PetDragRecovery/LICENSE and THIRD_PARTY_NOTICES.md.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;

// No injection, input hooks, process termination, registry writes or network access.
public static class PetWindowAgent {
    const int EXSTYLE = -20, LAYERED = 0x80000, TOPMOST = 8, TOOLWINDOW = 0x80;
    const uint FRAME_REFRESH = 0x37; // NOSIZE|NOMOVE|NOZORDER|NOACTIVATE|FRAMECHANGED
    static volatile bool closing;
    static Process parent;
    static long parentStart;
    static int SessionId() { using (var current = Process.GetCurrentProcess()) return current.SessionId; }
    static readonly int currentSession = SessionId();
    delegate bool EnumProc(IntPtr hwnd, IntPtr unused);
    [StructLayout(LayoutKind.Sequential)] struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr unused);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsWindow(IntPtr hwnd);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool SetProp(IntPtr hwnd, string name, IntPtr value);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern IntPtr GetProp(IntPtr hwnd, string name);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern IntPtr RemoveProp(IntPtr hwnd, string name);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder text, int count);
    [DllImport("user32.dll", SetLastError=true)] static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll", SetLastError=true)] static extern int GetWindowLong(IntPtr hwnd, int index);
    [DllImport("user32.dll", SetLastError=true)] static extern int SetWindowLong(IntPtr hwnd, int index, int value);
    [DllImport("user32.dll", SetLastError=true)] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int w, int h, uint flags);
    [DllImport("user32.dll")] static extern short GetAsyncKeyState(int key);
    [DllImport("user32.dll")] static extern IntPtr GetThreadDesktop(uint thread);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll", SetLastError=true)] static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
    [DllImport("user32.dll")] static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool GetUserObjectInformation(IntPtr obj, int index, StringBuilder info, uint length, out uint needed);

    sealed class WindowInfo {
        public long Handle, Start;
        public int Pid, X, Y, Width, Height, Style;
        public string Version;
    }
    static string Hex(int value) { return "0x" + value.ToString("X8"); }
    static string Quote(string value) {
        var text = new StringBuilder("\"");
        foreach (char c in value ?? "") {
            if (c == '"' || c == '\\') text.Append('\\').Append(c);
            else if (c < 32) text.Append("\\u").Append(((int)c).ToString("x4"));
            else text.Append(c);
        }
        return text.Append('"').ToString();
    }
    static void Emit(string json) {
        try { Console.Out.WriteLine(json); Console.Out.Flush(); }
        catch (IOException) { closing = true; }
    }
    static bool ParentAlive() {
        try { return !closing && !parent.HasExited && parent.StartTime.ToUniversalTime().Ticks == parentStart; }
        catch { return false; }
    }
    static string DesktopName(IntPtr handle) {
        uint needed;
        var value = new StringBuilder(256);
        return GetUserObjectInformation(handle, 2, value, 512, out needed) ? value.ToString() : "";
    }
    static bool InputDesktopAvailable() {
        IntPtr input = OpenInputDesktop(0, false, 1); // DESKTOP_READOBJECTS
        if (input == IntPtr.Zero) return false;
        try {
            string name = DesktopName(input);
            return name.Length > 0 && name == DesktopName(GetThreadDesktop(GetCurrentThreadId()));
        } finally { CloseDesktop(input); }
    }
    static bool ButtonDown() { return (GetAsyncKeyState(1) & 0x8000) != 0 || (GetAsyncKeyState(2) & 0x8000) != 0 || (GetAsyncKeyState(4) & 0x8000) != 0; }
    static bool ReadOwner(int pid, out long start, out string version) {
        start = 0; version = "";
        try {
            using (var process = Process.GetProcessById(pid)) {
                if (process.ProcessName != "ChatGPT" || process.SessionId != currentSession) return false;
                string prefix = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "WindowsApps") + Path.DirectorySeparatorChar;
                string file = Path.GetFullPath(process.MainModule.FileName);
                if (!file.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) return false;
                var match = Regex.Match(file.Substring(prefix.Length), @"^OpenAI\.Codex_([0-9.]+)_(x64|arm64)__2p2nqsd0c76g0\\app\\ChatGPT\.exe$", RegexOptions.IgnoreCase);
                if (!match.Success) return false;
                start = process.StartTime.ToUniversalTime().Ticks;
                version = match.Groups[1].Value;
                return true;
            }
        } catch { return false; }
    }
    static bool SameIdentity(WindowInfo window) {
        uint owner;
        long start; string version;
        GetWindowThreadProcessId(new IntPtr(window.Handle), out owner);
        return IsWindow(new IntPtr(window.Handle)) && owner == window.Pid && ReadOwner(window.Pid, out start, out version) && start == window.Start;
    }
    static List<WindowInfo> FindWindows() {
        var result = new List<WindowInfo>();
        var owners = new Dictionary<int, Tuple<long, string>>();
        foreach (var process in Process.GetProcessesByName("ChatGPT")) {
            try { long start; string version; if (ReadOwner(process.Id, out start, out version)) owners[process.Id] = Tuple.Create(start, version); }
            finally { process.Dispose(); }
        }
        EnumWindows(delegate(IntPtr hwnd, IntPtr unused) {
            uint pid; GetWindowThreadProcessId(hwnd, out pid);
            Tuple<long, string> owner;
            if (!owners.TryGetValue((int)pid, out owner) || !IsWindowVisible(hwnd)) return true;
            var name = new StringBuilder(256); GetClassName(hwnd, name, 256);
            if (name.ToString() != "Chrome_WidgetWin_1") return true;
            int style = GetWindowLong(hwnd, EXSTYLE);
            if ((style & (TOPMOST | TOOLWINDOW | LAYERED)) != (TOPMOST | TOOLWINDOW | LAYERED)) return true;
            Rect rect; if (!GetWindowRect(hwnd, out rect) || rect.Right <= rect.Left || rect.Bottom <= rect.Top) return true;
            result.Add(new WindowInfo { Handle=hwnd.ToInt64(), Pid=(int)pid, Start=owner.Item1, Version=owner.Item2,
                X=rect.Left, Y=rect.Top, Width=rect.Right-rect.Left, Height=rect.Bottom-rect.Top, Style=style });
            return true;
        }, IntPtr.Zero);
        return result;
    }
    static void Repair(long id, int pid, long start, long handle) {
        if (!InputDesktopAvailable() || ButtonDown()) { Emit("{\"id\":"+id+",\"ok\":false,\"code\":\"input_busy\"}"); return; }
        var candidates = FindWindows();
        if (candidates.Count != 1) { Emit("{\"id\":"+id+",\"ok\":false,\"code\":\"ambiguous_window\"}"); return; }
        var window = candidates[0];
        if (window.Pid != pid || window.Start != start || window.Handle != handle || !SameIdentity(window)) {
            Emit("{\"id\":"+id+",\"ok\":false,\"code\":\"window_changed\"}"); return;
        }
        IntPtr hwnd = new IntPtr(handle);
        int original = GetWindowLong(hwnd, EXSTYLE);
        if (original != window.Style) { Fail("window_changed"); return; }
        int temporary = original & ~LAYERED;
        bool modified = false, restored = false;
        string marker = "CodexTweaksPetDragRecovery-" + Guid.NewGuid().ToString("N");
        bool marked = false;
        string error = null;
        try {
            if (!ParentAlive()) { error = "stopping"; return; }
            marked = SetProp(hwnd, marker, new IntPtr(1));
            if (!marked || !SameIdentity(window) || GetProp(hwnd, marker) != new IntPtr(1)) throw new InvalidOperationException("window_changed");
            int previous = SetWindowLong(hwnd, EXSTYLE, temporary);
            if (previous == 0) throw new InvalidOperationException("clear_style_failed");
            modified = true;
            if (GetWindowLong(hwnd, EXSTYLE) != temporary) throw new InvalidOperationException("clear_style_failed");
            if (!SetWindowPos(hwnd, IntPtr.Zero, 0, 0, 0, 0, FRAME_REFRESH)) throw new InvalidOperationException("refresh_failed");
            var timer = Stopwatch.StartNew();
            while (timer.ElapsedMilliseconds < 3000 && ParentAlive() && InputDesktopAvailable() && !ButtonDown()) Thread.Sleep(50);
            if (timer.ElapsedMilliseconds < 3000) error = "reset_interrupted";
        } catch (Exception ex) { error = ex.Message; }
        finally {
            if (modified) {
                if (!SameIdentity(window) || GetProp(hwnd, marker) != new IntPtr(1)) error = "window_destroyed";
                else {
                    SetWindowLong(hwnd, EXSTYLE, original);
                    bool refreshed = SetWindowPos(hwnd, IntPtr.Zero, 0, 0, 0, 0, FRAME_REFRESH);
                    restored = GetWindowLong(hwnd, EXSTYLE) == original;
                    if (!restored) error = "restore_failed";
                    else if (!refreshed) error = "restore_refresh_failed";
                }
            }
            if (marked && GetProp(hwnd, marker) == new IntPtr(1)) RemoveProp(hwnd, marker);
            Emit("{\"id\":"+id+",\"ok\":"+(error == null && restored ? "true" : "false")+
                ",\"code\":"+Quote(error ?? "window_reset")+",\"original\":"+Quote(Hex(original))+
                ",\"temporary\":"+Quote(Hex(temporary))+",\"restoredExactly\":"+(restored ? "true" : "false")+"}");
        }
    }
    public static void RecoverOnce(int parentPid) {
        using (var gate = new Mutex(false, "Local\\CodexTweaksPetDragRecoveryV1")) {
            bool held = false;
            try {
                try { held = gate.WaitOne(0); } catch (AbandonedMutexException) { held = true; }
                if (!held) { Fail("already_running"); return; }
                parent = Process.GetProcessById(parentPid);
                parentStart = parent.StartTime.ToUniversalTime().Ticks;
                if (!InputDesktopAvailable() || ButtonDown()) { Fail("input_busy"); return; }
                var before = FindWindows();
                if (before.Count != 1) { Fail(before.Count == 0 ? "no_window" : "ambiguous_window"); return; }
                var window = before[0];
                // Wait for a stable window; never reset a window during a drag.
                var timer = Stopwatch.StartNew();
                while (timer.ElapsedMilliseconds < 2000) {
                    if (!ParentAlive() || !InputDesktopAvailable() || ButtonDown()) { Fail("input_busy"); return; }
                    Thread.Sleep(50);
                    var current = FindWindows();
                    if (current.Count != 1 || !Stable(window, current[0])) { Fail("window_changed"); return; }
                }
                Repair(1, window.Pid, window.Start, window.Handle);
            } catch { Fail("native_error"); }
            finally { closing = true; if (parent != null) parent.Dispose(); if (held) gate.ReleaseMutex(); }
        }
    }
    static bool Stable(WindowInfo a, WindowInfo b) {
        return a.Handle == b.Handle && a.Pid == b.Pid && a.Start == b.Start && a.Style == b.Style &&
            a.X == b.X && a.Y == b.Y && a.Width == b.Width && a.Height == b.Height;
    }
    static void Fail(string code) { Emit("{\"ok\":false,\"code\":" + Quote(code) + "}"); }
}
