# Mitosis Project

## Project Overview
Primary language: Rust. Always run `cargo check` or `cargo build` after changes to verify compilation. Prefer practical runtime verification over purely syntactic checks.

## Working Style
- When the user suggests a simpler approach or architectural change, adopt it immediately rather than defending the current implementation. The user has better context on the desired design.
- Before implementing non-trivial features, outline the proposed architecture and get approval before writing code.

## Concurrency & Parallelism Patterns
- When implementing concurrent/parallel patterns (producer-consumer, async pipelines), prefer the simplest architecture that overlaps I/O operations.
- Do NOT collect all items before processing — overlap production and consumption.
- Avoid redundant intermediate data structures (e.g., separate list + dict for the same data).
- Ask for clarification on the desired concurrency model before implementing.

## Streaming & Real-Time Data
- When streaming data (stdout, network, etc.), always verify that output is truly streaming in real-time and not being buffered.
- Test with continuous output commands (e.g., `ping`) rather than short-lived ones.

## Git Workflow
- Before committing, always run `git status` and `git diff --cached` to ensure only the intended files are staged.
- Never commit previously staged files that aren't part of the current task.
- If unsure, show the user what will be committed before proceeding.
