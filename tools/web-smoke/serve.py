"""Serve a built web app with the two headers threaded WebAssembly needs.

Not part of the product. `flutter build web` produces static files and the app
is dead in the water without `Cross-Origin-Opener-Policy` and
`Cross-Origin-Embedder-Policy` (ADR 0002), so anything that runs the build —
`just web-smoke`, or a person looking at it — needs a server that sets them.
"""

import functools
import http.server
import sys


class Handler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        # So a reload in the smoke test reloads the app and not a cached copy of
        # the one before the change being tested.
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    directory, port = sys.argv[1], int(sys.argv[2])
    http.server.ThreadingHTTPServer(
        ("127.0.0.1", port),
        functools.partial(Handler, directory=directory),
    ).serve_forever()
