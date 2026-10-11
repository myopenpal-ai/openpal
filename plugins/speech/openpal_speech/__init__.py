"""OpenPal Voice plugin — local text-to-speech (Piper) and speech-to-text
(faster-whisper), exposed over a tiny loopback HTTP service the Rust app drives.

See PROTOCOL.md for the HTTP contract and PLUGINS.md (§3/§4/§7) for how the
framework sets the plugin up and runs its service.
"""

__all__ = ["config", "stt", "tts"]
