"""Temporary probe: which way of closing a console delivers CTRL_CLOSE_EVENT here."""
import ctypes
import os
import subprocess
import sys
import time
import winreg
from ctypes import wintypes

CHILD = r"""
import ctypes, sys, time
log = sys.argv[1]
def handler(event):
    with open(log, "a") as f:
        f.write("event %d\n" % event)
    time.sleep(1)
    return 1
cb = ctypes.WINFUNCTYPE(ctypes.c_int, ctypes.c_uint)(handler)
k = ctypes.WinDLL("kernel32")
k.SetConsoleCtrlHandler(None, 0)
k.SetConsoleCtrlHandler(cb, 1)
with open(log, "a") as f:
    f.write("ready\n")
time.sleep(60)
"""

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
kernel32.GetConsoleWindow.restype = wintypes.HWND
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]

for key in (r"Console\%%Startup",):
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key) as k:
            for name in ("DelegationConsole", "DelegationTerminal"):
                try:
                    print(key, name, winreg.QueryValueEx(k, name)[0])
                except OSError as e:
                    print(key, name, "unset", e)
    except OSError as e:
        print(key, "missing", e)
print("session", os.environ.get("SESSIONNAME"), "user", os.environ.get("USERNAME"))



def spawn(tag):
    log = os.path.join(os.environ.get("RUNNER_TEMP", "."), f"probe-{tag}.log")
    if os.path.exists(log):
        os.remove(log)
    p = subprocess.Popen([sys.executable, "-I", "-c", CHILD, log],
                         creationflags=subprocess.CREATE_NEW_CONSOLE,
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL)
    for _ in range(100):
        if os.path.exists(log):
            break
        time.sleep(0.1)
    return p, log


WINDOW = r"""
import ctypes, sys
from ctypes import wintypes
k = ctypes.WinDLL("kernel32", use_last_error=True)
u = ctypes.WinDLL("user32", use_last_error=True)
k.GetConsoleWindow.restype = wintypes.HWND
u.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
k.FreeConsole()
if not k.AttachConsole(int(sys.argv[1])):
    sys.exit("AttachConsole failed %d" % ctypes.get_last_error())
w = k.GetConsoleWindow()
k.FreeConsole()
buf = ctypes.create_unicode_buffer(256)
u.GetClassNameW(w, buf, 256)
print(w or 0, buf.value)
"""


def window_of(pid):
    r = subprocess.run([sys.executable, "-I", "-c", WINDOW, str(pid)], capture_output=True,
                       text=True, stdin=subprocess.DEVNULL,
                       creationflags=subprocess.CREATE_NO_WINDOW)
    if r.returncode:
        return None, r.stderr.strip()
    w, cls = r.stdout.split(maxsplit=1)
    return int(w) or None, cls.strip()


def outcome(p, log, tag):
    try:
        code = p.wait(10)
    except subprocess.TimeoutExpired:
        p.kill()
        code = "alive after 10 s"
    print(f"RESULT {tag}: exit={code!r} log={open(log).read().split()!r}", flush=True)



def methods(prefix):
    p, log = spawn(prefix + "post")
    w, info = window_of(p.pid)
    print(prefix, "window", w, info)
    if w:
        print("PostMessage", user32.PostMessageW(w, 0x0010, 0, 0), ctypes.get_last_error())
    outcome(p, log, prefix + " PostMessage WM_CLOSE")
    p, log = spawn(prefix + "taskkill")
    r = subprocess.run(["taskkill", "/PID", str(p.pid)], capture_output=True, text=True,
                       stdin=subprocess.DEVNULL)
    print("taskkill", r.returncode, r.stdout.strip(), r.stderr.strip())
    outcome(p, log, prefix + " taskkill")


methods("default")
CONHOST = "{B23D10C0-E52E-411E-9D5B-C09FDF709C7D}"
with winreg.CreateKey(winreg.HKEY_CURRENT_USER, r"Console\%%Startup") as k:
    winreg.SetValueEx(k, "DelegationConsole", 0, winreg.REG_SZ, CONHOST)
    winreg.SetValueEx(k, "DelegationTerminal", 0, winreg.REG_SZ, CONHOST)
methods("conhost")
