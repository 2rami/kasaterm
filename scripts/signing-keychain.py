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


if __name__ == "__main__":
    unlock(sys.argv[1])
