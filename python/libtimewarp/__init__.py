"""The Python tool over libtimewarp's C library.

Attach to a session by the path of its session file, read what it is doing, and change it:

    from libtimewarp import Controller, Family

    with Controller(path) as session:
        print(session.get(Family.REALTIME))
        with session.transaction() as change:
            change.shift(Family.REALTIME, -2 * 3600 * 10**9)

The library underneath offers primitives only. Anything with memory, like a freeze that remembers
the rate to come back to, lives here; :mod:`libtimewarp.cli` is where that one is built.
"""

from .controller import (
    Code,
    Controller,
    Error,
    Family,
    FamilyInfo,
    Fault,
    FaultCode,
    Info,
    Keeper,
    Transaction,
    attach,
)
from .discovery import SHARED_FILE, Found, select, session_dir, sessions, shared_file_of

__all__ = [
    "SHARED_FILE",
    "Code",
    "Controller",
    "Error",
    "Family",
    "FamilyInfo",
    "Fault",
    "FaultCode",
    "Found",
    "Info",
    "Keeper",
    "Transaction",
    "attach",
    "select",
    "session_dir",
    "sessions",
    "shared_file_of",
]
