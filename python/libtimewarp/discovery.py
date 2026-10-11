"""Finding the session to act on.

Session names are generated, so there is no name to type. Two ways in, and the tool takes them in
this order: the path in ``LIBTIMEWARP__SHARED_FILE`` in the environment of any process of the
faked tree, or the session directory, whose files are told apart by the creator pid and creation
time their headers carry.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Optional

from .controller import Controller, Error, Fault, Info, Keeper

#: The internal variable a faked process carries the path of its session file in.
SHARED_FILE = "LIBTIMEWARP__SHARED_FILE"

#: What a finished session file is called.
SESSION_SUFFIX = ".session"

#: The variable naming the parent of the session directory.
SESSION_DIR = "LIBTIMEWARP_SESSION_DIR"

#: The variable naming the configuration file.
CONFIG = "LIBTIMEWARP_CONFIG"

#: The user's configuration file, under ``$XDG_CONFIG_HOME``, and the system-wide one.
USER_FILE = "libtimewarp/config.toml"
SYSTEM_FILE = "/etc/libtimewarp/config.toml"


def config_file_session_dir(env: Mapping[str, str] = os.environ) -> Optional[str]:
    """The ``session_dir`` of the library's configuration file, or ``None``.

    The file is found the way the library finds it. A file that is not TOML, or whose
    ``session_dir`` is not a string, sets nothing here; the library refuses the whole file then.
    """
    named = env.get(CONFIG)
    if named:
        candidates = [Path(named)]
    else:
        base = env.get("XDG_CONFIG_HOME") or (
            os.path.join(env["HOME"], ".config") if env.get("HOME") else None
        )
        candidates = ([Path(base) / USER_FILE] if base else []) + [Path(SYSTEM_FILE)]
    for path in candidates:
        try:
            with open(path, "rb") as file:
                document = tomllib.load(file)
        except FileNotFoundError:
            if named:
                return None
            continue
        except (OSError, tomllib.TOMLDecodeError):
            return None
        found = document.get("session_dir")
        return found if isinstance(found, str) else None
    return None


def session_dir(env: Mapping[str, str] = os.environ, euid: Optional[int] = None) -> Path:
    """The directory holding session files: the first rule that applies.

    The same three rules the library follows, so both sides compute the same path. The session
    directory variable is taken from the configuration file when the environment does not set it.
    """
    named = env[SESSION_DIR] if SESSION_DIR in env else config_file_session_dir(env)
    if named:
        return Path(named) / "libtimewarp"
    runtime = env.get("XDG_RUNTIME_DIR")
    if runtime:
        return Path(runtime) / "libtimewarp"
    tmp = env["TMPDIR"] if "TMPDIR" in env else "/tmp"
    return Path(tmp) / f"libtimewarp-{os.geteuid() if euid is None else euid}"


def _environ_blob(pid: int) -> bytes:
    """The environment of a process of ours, however this platform gives it up."""
    if sys.platform.startswith("linux"):
        return Path(f"/proc/{pid}/environ").read_bytes()
    if sys.platform.startswith("freebsd"):
        procfs = Path(f"/proc/{pid}/env")
        if procfs.exists():
            return procfs.read_bytes()
        # No procfs mounted, which is the default there, so ask the tool that reads the kernel's
        # copy directly.
        return subprocess.run(
            ["procstat", "-h", "env", str(pid)],
            check=True,
            capture_output=True,
        ).stdout
    if sys.platform == "darwin":
        # There is no /proc; ps prints the environment of a process of ours after its command.
        return subprocess.run(
            ["ps", "-Ewww", "-o", "command=", "-p", str(pid)],
            check=True,
            capture_output=True,
        ).stdout
    raise LookupError(f"reading another process's environment is not implemented on {sys.platform}")


def shared_file_of(pid: int) -> Path:
    """The session file a process of the faked tree carries the path of.

    On Linux the environment is read whole and split on NULs, which is exact. Where it arrives as a
    line of text instead, the value is taken up to the first space, so a path with a space in it is
    the one case this cannot answer; name the session with ``--session`` there.
    """
    try:
        blob = _environ_blob(pid)
    except (OSError, subprocess.CalledProcessError) as why:
        raise LookupError(f"cannot read the environment of pid {pid}: {why}") from why

    prefix = SHARED_FILE.encode() + b"="
    for entry in blob.split(b"\0"):
        if entry.startswith(prefix):
            return Path(os.fsdecode(entry[len(prefix) :]))
    found = re.search(prefix + rb"(\S+)", blob)
    if found:
        return Path(os.fsdecode(found.group(1)))
    raise LookupError(f"pid {pid} carries no {SHARED_FILE}, so it is in no session of ours")


@dataclass(frozen=True)
class Found:
    """A session file in the directory, and enough of its header to tell it from its neighbours."""

    path: Path
    creator_pid: int
    creation_time: int
    keeper: int
    fault: Optional[Fault]

    @classmethod
    def of(cls, path: Path, info: Info) -> "Found":
        return cls(
            path=path,
            creator_pid=info.creator_pid,
            creation_time=info.creation_time,
            keeper=info.keeper,
            fault=info.fault,
        )

    @property
    def keeper_label(self) -> str:
        try:
            return Keeper(self.keeper).label
        except ValueError:
            return f"keeper-{self.keeper}"


def sessions(directory: Path) -> list[Found]:
    """Every session in the directory, newest first.

    Creation is atomic, so every file there is whole; one that will not open is left out rather
    than reported, because it is not a session this tool can act on.
    """
    try:
        entries = sorted(directory.iterdir())
    except OSError:
        return []
    found = []
    for path in entries:
        if not path.name.endswith(SESSION_SUFFIX):
            continue
        try:
            with Controller(path) as session:
                found.append(Found.of(path, session.info()))
        except Error:
            continue
    found.sort(key=lambda one: one.creation_time, reverse=True)
    return found


def select(
    directory: Path,
    creator_pid: Optional[int] = None,
    newest: bool = False,
) -> Found:
    """The one session a command should act on, out of the directory.

    With one session there it is that one. With several, the creator pid or the creation time
    picks, and without either the caller is told what is there rather than guessed at.
    """
    found = sessions(directory)
    if creator_pid is not None:
        found = [one for one in found if one.creator_pid == creator_pid]
    if not found:
        raise LookupError(f"no session in {directory}")
    if len(found) == 1 or newest:
        return found[0]
    listed = "\n".join(
        f"  {one.path} (creator pid {one.creator_pid}, created {one.creation_time} ns)"
        for one in found
    )
    raise LookupError(
        f"{len(found)} sessions in {directory}; name one with --session, --creator or "
        f"--newest:\n{listed}"
    )
