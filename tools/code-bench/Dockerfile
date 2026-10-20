# syntax=docker/dockerfile:1
# The proxy's code benchmark: in this container opencode solves tasks against
# the proxy's OpenAI endpoint, Python checks the result. The coding agent may
# run commands here; the container holds nothing but the runner.
FROM node:22-bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 python-is-python3 ca-certificates iptables \
    && rm -rf /var/lib/apt/lists/*
# A fixed version: the JSON events of `opencode run` are not a stable format.
ARG OPENCODE_VERSION=1.19.3
RUN npm install -g "opencode-ai@${OPENCODE_VERSION}" && npm cache clean --force \
    && opencode --version
# The runner runs as root to set up the coding agent's firewall and to start
# it as a user of its own; opencode and the tests run as `agent`.

RUN useradd --create-home agent
COPY runner.py /app/runner.py
WORKDIR /app
ENV LISTEN_ADDR=0.0.0.0:8096 \
    AGENT_USER=agent \
    PROXY_URL=http://host.docker.internal:8080 \
    OPENCODE_DISABLE_MODELS_FETCH=1 \
    OPENCODE_DISABLE_AUTOUPDATE=1
EXPOSE 8096
CMD ["python3", "/app/runner.py"]
