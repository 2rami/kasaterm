#!/usr/bin/env python3
"""Unlock the optional signing keychain without putting its password in argv."""
import ctypes
import pathlib
import sys


def unlock(path: str) -> None:
    library = ctypes.CDLL("/System/Library/Frameworks/Security.framework/Security")
    library.SecKeychainOpen.argtypes = [ctypes.c_char_p, ctypes.POINTER(ctypes.c_void_p)]
    library.SecKeychainOpen.restype = ctypes.c_int32
    library.SecKeychainUnlock.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_char_p, ctypes.c_bool]
    library.SecKeychainUnlock.restype = ctypes.c_int32
    keychain = ctypes.c_void_p()
    status = library.SecKeychainOpen(path.encode(), ctypes.byref(keychain))
    if status:
        raise RuntimeError(f"signing keychain open failed: {status}")
    password = pathlib.Path(path).with_name("keychain-password").read_bytes()
    status = library.SecKeychainUnlock(keychain, len(password), password, True)
    if status:
        raise RuntimeError(f"signing keychain unlock failed: {status}")


def probe(path: str) -> bool:
    """That keychain's Apple identity signs without a password dialog — true/false.

    codesign pops a GUI prompt when the key's ACL does not admit it, and a build
    that hits the prompt just hangs. So try once on a scratch binary with a short
    deadline; a timeout means the key is closed and the caller must not use it.
    """
    import shutil
    import subprocess
    import tempfile

    lst = subprocess.run(
        ["security", "find-identity", "-v", "-p", "codesigning", path],
        capture_output=True, text=True,
    ).stdout
    ident = next(
        (line.split()[1] for line in lst.splitlines()
         if '"Developer ID Application: ' in line or '"Apple Development: ' in line),
        None,
    )
    if not ident:
        return False
    with tempfile.TemporaryDirectory() as d:
        target = pathlib.Path(d) / "probe"
        shutil.copy("/bin/ls", target)
        try:
            r = subprocess.run(
                ["codesign", "-f", "-s", ident, "--keychain", path, str(target)],
                capture_output=True, timeout=6,
            )
        except subprocess.TimeoutExpired:
            return False
        return r.returncode == 0


if __name__ == "__main__":
    if sys.argv[1] == "--probe":
        sys.exit(0 if probe(sys.argv[2]) else 1)
    unlock(sys.argv[1])
