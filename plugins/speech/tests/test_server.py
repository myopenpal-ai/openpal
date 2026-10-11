"""Integration test for the HTTP service, with STT/TTS stubbed out so no models
or binaries are needed. Verifies the routing + wire shapes in PROTOCOL.md."""

from __future__ import annotations

import json
import socket
import threading
import urllib.request
import urllib.error
from http.server import ThreadingHTTPServer

import pytest

from openpal_speech import server as server_mod
from openpal_speech.config import Config


@pytest.fixture()
def service(monkeypatch):
    # Stub the heavy work: STT echoes a fixed string, TTS returns marker bytes.
    monkeypatch.setattr(server_mod.stt, "transcribe", lambda cfg, body: "hello world")
    monkeypatch.setattr(
        server_mod.tts,
        "synthesize",
        lambda cfg, text, voice=None, **_kwargs: b"RIFFwav:" + text.encode(),
    )

    cfg = Config(data_dir="/tmp/speech-test")
    # Bind an ephemeral loopback port.
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()

    httpd = ThreadingHTTPServer(("127.0.0.1", port), server_mod.make_handler(cfg))
    t = threading.Thread(target=httpd.serve_forever, daemon=True)
    t.start()
    try:
        yield port
    finally:
        httpd.shutdown()
        httpd.server_close()


def _get(port, path):
    with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=5) as r:
        return r.status, r.read()


def _post(port, path, data, ctype):
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=data,
        headers={"Content-Type": ctype},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5) as r:
        return r.status, r.headers.get("Content-Type"), r.read()


def test_healthz(service):
    status, body = _get(service, "/healthz")
    assert status == 200
    assert json.loads(body)["status"] == "ok"


def test_transcribe(service):
    status, ctype, body = _post(service, "/transcribe", b"fakewavbytes", "audio/wav")
    assert status == 200
    assert json.loads(body)["text"] == "hello world"


def test_speak_returns_wav(service):
    payload = json.dumps({"text": "hi"}).encode()
    status, ctype, body = _post(service, "/speak", payload, "application/json")
    assert status == 200
    assert ctype == "audio/wav"
    assert body == b"RIFFwav:hi"


def test_unknown_route_404(service):
    with pytest.raises(urllib.error.HTTPError) as exc:
        _get(service, "/nope")
    assert exc.value.code == 404
