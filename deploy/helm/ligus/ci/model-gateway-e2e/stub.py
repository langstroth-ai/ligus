# A tiny OpenAI/Anthropic-compatible stub model for the Ligus model-gateway
# e2e. It never calls a real provider. It logs every request's headers; a
# bearer JWT is logged as its claims (never the token), any other credential
# as a SHA-256 prefix. STUB_API_KEY, when set, is required as a bearer key.
import base64, hashlib, json, os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SEEN = []
KEY = os.environ.get("STUB_API_KEY", "")

def summarize(name, value):
    n = name.lower()
    if n in ("authorization", "x-api-key", "api-key"):
        v = value.split(" ", 1)[1] if value.lower().startswith("bearer ") else value
        parts = v.split(".")
        if len(parts) == 3:
            try:
                pad = parts[1] + "=" * (-len(parts[1]) % 4)
                c = json.loads(base64.urlsafe_b64decode(pad))
                return {"jwt": {k: c.get(k) for k in ("iss", "aud", "sub", "exp")}}
            except Exception:
                pass
        return {"sha256": hashlib.sha256(v.encode()).hexdigest()[:12]}
    return value

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a):
        pass
    def _send(self, code, body, ctype="application/json"):
        b = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)
    def do_GET(self):
        if self.path == "/seen":
            return self._send(200, json.dumps(SEEN))
        if self.path in ("/healthz", "/v1/models"):
            return self._send(200, json.dumps({"data": [{"id": "stub"}]}))
        self._send(404, "{}")
    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(n) if n else b""
        rec = {"path": self.path, "headers": {k.lower(): summarize(k, v) for k, v in self.headers.items()}}
        SEEN.append(rec)
        print(json.dumps(rec), flush=True)
        if KEY:
            got = self.headers.get("authorization", "")
            if got != "Bearer " + KEY:
                return self._send(401, json.dumps({"error": {"message": "stub: bad key"}}))
        try:
            req = json.loads(raw or b"{}")
        except Exception:
            req = {}
        text = "pong from the stub model"
        if self.path.endswith("/messages"):
            if req.get("stream"):
                ev = [
                    ("message_start", {"type": "message_start", "message": {"id": "m", "role": "assistant", "content": []}}),
                    ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
                    ("message_stop", {"type": "message_stop"}),
                ]
                return self._send(200, "".join(f"event: {e}\ndata: {json.dumps(d)}\n\n" for e, d in ev), "text/event-stream")
            return self._send(200, json.dumps({"id": "m", "type": "message", "role": "assistant", "content": [{"type": "text", "text": text}]}))
        if req.get("stream"):
            chunk = {"choices": [{"index": 0, "delta": {"content": text}}]}
            return self._send(200, f"data: {json.dumps(chunk)}\n\ndata: [DONE]\n\n", "text/event-stream")
        return self._send(200, json.dumps({"id": "c", "object": "chat.completion", "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}]}))

ThreadingHTTPServer(("0.0.0.0", int(os.environ.get("PORT", "8080"))), H).serve_forever()
