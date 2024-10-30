# Introduction

**Mitosis** is a Rust library and a command line tool to run distributed platforms for transport research.

This guide is an example of how to use Mitosis to run a simple distributed platform to parallelize your tasks.
It is designed for transport-layer research, but it can be used for any other purpose.

## Basic Workflow

The Mitosis CLI tool is a single binary that provides subcommands for starting the Coordinator, Worker and Client processes.

Users function as units for access control, while groups operate as units for tangible resource control.
Every user has an identically named group but also has the option to create or join additional groups.

Users can delegate tasks to various groups via the Client, which are then delivered to the Coordinator and subsequently executed by the corresponding Worker.
Each Worker can be configured to permit specific groups and carry tags to denote its characteristics.

Tasks, once submitted, are distributed to different Workers based on their groups and tags. Every task is assigned a unique UUID, allowing users to track the status and results of their tasks.

## Contributing

Mitosis is free and open source. You can find the source code on
[GitHub](https://github.com/stack-rs/mitosis) and issues and feature requests can be posted on
the [GitHub issue tracker](https://github.com/stack-rs/mitosis/issues). Mitosis relies on the community to fix bugs and
add features: if you'd like to contribute, please read
the [CONTRIBUTING](https://github.com/stack-rs/mitosis/blob/master/CONTRIBUTING.md) guide and consider opening
a [pull request](https://github.com/stack-rs/mitosis/pulls).
