import functools
import http.server
import ssl
import sys
from pathlib import Path

root, certificate, key, request_log = sys.argv[1:]


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        with Path(request_log).open("a") as output:
            output.write(self.path + "\n")
        if (
            self.headers.get("Authorization")
            != "Basic cmV2aWV3LXVzZXI6cmV2aWV3LXBhc3N3b3Jk"
        ):
            self.send_error(401)
            return
        super().do_GET()


server = http.server.ThreadingHTTPServer(
    ("127.0.0.1", 0), functools.partial(Handler, directory=root)
)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(certificate, key)
server.socket = context.wrap_socket(server.socket, server_side=True)
print(server.server_port, flush=True)
server.serve_forever()
