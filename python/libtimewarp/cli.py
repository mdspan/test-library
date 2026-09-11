"""The command-line tool over the C library.

Everything here is one level above the library: the library offers primitives, and the memory a
freeze needs, the rate to come back to, is kept here, in a file of the tool's own.

Output is ``key=value`` lines, one fact each, so the tool reads as well from a script as from a
terminal.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path
from typing import Optional, Sequence

from . import discovery, values
from .controller import Controller, Error, Family, Info

#: Where the tool keeps what it has to remember between runs.
STATE_FILE = "frozen.json"

#: argparse reads any token starting with a dash as an option, and `-2h` is exactly the spelling
#: the library's own variables use for a duration. This widens the pattern argparse uses to tell a
#: negative number from an option so that a signed duration or rate passes through as a value. None
#: of the tool's own options can match it. An argparse that has no such pattern leaves `--` as the
#: way to write one, which the usage line then says.
_SIGNED_VALUE = re.compile(r"^[+-]\d+(\.\d+)?(ns|us|ms|s|m|h|d)?$")


def _takes_a_signed_value(parser: argparse.ArgumentParser) -> argparse.ArgumentParser:
    if hasattr(parser, "_negative_number_matcher"):
        parser._negative_number_matcher = _SIGNED_VALUE
    return parser


def state_dir() -> Path:
    """The tool's own directory, which is not the library's: the library writes sessions there and
    nothing else."""
    named = os.environ.get("LIBTIMEWARP_STATE_DIR")
    if named:
        return Path(named)
    base = os.environ.get("XDG_STATE_HOME") or os.path.join(os.path.expanduser("~"), ".local/state")
    return Path(base) / "libtimewarp"


def _remembered() -> dict:
    try:
        with open(state_dir() / STATE_FILE, "r", encoding="utf-8") as file:
            return json.load(file)
    except (OSError, ValueError):
        return {}


def _remember(state: dict) -> None:
    directory = state_dir()
    directory.mkdir(parents=True, exist_ok=True)
    with open(directory / STATE_FILE, "w", encoding="utf-8") as file:
        json.dump(state, file, indent=1, sort_keys=True)


def say(key: str, value: object) -> None:
    """One fact, on its own line."""
    print(f"{key}={value}")


def _directory(args: argparse.Namespace) -> Path:
    return Path(args.dir) if args.dir else discovery.session_dir()


def _chosen(args: argparse.Namespace) -> Path:
    """The session file a command acts on."""
    if args.session:
        return Path(args.session)
    if args.pid is not None:
        return discovery.shared_file_of(args.pid)
    return discovery.select(_directory(args), args.creator, args.newest).path


def _reading(family: Family, nanos: int) -> None:
    say(f"{family.label}.ns", nanos)
    if family is Family.REALTIME:
        say(f"{family.label}.iso", values.format_instant(nanos))
    else:
        say(f"{family.label}.elapsed", values.format_duration(nanos))


def _report(path: Path, info: Info) -> None:
    say("session", path)
    say("keeper", info.keeper_label)
    say("hook", info.hook or "")
    say("creator-pid", info.creator_pid)
    say("created", values.format_instant(info.creation_time))
    say("created-ns", info.creation_time)
    say("emulate-vdso", int(info.emulate_vdso))
    say("fail-closed", int(info.fail_closed))
    say("fault", info.fault.label if info.fault else "")
    say("fault-pid", info.fault.pid if info.fault else "")
    for family in Family:
        line = info.families[family]
        say(f"{family.label}.rate", f"{line.rate:.9f}")
        say(f"{family.label}.timebase", values.clock_name(line.timebase))


def command_list(args: argparse.Namespace) -> int:
    directory = _directory(args)
    say("directory", directory)
    found = discovery.sessions(directory)
    for index, one in enumerate(found):
        say(f"session.{index}.path", one.path)
        say(f"session.{index}.creator-pid", one.creator_pid)
        say(f"session.{index}.created", values.format_instant(one.creation_time))
        say(f"session.{index}.created-ns", one.creation_time)
        say(f"session.{index}.keeper", one.keeper_label)
        say(f"session.{index}.fault", one.fault.label if one.fault else "")
    say("count", len(found))
    return 0


def command_info(args: argparse.Namespace) -> int:
    path = _chosen(args)
    with Controller(path) as session:
        _report(path, session.info())
    return 0


def command_get(args: argparse.Namespace) -> int:
    family = Family.named(args.family)
    with Controller(_chosen(args)) as session:
        _reading(family, session.get(family))
    return 0


def command_shift(args: argparse.Namespace) -> int:
    family = Family.named(args.family)
    nanos = values.parse_duration(args.by)
    with Controller(_chosen(args)) as session:
        with session.transaction() as change:
            change.shift(family, nanos)
        _reading(family, session.get(family))
    return 0


def command_set_time(args: argparse.Namespace) -> int:
    family = Family.named(args.family)
    nanos = values.parse_instant(args.to)
    with Controller(_chosen(args)) as session:
        with session.transaction() as change:
            change.set_time(family, nanos)
        _reading(family, session.get(family))
    return 0


def command_set_rate(args: argparse.Namespace) -> int:
    family = Family.named(args.family)
    rate = values.parse_rate(args.rate)
    with Controller(_chosen(args)) as session:
        with session.transaction() as change:
            change.set_rate(family, rate)
        say(f"{family.label}.rate", f"{session.info().families[family].rate:.9f}")
    return 0


def _families(names: Sequence[str]) -> list[Family]:
    if not names:
        return [Family.REALTIME, Family.MONOTONIC, Family.BOOTTIME]
    return [Family.named(name) for name in names]


def command_freeze(args: argparse.Namespace) -> int:
    """Remembers each family's rate and sets it to zero, in one transaction.

    The library has no memory of its own, by design, so the rate to come back to is written here.
    """
    path = _chosen(args)
    wanted = _families(args.families)
    with Controller(path) as session:
        info = session.info()
        state = _remembered()
        kept = state.setdefault(str(path), {})
        with session.transaction() as change:
            for family in wanted:
                rate = info.families[family].rate
                # A family frozen twice keeps the rate of the first freeze, so a second one is not
                # a way to lose the rate to come back to.
                kept.setdefault(family.label, rate)
                change.set_rate(family, 0.0)
        _remember(state)
        for family in wanted:
            say(f"{family.label}.rate", "0.000000000")
            say(f"{family.label}.remembered", f"{kept[family.label]:.9f}")
    return 0


def command_thaw(args: argparse.Namespace) -> int:
    """Puts back the rate the freeze remembered, in one transaction."""
    path = _chosen(args)
    wanted = _families(args.families)
    state = _remembered()
    kept = state.get(str(path), {})
    missing = [family.label for family in wanted if family.label not in kept]
    if missing:
        raise LookupError(
            f"no remembered rate for {', '.join(missing)} of {path}; set the rate outright with "
            "set-rate"
        )
    with Controller(path) as session:
        with session.transaction() as change:
            for family in wanted:
                change.set_rate(family, kept[family.label])
        for family in wanted:
            kept.pop(family.label, None)
        if not kept:
            state.pop(str(path), None)
        _remember(state)
        info = session.info()
        for family in wanted:
            say(f"{family.label}.rate", f"{info.families[family].rate:.9f}")
    return 0


def parser() -> argparse.ArgumentParser:
    """The tool's arguments, and the one-line description of each command."""
    root = argparse.ArgumentParser(
        prog="libtimewarp",
        description="Read and change a live libtimewarp session.",
    )
    picking = root.add_argument_group("picking the session")
    picking.add_argument("--session", help="the path of the session file, outright")
    picking.add_argument(
        "--pid",
        type=int,
        help="take the path from the environment of this process of the faked tree",
    )
    picking.add_argument(
        "--creator",
        type=int,
        help="among the sessions in the directory, the one this process created",
    )
    picking.add_argument(
        "--newest",
        action="store_true",
        help="among the sessions in the directory, the one created last",
    )
    picking.add_argument(
        "--dir",
        help="the directory holding session files, in place of the one the naming rule computes",
    )

    commands = root.add_subparsers(dest="command", required=True)
    commands.add_parser("list", help="every session in the directory").set_defaults(
        run=command_list
    )
    commands.add_parser(
        "info", help="how the session was set up and what went wrong with it"
    ).set_defaults(run=command_info)

    get = commands.add_parser("get", help="what a family shows now")
    get.add_argument("family")
    get.set_defaults(run=command_get)

    shift = _takes_a_signed_value(commands.add_parser("shift", help="move a family by a duration"))
    shift.add_argument("family")
    shift.add_argument("by", help="a duration such as -2h, 90s or 500ms")
    shift.set_defaults(run=command_shift)

    set_time = _takes_a_signed_value(
        commands.add_parser("set-time", help="make a family show a value now")
    )
    set_time.add_argument("family")
    set_time.add_argument(
        "to", help="2020-01-01T00:00:00Z, @1577836800, or a duration for a monotonic family"
    )
    set_time.set_defaults(run=command_set_time)

    set_rate = commands.add_parser("set-rate", help="change how fast a family runs")
    set_rate.add_argument("family")
    set_rate.add_argument("rate", help="fake seconds per second; 0 freezes")
    set_rate.set_defaults(run=command_set_rate)

    freeze = commands.add_parser(
        "freeze", help="remember each family's rate and set it to zero"
    )
    freeze.add_argument("families", nargs="*")
    freeze.set_defaults(run=command_freeze)

    thaw = commands.add_parser("thaw", help="put back the rate the freeze remembered")
    thaw.add_argument("families", nargs="*")
    thaw.set_defaults(run=command_thaw)
    return root


def main(argv: Optional[Sequence[str]] = None) -> int:
    """Runs one command. A refusal goes to stderr and leaves a nonzero status behind."""
    args = parser().parse_args(argv)
    try:
        return args.run(args)
    except Error as refused:
        name = refused.code.name.lower() if hasattr(refused.code, "name") else refused.code
        print(f"libtimewarp: {name}: {refused.message}", file=sys.stderr)
        return 1
    except (LookupError, ValueError, OSError) as why:
        print(f"libtimewarp: {why}", file=sys.stderr)
        return 1


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
