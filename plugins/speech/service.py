#!/usr/bin/env python3
"""Long-lived Voice service for the OpenPal speech plugin (PLUGINS.md §4).

The framework runs this inside the plugin venv:

    <venv>/bin/python service.py --port <port> --data-dir <data>

(cwd = plugin dir, env OPENPAL_PLUGIN_DATA=<data>, kill_on_drop, stderr →
service.log). The framework then polls http://127.0.0.1:<port>/healthz until a
non-5xx response, marking the service Running.

Binds 127.0.0.1 ONLY. The actual STT/TTS work and the HTTP contract live in
`openpal_speech/` (see PROTOCOL.md). Models load lazily on first request so the
health check passes immediately.
"""

from __future__ import annotations

import argparse
import os
import signal
import sys

from openpal_speech.config import Config
from openpal_speech.server import serve


def _log(msg: str) -> None:
    print(f"[service] {msg}", file=sys.stderr, flush=True)


def _install_signal_handlers() -> None:
    """Exit promptly on SIGTERM/SIGINT (what kill_on_drop sends)."""

    def _handler(signum, _frame):  # noqa: ANN001
        _log(f"received signal {signum}; shutting down")
        os._exit(0)

    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            signal.signal(sig, _handler)
        except (ValueError, OSError):
            pass


def main(argv=None) -> int:
    if argv is None:
        argv = sys.argv[1:]
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--port", dest="port", type=int, required=True)
    parser.add_argument("--data-dir", dest="data_dir", default="")
    args, _unknown = parser.parse_known_args(argv)

    data_dir = args.data_dir or os.environ.get("OPENPAL_PLUGIN_DATA", "")
    if not data_dir:
        _log("FATAL: no --data-dir / OPENPAL_PLUGIN_DATA")
        return 2

    _install_signal_handlers()
    cfg = Config(data_dir=data_dir)
    try:
        serve(cfg, "127.0.0.1", int(args.port))
    except Exception as exc:  # pragma: no cover - exercised at runtime
        import traceback

        traceback.print_exc()
        _log(f"FATAL: service stopped: {exc}")
        return 6
    return 0


if __name__ == "__main__":
    sys.exit(main())
