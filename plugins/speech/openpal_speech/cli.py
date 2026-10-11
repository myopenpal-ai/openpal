"""A tiny CLI for manually exercising the plugin during development:

    python -m openpal_speech speak "hello there" out.wav --data-dir <data>
    python -m openpal_speech transcribe in.wav --data-dir <data>

Not used by the app (which talks to service.py over HTTP) — purely a dev aid.
"""

from __future__ import annotations

import argparse
import sys

from .config import config_from_env
from . import stt, tts


def main(argv=None) -> int:
    if argv is None:
        argv = sys.argv[1:]
    parser = argparse.ArgumentParser(prog="openpal_speech")
    parser.add_argument("--data-dir", dest="data_dir", default=None)
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_speak = sub.add_parser("speak", help="synthesize text to a WAV file")
    p_speak.add_argument("text")
    p_speak.add_argument("out", help="output .wav path")
    p_speak.add_argument("--voice", default=None)
    # Optional Piper synthesis controls; omitted ones keep Piper's defaults.
    p_speak.add_argument("--length-scale", type=float, default=None)
    p_speak.add_argument("--noise-scale", type=float, default=None)
    p_speak.add_argument("--noise-w", type=float, default=None)
    p_speak.add_argument("--sentence-silence", type=float, default=None)

    p_tr = sub.add_parser("transcribe", help="transcribe a WAV file to text")
    p_tr.add_argument("wav", help="input .wav path")
    p_tr.add_argument("--lang", default=None)

    args = parser.parse_args(argv)
    cfg = config_from_env(args.data_dir)

    if args.cmd == "speak":
        data = tts.synthesize(
            cfg,
            args.text,
            args.voice,
            length_scale=args.length_scale,
            noise_scale=args.noise_scale,
            noise_w=args.noise_w,
            sentence_silence=args.sentence_silence,
        )
        with open(args.out, "wb") as fh:
            fh.write(data)
        print(f"wrote {len(data)} bytes to {args.out}")
        return 0
    if args.cmd == "transcribe":
        with open(args.wav, "rb") as fh:
            wav = fh.read()
        print(stt.transcribe(cfg, wav, language=args.lang))
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
