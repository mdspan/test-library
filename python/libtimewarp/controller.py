"""Reading and changing a session, one Python object per handle in the C header.

The library offers primitives: a read, and a transaction that stages operations and publishes them
at once. Anything with memory, like a freeze that remembers the rate to come back to, is built on
top of this, in :mod:`libtimewarp.cli`.
"""

from __future__ import annotations

import ctypes
import enum
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from . import _c


class Code(enum.IntEnum):
    """Why a call did not do what it was asked. ``OK`` is success."""

    OK = 0
    NO_SESSION = 1
    BAD_SESSION_FILE = 2
    BAD_ARGUMENT = 3
    WOULD_GO_BACKWARDS = 4
    UNSUPPORTED_FOR_KEEPER = 5
    NOT_OURS = 6
    IO = 7


class Family(enum.IntEnum):
    """A logical grouping of clocks, and the atomic unit of configuration."""

    REALTIME = 0
    MONOTONIC = 1
    BOOTTIME = 2
    CPU = 3

    @property
    def label(self) -> str:
        """The name a user writes for this family."""
        return self.name.lower()

    @classmethod
    def named(cls, text: str) -> "Family":
        """The family a user's word names."""
        try:
            return cls[text.strip().upper()]
        except KeyError:
            known = ", ".join(family.label for family in cls)
            raise ValueError(f"no family called {text!r}; the families are {known}") from None


class Keeper(enum.IntEnum):
    """Which timekeeper computes the families' values."""

    AFFINE = 1
    NULL = 2

    @property
    def label(self) -> str:
        return self.name.lower()


class FaultCode(enum.IntEnum):
    """Why a session stopped faking."""

    BAD_VALUE = 1

    @property
    def label(self) -> str:
        return self.name.lower()


class Error(Exception):
    """A refused call: the stable code, and the message that came back beside it."""

    def __init__(self, code: int, message: str) -> None:
        super().__init__(message or f"error {code}")
        try:
            self.code = Code(code)
        except ValueError:
            # A code from a newer library than this tool. It is still an error.
            self.code = code
        self.message = message


@dataclass(frozen=True)
class Fault:
    """What went wrong with a session, and the process that hit it."""

    code: int
    pid: int

    @property
    def label(self) -> str:
        try:
            return FaultCode(self.code).label
        except ValueError:
            return f"code-{self.code}"


@dataclass(frozen=True)
class FamilyInfo:
    """One family's settings, as the session holds them now."""

    rate: float
    #: The genuine clock passage is measured against, or ``None`` for Cpu, which tracks none.
    timebase: Optional[int]


@dataclass(frozen=True)
class Info:
    """Everything a session reports about itself."""

    keeper: int
    hook: Optional[Path]
    creator_pid: int
    #: Genuine ``CLOCK_REALTIME`` when the session was created, in nanoseconds.
    creation_time: int
    emulate_vdso: bool
    fail_closed: bool
    families: dict
    fault: Optional[Fault]

    @property
    def keeper_label(self) -> str:
        try:
            return Keeper(self.keeper).label
        except ValueError:
            return f"keeper-{self.keeper}"


def _check(status: int) -> None:
    if status != Code.OK:
        raise Error(status, _c.last_error())


class Transaction:
    """A change in flight: the write lock is held and nothing is written until the commit.

    Used as a context manager, a clean exit commits and an exception aborts.
    """

    def __init__(self, handle: ctypes.c_void_p) -> None:
        self._handle: Optional[ctypes.c_void_p] = handle

    def _spend(self) -> ctypes.c_void_p:
        if self._handle is None:
            raise RuntimeError("this transaction has already been committed or aborted")
        handle, self._handle = self._handle, None
        return handle

    def _open(self) -> ctypes.c_void_p:
        if self._handle is None:
            raise RuntimeError("this transaction has already been committed or aborted")
        return self._handle

    def shift(self, family: Family, nanos: int) -> None:
        """Stages a move of the family by a duration."""
        _check(_c.library().libtimewarp_shift(self._open(), int(family), int(nanos)))

    def set_time(self, family: Family, nanos: int) -> None:
        """Stages a value for the family to show now."""
        _check(_c.library().libtimewarp_set_time(self._open(), int(family), int(nanos)))

    def set_rate(self, family: Family, rate: float) -> None:
        """Stages how fast the family runs. Zero freezes it."""
        _check(_c.library().libtimewarp_set_rate(self._open(), int(family), float(rate)))

    def commit(self) -> None:
        """Applies the staged operations against one anchor and publishes once."""
        _check(_c.library().libtimewarp_commit(self._spend()))

    def abort(self) -> None:
        """Discards the staged operations and releases the write lock."""
        _c.library().libtimewarp_abort(self._spend())

    def __enter__(self) -> "Transaction":
        return self

    def __exit__(self, kind, value, traceback) -> bool:
        if self._handle is None:
            return False
        if kind is None:
            self.commit()
        else:
            self.abort()
        return False


class Controller:
    """An attached session. Used as a context manager, leaving the block closes it."""

    def __init__(self, path: os.PathLike | str) -> None:
        self.path = Path(path)
        handle = ctypes.c_void_p()
        _check(
            _c.library().libtimewarp_attach(
                os.fsencode(self.path), ctypes.byref(handle)
            )
        )
        self._handle: Optional[ctypes.c_void_p] = handle

    def _open(self) -> ctypes.c_void_p:
        if self._handle is None:
            raise RuntimeError("this session has already been closed")
        return self._handle

    def close(self) -> None:
        """Releases the session handle. Closing twice is not an error."""
        if self._handle is not None:
            handle, self._handle = self._handle, None
            _c.library().libtimewarp_close(handle)

    def get(self, family: Family) -> int:
        """What the family shows now, in nanoseconds."""
        showing = ctypes.c_int64()
        _check(
            _c.library().libtimewarp_get(self._open(), int(family), ctypes.byref(showing))
        )
        return showing.value

    def info(self) -> Info:
        """How the session was set up, what each family is doing, and what went wrong with it."""
        raw = _c.Info()
        _check(_c.library().libtimewarp_info(self._open(), ctypes.byref(raw)))
        families = {
            family: FamilyInfo(
                rate=raw.rate[int(family)],
                timebase=None if family is Family.CPU else raw.timebase[int(family)],
            )
            for family in Family
        }
        return Info(
            keeper=raw.keeper,
            hook=Path(os.fsdecode(raw.hook_path)) if raw.hook_path else None,
            creator_pid=raw.creator_pid,
            creation_time=raw.creation_time,
            emulate_vdso=bool(raw.emulate_vdso),
            fail_closed=bool(raw.fail_closed),
            families=families,
            fault=Fault(raw.fault_code, raw.fault_pid) if raw.fault_code else None,
        )

    def transaction(self) -> Transaction:
        """Takes the write lock, and nothing else."""
        handle = ctypes.c_void_p()
        _check(_c.library().libtimewarp_begin(self._open(), ctypes.byref(handle)))
        return Transaction(handle)

    def __enter__(self) -> "Controller":
        return self

    def __exit__(self, kind, value, traceback) -> bool:
        self.close()
        return False

    def __del__(self) -> None:
        try:
            self.close()
        except Exception:  # pragma: no cover - an interpreter shutting down
            pass


def attach(path: os.PathLike | str) -> Controller:
    """Attaches by the path of the session file."""
    return Controller(path)
