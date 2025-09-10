# Mitosis: A Unified Transport Evaluation Framework

**Mitosis** is a Rust library and command-line tool designed for running distributed platforms, particularly for transport research. It provides a framework for parallelizing tasks across multiple workers in a controlled, managed environment.
It is designed for transport-layer research, but it can be used for any other purpose.

## Overview

Mitosis consists of three main components:

- **Coordinator**: Central management service that schedules tasks and tracks execution
- **Worker**: Task execution nodes that fetch and run jobs
- **Client**: Interface for submitting tasks, managing users, and monitoring progress

The system uses a user-group-based access control model where users can delegate tasks to groups, which are then executed by workers that have been configured to serve those groups.

You can view the full documentation and guidelines at [docs.stack.rs/mitosis](https://docs.stack.rs/mitosis).

## Quick Start

### Prerequisites

- Rust 1.76 or later
- PostgreSQL database
- S3-compatible storage (e.g., MinIO)
- Redis (optional, for enhanced monitoring)

### Installation

**Using pre-compiled binaries:**

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/stack-rs/mitosis/releases/latest/download/mito-installer.sh | sh
```

**From source:**

```bash
git clone https://github.com/stack-rs/mitosis.git
cd mitosis
cargo build --release
```

**From crates.io:**

```bash
cargo install mito
```

Now you can execute the `mito` command to enter the interactive client mode or use subcommands directly.

```bash
# Enter interactive mode:
mito client -i
# directly execute a command:
mito client tasks submit -- echo "Hello, Mitosis!"
```

For setting up the whole service (including the Coordinator and the Worker), please refer to our full [guidelines](https://docs.stack.rs/mitosis).

## Key Features

- **Distributed Task Execution**: Scale workloads across multiple workers
- **Role-Based Access Control**: Fine-grained permissions for users and groups
- **Tag-Based Worker Selection**: Target specific worker capabilities
- **Artifact Management**: Automatic collection and storage of task outputs
- **Interactive Client**: User-friendly CLI with both interactive and batch modes
- **SDK Support**: Rust and Python library for programmatic integration

## Architecture

Mitosis follows a coordinator-worker pattern:

1. **Client** submits tasks to the **Coordinator**
2. **Coordinator** stores tasks and makes them available to **Workers**
3. **Workers** poll for tasks, execute them, and report results back
4. Results and artifacts are stored and can be retrieved through the **Client**

## Documentation

- **[User Guide](https://docs.stack.rs/mitosis)** - Complete documentation
- **[API Reference](https://docs.rs/netmito)** - Rust SDK documentation
- **[Contributing Guide](CONTRIBUTING.md)** - Development setup and guidelines
- **[OpenAPI Spec](openapi.yaml)** - The API specification for the Coordinator's HTTP endpoints

## Use Cases

- **Research Computing**: Distribute computational workloads across lab resources
- **CI/CD Pipelines**: Execute build and test tasks on dedicated workers
- **Data Processing**: Parallel processing of large datasets
- **Network Testing**: Transport layer protocol evaluation and benchmarking
- **General Distributed Computing**: Any scenario requiring managed task distribution

## Community & Support

- **GitHub Issues**: [Report bugs and request features](https://github.com/stack-rs/mitosis/issues)
- **Discussions**: [Community discussions and Q&A](https://github.com/stack-rs/mitosis/discussions)
- **Contributing**: We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md)

## License

Licensed under the Apache License 2.0. See [LICENSE](LICENSE) for details.
