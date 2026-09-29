import socket
import socketserver
import threading

ALLOWED = ('registry-1.docker.io', 'auth.docker.io',
           'production.cloudfront.docker.com', 'production.cloudflare.docker.com',
           'docker-images-prod.6aa30f8b08e16409b46e0173d6de2f56.r2.cloudflarestorage.com',
           'deb.debian.org', 'security.debian.org',
           'github.com', 'objects.githubusercontent.com',
           'release-assets.githubusercontent.com', 'passt.top')

class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        line = self.rfile.readline(4096).decode('ascii', 'replace').strip()
        if not line.startswith('CONNECT '):
            self.wfile.write(b'HTTP/1.1 405 Method Not Allowed\r\n\r\n')
            return
        target = line.split()[1]
        host, sep, port = target.rpartition(':')
        while self.rfile.readline(4096) not in (b'\r\n', b'\n', b''):
            pass
        if not sep or port != '443' or host not in ALLOWED:
            self.wfile.write(b'HTTP/1.1 403 Forbidden\r\n\r\n')
            return
        try:
            upstream = socket.create_connection((host, 443), timeout=15)
        except OSError:
            self.wfile.write(b'HTTP/1.1 502 Bad Gateway\r\n\r\n')
            return
        self.wfile.write(b'HTTP/1.1 200 Connection Established\r\n\r\n')
        self.wfile.flush()
        self.connection.settimeout(120)
        upstream.settimeout(120)
        def relay(source, destination):
            try:
                while data := source.recv(65536):
                    destination.sendall(data)
            except (ConnectionResetError, BrokenPipeError, OSError):
                pass
            finally:
                try:
                    destination.shutdown(socket.SHUT_WR)
                except OSError:
                    pass

        to_upstream = threading.Thread(target=relay, args=(self.connection, upstream))
        to_upstream.start()
        try:
            relay(upstream, self.connection)
        finally:
            to_upstream.join()
            upstream.close()

class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

with Server(('127.0.0.1', 18765), Handler) as srv:
    srv.serve_forever()
