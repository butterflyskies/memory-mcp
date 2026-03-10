# ADR-0005: fastembed Scaffolded and Configurable From the Start

## Status
Accepted

## Context
The TODO specifies "build embedding index alongside storage from the start." The embedding
model could be hardcoded or made configurable. fastembed could be included as a dependency
only or fully wired up in the scaffold.

## Decision
Wire up fastembed in the scaffold with a configurable model via `--embedding-model` CLI arg.
Default to AllMiniLML6V2 (384 dimensions, ~23MB download). The embedding engine and vector
index are initialized at startup, not deferred.

## Consequences
- Semantic recall works from the first functional build, not as a later bolt-on
- Model is swappable without code changes — just change the CLI arg
- First run downloads the model from HuggingFace (~23MB) — acceptable given fast internet
- For k8s: bake model into container image or accept cold-start delay
