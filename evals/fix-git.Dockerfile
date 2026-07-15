# syntax=docker/dockerfile:1.7
FROM ghcr.io/astral-sh/uv:0.9.5 AS uv

FROM python:3.13-slim-bookworm

WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends git \
    && rm -rf /var/lib/apt/lists/*

# This build context is Harbor's untouched task environment directory.
COPY setup.sh ./
COPY resources /app/resources
RUN bash /app/setup.sh

WORKDIR /app/personal-site
COPY --from=uv /uv /uvx /usr/local/bin/
RUN uv pip install --system --no-cache \
    pytest==8.4.1 \
    pytest-json-ctrf==0.3.5
