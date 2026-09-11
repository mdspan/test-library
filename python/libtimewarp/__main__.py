"""``python -m libtimewarp``: the same entry point the installed command runs."""

from .cli import main

raise SystemExit(main())
