#!/usr/bin/env python3
"""Long-lived SearXNG launcher for the OpenPal web-search plugin (PLUGINS.md §4).

The framework runs this inside the plugin venv:

    <venv>/bin/python service.py --port <port> --data-dir <data>

(cwd = plugin dir, env OPENPAL_PLUGIN_DATA=<data>, kill_on_drop, stderr →
service.log). The framework then polls http://127.0.0.1:<port>/healthz until a
non-5xx response, marking the service Running.

This script runs SearXNG *from source* (the pinned tarball that setup_plugin.py
downloaded into <data>/searxng-src) — SearXNG is NOT pip-installed as a package,
so we add searxng-src to sys.path and import searx.webapp directly.

Design notes:
  * Binds 127.0.0.1 ONLY (loopback). The port comes from the framework.
  * SEARXNG_SETTINGS_PATH points at <data>/settings.yml (generated at setup).
  * Host/port are passed straight to app.run(), which is authoritative — the
    generated settings.yml deliberately omits server.bind_address/port.
  * SIGTERM is handled so the framework's kill is clean.
  * All logs go to stderr.
"""

from __future__ import annotations

import argparse
import os
import signal
import sys
import threading


def _log(msg: str) -> None:
    print(f"[service] {msg}", file=sys.stderr, flush=True)


def _install_signal_handlers() -> None:
    """Exit cleanly on SIGTERM/SIGINT so the framework's kill is graceful.

    werkzeug's dev server installs its own SIGINT handling, but SIGTERM (what
    `kill_on_drop` / a normal kill sends) would otherwise just terminate the
    process abruptly. Translating it to a fast, clean exit keeps the log tidy.
    """

    def _handler(signum, _frame):  # noqa: ANN001
        _log(f"received signal {signum}; shutting down")
        # os._exit avoids re-entrancy issues with the serving thread / werkzeug.
        os._exit(0)

    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            signal.signal(sig, _handler)
        except (ValueError, OSError):
            # Not on the main thread, or platform doesn't support it; ignore.
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

    host = "127.0.0.1"
    port = int(args.port)

    searxng_src = os.path.join(data_dir, "searxng-src")
    settings_path = os.path.join(data_dir, "settings.yml")

    if not os.path.isfile(os.path.join(searxng_src, "searx", "webapp.py")):
        _log(f"FATAL: SearXNG source not found at {searxng_src} (run setup first)")
        return 3
    if not os.path.isfile(settings_path):
        _log(f"FATAL: settings.yml not found at {settings_path} (run setup first)")
        return 4

    # SearXNG reads its config from this env var. Set BEFORE importing searx.
    os.environ["SEARXNG_SETTINGS_PATH"] = settings_path

    # Run SearXNG from source: prepend its dir so `import searx.*` resolves.
    if searxng_src not in sys.path:
        sys.path.insert(0, searxng_src)

    _install_signal_handlers()

    _log(f"settings: {settings_path}")
    _log(f"searxng src: {searxng_src}")
    _log(f"starting SearXNG on http://{host}:{port} (loopback only)")

    # Import after env + sys.path are set. Importing searx.webapp runs its
    # module-level init() (engines/network/limiter), which can take a moment.
    try:
        from searx import webapp  # type: ignore
    except Exception as exc:  # pragma: no cover - exercised at runtime
        import traceback

        traceback.print_exc()
        _log(f"FATAL: failed to import SearXNG: {exc}")
        return 5

    app = webapp.app

    # werkzeug's dev server is fine for a local, single-user instance.
    # threaded=True so concurrent /healthz + /search don't block each other.
    # use_reloader=False is essential (a reloader would fork a second process).
    try:
        app.run(
            host=host,
            port=port,
            threaded=True,
            use_reloader=False,
            debug=False,
        )
    except Exception as exc:  # pragma: no cover - exercised at runtime
        import traceback

        traceback.print_exc()
        _log(f"FATAL: SearXNG server stopped: {exc}")
        return 6
    return 0


if __name__ == "__main__":
    sys.exit(main())
