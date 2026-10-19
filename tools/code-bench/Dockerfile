# syntax=docker/dockerfile:1
# Der Code-Benchmark des Proxys: opencode löst in diesem Container Aufgaben
# gegen den OpenAI-Endpunkt des Proxys, Python prüft das Ergebnis. Der Agent
# darf hier Befehle ausführen; der Container hält nichts außer dem Runner.
FROM node:22-bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 python-is-python3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Feste Version: die JSON-Ereignisse von `opencode run` sind kein stabiles Format.
ARG OPENCODE_VERSION=1.19.3
RUN npm install -g "opencode-ai@${OPENCODE_VERSION}" && npm cache clean --force \
    && opencode --version
RUN useradd --create-home bench
COPY runner.py /app/runner.py
USER bench
WORKDIR /home/bench
ENV LISTEN_ADDR=0.0.0.0:8096 \
    PROXY_URL=http://host.docker.internal:8080 \
    OPENCODE_DISABLE_MODELS_FETCH=1 \
    OPENCODE_DISABLE_AUTOUPDATE=1
EXPOSE 8096
CMD ["python3", "/app/runner.py"]
