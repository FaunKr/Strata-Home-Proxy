"""Sets the runner's firewall and probes it; runs as root in its own network
namespace (unshare --net), so the host's rules stay untouched. Prints which
connections got through, as JSON."""

import fcntl
import json
import os
import pwd
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
api, api_port = listener()
runner.AGENT_USER = "nobody"
runner.RELAY_ADDR = f"127.0.0.1:{relay_port}"
runner.LISTEN = f"0.0.0.0:{api_port}"
problem = runner.set_firewall()

PROBE = (
    "import socket, sys\n"
    "try:\n"
    "    socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=2).close()\n"
    "    print('yes')\n"
    "except OSError:\n"
    "    print('no')\n"
)

# The agent's own server, as a task's tests start one: bound by the agent.
SERVE = (
    "import socket, sys\n"
    "s = socket.socket(); s.bind(('127.0.0.1', 0)); s.listen()\n"
    "port = s.getsockname()[1]\n"
    "try:\n"
    "    socket.create_connection(('127.0.0.1', port), timeout=2).close()\n"
    "    print('yes')\n"
    "except OSError:\n"
    "    print('no')\n"
)

# An address outside the container, as the internet or the NAS would be.
OUTSIDE = "192.0.2.1"
# A veth pair gives the namespace a route out; nothing answers on it.
for cmd in (
    ["ip", "link", "add", "probe0", "type", "veth", "peer", "name", "probe1"],
    ["ip", "addr", "add", "192.0.2.2/24", "dev", "probe0"],
    ["ip", "link", "set", "probe0", "up"],
    ["ip", "link", "set", "probe1", "up"],
):
    subprocess.run(cmd, check=True, capture_output=True)


def ids(user):
    # Like the runner's agent: its own user and group, no others.
    return {"user": user, "group": pwd.getpwnam(user).pw_gid, "extra_groups": []} if user else {}


def reaches(port, user=None, host="127.0.0.1"):
    out = subprocess.run([sys.executable, "-I", "-c", PROBE, host, str(port)], capture_output=True, text=True, **ids(user))
    return out.stdout.strip() == "yes"


def serves_itself(user):
    out = subprocess.run([sys.executable, "-I", "-c", SERVE], capture_output=True, text=True, **ids(user))
    return out.stdout.strip() == "yes"


def rejected_outside(user):
    """Whether a connection out of the container is refused at once by the
    firewall (nothing answers at OUTSIDE, so without it the attempt hangs)."""
    probe = (
        "import socket, sys\n"
        "try:\n"
        f"    socket.create_connection(('{OUTSIDE}', 80), timeout=2).close()\n"
        "    print('open')\n"
        "except ConnectionRefusedError:\n"
        "    print('refused')\n"
        "except OSError:\n"
        "    print('timeout')\n"
    )
    out = subprocess.run([sys.executable, "-I", "-c", probe], capture_output=True, text=True, **ids(user))
    return out.stdout.strip() == "refused"


print(
    json.dumps(
        {
            "problem": problem,
            "agent_relay": reaches(relay_port, "nobody"),
            "agent_other": reaches(other_port, "nobody"),
            "agent_api": reaches(api_port, "nobody"),
            "agent_own_server": serves_itself("nobody"),
            "agent_outside_rejected": rejected_outside("nobody"),
            "root_other": reaches(other_port),
        }
    )
)
