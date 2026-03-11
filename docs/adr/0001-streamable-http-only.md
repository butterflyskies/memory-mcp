# ADR-0001: Streamable HTTP Only

## Status
Accepted

## Context
MCP supports three server transports: stdio, SSE, and Streamable HTTP. The server needs to
run both locally (dev) and in a Kubernetes cluster (production, multi-machine access).

## Decision
Use Streamable HTTP as the sole transport. No stdio, no SSE. Serve via Axum on a configurable
bind address — `127.0.0.1:3000` for local dev, `0.0.0.0:3000` for k8s.

## Consequences
- One transport, one code path — simpler to maintain and debug
- SSE is deprecated in the MCP spec (2025-03-26); no reason to support legacy in a greenfield project
- stdio would require a separate code path for process-level IPC; HTTP on localhost achieves the same thing
- MCP clients that only support stdio cannot use this server (not a current constraint)
