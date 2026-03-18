# Stage 1: Build
FROM rust:1.87-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends libdbus-1-dev pkg-config && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
RUN cargo build --release --features k8s

# Stage 2: Model download
# fastembed downloads models from HuggingFace into the cache directory on first
# use. We pre-download by running "warmup" in this stage so the runtime image
# ships with the model already on disk — no internet access required at pod
# startup, and cold-start latency is eliminated.
#
# We run as a dedicated non-root user (app, uid 1000) to avoid writing model
# files under /root. XDG_CACHE_HOME is pinned to /home/app/.cache so the
# cache path is deterministic regardless of the default HOME resolution.
FROM debian:bookworm-slim AS model
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd -m -u 1000 app
COPY --from=builder /build/target/release/memory-mcp /usr/local/bin/memory-mcp
USER app
ENV HOME=/home/app
RUN /usr/local/bin/memory-mcp warmup

# Stage 3: Runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git libdbus-1-3 && rm -rf /var/lib/apt/lists/*
RUN useradd -m -u 1000 memory-mcp
COPY --from=builder /build/target/release/memory-mcp /usr/local/bin/memory-mcp
# Copy the pre-warmed model cache from the model stage.
COPY --from=model /home/app/.cache/fastembed /home/memory-mcp/.cache/fastembed
RUN chown -R memory-mcp:memory-mcp /home/memory-mcp/.cache
USER memory-mcp
WORKDIR /home/memory-mcp
ENV MEMORY_MCP_BIND=0.0.0.0:8080
ENV MEMORY_MCP_REPO_PATH=/data/repo
# Explicitly pin XDG_CACHE_HOME so fastembed always resolves to the same path
# regardless of HOME or XDG defaults in the base image.
ENV XDG_CACHE_HOME=/home/memory-mcp/.cache
EXPOSE 8080
ENTRYPOINT ["memory-mcp"]
CMD ["serve"]
