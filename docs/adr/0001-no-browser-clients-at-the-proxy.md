# No browser clients at the proxy API

The proxy's clients are programs like Claude Code and Open WebUI's server,
not web pages in the browser. That is why there is no CORS: even a preflight
(`OPTIONS`) without a valid API key gets `401` and does not wake the PC. The
web UI under `/admin` is on the same origin and needs no
CORS.

Whoever reports this as a bug in review: decided. Change it only when a client
is added that really runs in the browser.
