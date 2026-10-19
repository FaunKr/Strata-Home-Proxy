"""Sets the runner's firewall and probes it; runs as root in its own network
namespace (unshare --net), so the host's rules stay untouched. Prints which
connections got through, as JSON."""

import fcntl
import json
import os
import socket
import struct
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import runner  # noqa: E402

# A new namespace has loopback down; SIOCSIFFLAGS with IFF_UP brings it up.
with socket.socket() as s:
    fcntl.ioctl(s, 0x8914, struct.pack("16sH14x", b"lo", 0x1))


def listener():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    s.listen()
    return s, s.getsockname()[1]


relay, relay_port = listener()
other, other_port = listener()
runner.AGENT_USER = "nobody"
runner.RELAY_ADDR = f"127.0.0.1:{relay_port}"
problem = runner.set_firewall()

PROBE = (
    "import socket, sys\n"
    "try:\n"
    "    socket.create_connection(('127.0.0.1', int(sys.argv[1])), timeout=2).close()\n"
    "    print('yes')\n"
    "except OSError:\n"
    "    print('no')\n"
)


def reaches(port, user=None):
    out = subprocess.run([sys.executable, "-I", "-c", PROBE, str(port)], user=user, capture_output=True, text=True)
    return out.stdout.strip() == "yes"


print(
    json.dumps(
        {
            "problem": problem,
            "agent_relay": reaches(relay_port, "nobody"),
            "agent_other": reaches(other_port, "nobody"),
            "root_other": reaches(other_port),
        }
    )
)
