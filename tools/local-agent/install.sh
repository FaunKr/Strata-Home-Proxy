#!/usr/bin/env bash
# Installs local-agent to ~/.local/bin (or $PREFIX) and creates its
# configuration file, readable only by you, if there is none yet.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
prefix=${PREFIX:-$HOME/.local/bin}
config=${XDG_CONFIG_HOME:-$HOME/.config}/local-agent/env

for tool in git jq curl claude; do
  command -v "$tool" >/dev/null || echo "Warning: $tool not found; local-agent needs it." >&2
done

mkdir -p "$prefix"
cp "$here/local-agent" "$prefix/local-agent"
chmod 755 "$prefix/local-agent"
echo "Installed $prefix/local-agent"
case ":$PATH:" in
  *":$prefix:"*) ;;
  *) echo "Add $prefix to your PATH." ;;
esac

if [[ ! -f $config ]]; then
  mkdir -p "$(dirname "$config")"
  (
    umask 077
    cat >"$config" <<'CONF'
# local-agent: the Proxy on the NAS and a client key from Access in its web UI.
LOCAL_AGENT_URL=
LOCAL_AGENT_KEY=
CONF
  )
  echo "Fill in $config, then run: local-agent check"
fi
