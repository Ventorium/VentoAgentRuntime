# SPDX-License-Identifier: MIT
FROM debian:bookworm-slim

RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      bash ca-certificates coreutils curl fd-find findutils git gzip jq nodejs npm \
      python3 ripgrep tar wget zstd \
 && ln -s /usr/bin/fdfind /usr/local/bin/fd \
 && rm -rf /var/lib/apt/lists/*

COPY agentd /agentd
RUN chmod 0755 /agentd && mkdir -p /workspace /knowledge

ENTRYPOINT ["/agentd"]

