# syntax=docker/dockerfile:1
# The proxy's code benchmark: in this container opencode solves tasks against
# the proxy's OpenAI endpoint, Python checks the result. The coding agent may
# run commands here; the container holds nothing but the runner.

# The repo task (tasks/pc-force-endpoints): this repo before a real feature,
# its crates vendored and its tests prebuilt, so a run needs no internet.
# prepare_repo_task.py also checks that the feature's own change scores full
# marks and the untouched checkout does not; otherwise the build fails.
FROM rust:1.97-bookworm AS repo-task
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
ARG REPO_URL=https://github.com/FaunKr/Strata-Home-Proxy.git
RUN git clone --quiet --filter=blob:none "$REPO_URL" /src
COPY runner.py prepare_repo_task.py /build/
COPY tasks/pc-force-endpoints/repo.json /build/tasks/pc-force-endpoints/
RUN python3 /build/prepare_repo_task.py /build/tasks/pc-force-endpoints /src /opt/code-bench \
    && cp -a "$(rustc --print sysroot)" /opt/rust

FROM node:22-bookworm-slim
# git for the repo task's diff, gcc as the linker for Rust.
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 python-is-python3 ca-certificates iptables \
        git gcc libc6-dev \
    && rm -rf /var/lib/apt/lists/*
COPY --from=repo-task /opt/rust /opt/rust
COPY --from=repo-task /opt/code-bench /opt/code-bench
ENV PATH=/opt/rust/bin:$PATH
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
