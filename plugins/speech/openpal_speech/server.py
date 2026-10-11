"""The loopback HTTP service the Rust app drives (PROTOCOL.md).

A tiny stdlib `http.server` — no web framework — exposing:

    GET  /healthz      -> 200 {"status":"ok"}   (used by the framework's poll)
    POST /transcribe   body = WAV bytes          -> 200 {"text": "..."}     (STT)
    POST /speak        body = {"text","voice"?}   -> 200 audio/wav (WAV)     (TTS)

Models load lazily on first use, so /healthz is ready as soon as the socket is
bound. Errors return a JSON {"error": "..."} with a 4xx/5xx status; the Rust app
turns those into a user-safe sentence.
"""

from __future__ import annotations

import json
import sys
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from . import stt, tts, voices
from .config import Config

# Cap a single request body (a minute of 48 kHz 16-bit mono is ~5.5 MB; allow
# generous headroom but reject absurd bodies).
_MAX_BODY = 64 * 1024 * 1024


def _log(msg: str) -> None:
    print(f"[service] {msg}", file=sys.stderr, flush=True)


def make_handler(cfg: Config):
    class Handler(BaseHTTPRequestHandler):
        # Silence the default per-request stderr spam; we log what we care about.
        def log_message(self, *args):  # noqa: ANN001, D401
            return

        def _send(self, code: int, ctype: str, body: bytes) -> None:
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)

        def _json(self, code: int, obj: dict) -> None:
            self._send(code, "application/json", json.dumps(obj).encode("utf-8"))

        def _read_body(self) -> bytes:
            length = int(self.headers.get("Content-Length") or 0)
            if length <= 0:
                return b""
            if length > _MAX_BODY:
                raise ValueError("request body too large")
            return self.rfile.read(length)

        def do_GET(self):  # noqa: N802
            path = self.path.split("?", 1)[0]
            try:
                if path == "/healthz":
                    self._json(200, {"status": "ok"})
                elif path == "/voices":
                    self._json(200, {"voices": voices.list_voices(cfg)})
                else:
                    self._json(404, {"error": "not found"})
            except Exception as exc:  # one JSON error per request; never crash.
                traceback.print_exc()
                _log(f"{path} failed: {exc}")
                self._json(500, {"error": str(exc)})

        def do_POST(self):  # noqa: N802
            path = self.path.split("?", 1)[0]
            try:
                body = self._read_body()
                if path == "/transcribe":
                    text = stt.transcribe(cfg, body)
                    self._json(200, {"text": text})
                elif path == "/speak":
                    req = json.loads(body.decode("utf-8")) if body else {}
                    wav = tts.synthesize(
                        cfg,
                        req.get("text", ""),
                        req.get("voice"),
                        length_scale=req.get("length_scale"),
                        noise_scale=req.get("noise_scale"),
                        noise_w=req.get("noise_w"),
                        sentence_silence=req.get("sentence_silence"),
                    )
                    self._send(200, "audio/wav", wav)
                elif path == "/voices/download":
                    req = json.loads(body.decode("utf-8")) if body else {}
                    voices.download_voice(cfg, req.get("voice", ""))
                    self._json(200, {"voices": voices.list_voices(cfg)})
                else:
                    self._json(404, {"error": "not found"})
            except Exception as exc:  # one JSON error per request; never crash.
                traceback.print_exc()
                _log(f"{path} failed: {exc}")
                self._json(500, {"error": str(exc)})

    return Handler


def serve(cfg: Config, host: str, port: int) -> None:
    httpd = ThreadingHTTPServer((host, port), make_handler(cfg))
    _log(f"listening on http://{host}:{port} (loopback only)")
    _log(f"data dir: {cfg.data_dir}")
    try:
        httpd.serve_forever(poll_interval=0.5)
    finally:
        httpd.server_close()
