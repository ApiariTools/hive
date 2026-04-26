#!/usr/bin/env python3
"""Minimal Kokoro TTS server for hive."""
import io
import json
from http.server import HTTPServer, BaseHTTPRequestHandler
import kokoro_onnx
import soundfile as sf

# Load model once at startup
kokoro = kokoro_onnx.Kokoro("kokoro-v1.0.onnx", "voices-v1.0.bin")


class TTSHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path != "/tts":
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length))
        text = body.get("text", "")
        voice = body.get("voice", "af_heart")
        if not text:
            self.send_error(400, "Missing text")
            return

        samples, sample_rate = kokoro.create(text, voice=voice, speed=1.0)

        buf = io.BytesIO()
        sf.write(buf, samples, sample_rate, format="WAV")
        wav_data = buf.getvalue()

        self.send_response(200)
        self.send_header("Content-Type", "audio/wav")
        self.send_header("Content-Length", str(len(wav_data)))
        self.end_headers()
        self.wfile.write(wav_data)

    def log_message(self, format, *args):
        pass  # Suppress request logging


if __name__ == "__main__":
    print("TTS server starting on :4201")
    HTTPServer(("127.0.0.1", 4201), TTSHandler).serve_forever()
