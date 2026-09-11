"""The ctypes surface over ``libtimewarp_controller``.

One declaration per line of ``include/libtimewarp_controller.h``. Nothing above this module
touches ctypes, and nothing here knows what a command-line tool is.
"""

from __future__ import annotations

import ctypes
import ctypes.util
import os
import sys
from pathlib import Path
from typing import Iterator

#: As long as the session file reserves for the hook path.
HOOK_PATH_MAX = 4096

#: How many families the arrays in ``struct libtimewarp_info`` carry.
FAMILIES = 4

#: The variable naming the shared object outright, for a build that is not installed anywhere.
LIBRARY_VARIABLE = "LIBTIMEWARP_CONTROLLER_LIB"

_STEM = "timewarp_controller"


class Info(ctypes.Structure):
    """``struct libtimewarp_info``, field for field."""

    _fields_ = [
        ("keeper", ctypes.c_uint32),
        ("emulate_vdso", ctypes.c_uint32),
        ("fail_closed", ctypes.c_uint32),
        ("creator_pid", ctypes.c_uint32),
        ("creation_time", ctypes.c_int64),
        ("fault_code", ctypes.c_uint32),
        ("fault_pid", ctypes.c_uint32),
        ("rate", ctypes.c_double * FAMILIES),
        ("timebase", ctypes.c_int32 * FAMILIES),
        ("hook_path", ctypes.c_char * HOOK_PATH_MAX),
    ]


def _file_names() -> tuple[str, ...]:
    if sys.platform == "darwin":
        return (f"lib{_STEM}.dylib",)
    return (f"lib{_STEM}.so",)


def _candidates() -> Iterator[str]:
    """Where to look for the shared object, nearest first."""
    named = os.environ.get(LIBRARY_VARIABLE)
    if named:
        yield named
    here = Path(__file__).resolve().parent
    # An in-tree build: the package, the repository beside it, and cargo's output directories.
    for base in (here, here.parent, here.parent.parent):
        for name in _file_names():
            yield str(base / name)
            yield str(base / "target" / "release" / name)
            yield str(base / "target" / "debug" / name)
    found = ctypes.util.find_library(_STEM)
    if found:
        yield found
    # Last, the loader's own search path.
    yield from _file_names()


def _load() -> ctypes.CDLL:
    tried = []
    for candidate in _candidates():
        try:
            return ctypes.CDLL(candidate)
        except OSError:
            tried.append(candidate)
    raise OSError(
        "libtimewarp_controller was not found. Build it with `cargo build -p capi`, then point "
        f"{LIBRARY_VARIABLE} at the shared object. Tried: " + ", ".join(tried)
    )


def _declare(lib: ctypes.CDLL) -> ctypes.CDLL:
    handle = ctypes.c_void_p
    lib.libtimewarp_last_error.argtypes = []
    lib.libtimewarp_last_error.restype = ctypes.c_char_p

    lib.libtimewarp_attach.argtypes = [ctypes.c_char_p, ctypes.POINTER(handle)]
    lib.libtimewarp_attach.restype = ctypes.c_int32

    lib.libtimewarp_close.argtypes = [handle]
    lib.libtimewarp_close.restype = None

    lib.libtimewarp_get.argtypes = [handle, ctypes.c_uint32, ctypes.POINTER(ctypes.c_int64)]
    lib.libtimewarp_get.restype = ctypes.c_int32

    lib.libtimewarp_info.argtypes = [handle, ctypes.POINTER(Info)]
    lib.libtimewarp_info.restype = ctypes.c_int32

    lib.libtimewarp_begin.argtypes = [handle, ctypes.POINTER(handle)]
    lib.libtimewarp_begin.restype = ctypes.c_int32

    for name in ("libtimewarp_shift", "libtimewarp_set_time"):
        call = getattr(lib, name)
        call.argtypes = [handle, ctypes.c_uint32, ctypes.c_int64]
        call.restype = ctypes.c_int32

    lib.libtimewarp_set_rate.argtypes = [handle, ctypes.c_uint32, ctypes.c_double]
    lib.libtimewarp_set_rate.restype = ctypes.c_int32

    lib.libtimewarp_commit.argtypes = [handle]
    lib.libtimewarp_commit.restype = ctypes.c_int32

    lib.libtimewarp_abort.argtypes = [handle]
    lib.libtimewarp_abort.restype = None
    return lib


_LIBRARY: ctypes.CDLL | None = None


def library() -> ctypes.CDLL:
    """The shared object, loaded and declared once."""
    global _LIBRARY
    if _LIBRARY is None:
        _LIBRARY = _declare(_load())
    return _LIBRARY


def last_error() -> str:
    """The message beside the code the last call on this thread returned."""
    message = library().libtimewarp_last_error()
    return message.decode("utf-8", "replace") if message else ""
