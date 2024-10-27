# Installation

The Mitosis project contains a CLI tool (named `mito`) that you can use to directly start a distributed platform,
and a SDK library (named `netmito`) that you can use to create your own client.

There are multiple ways to install the Mitosis CLI tool.
Choose any one of the methods below that best suit your needs.

## Pre-compiled binaries

Executable binaries are available for download on the [GitHub Releases page][releases].
Download the binary and extract the archive.
The archive contains an `mito` executable which you can run to start your distributed platform.

To make it easier to run, put the path to the binary into your `PATH` or install it in a directory that is already in your `PATH`.
For example, do `sudo install mito /usr/local/bin/mito` on Linux.

[releases]: https://github.com/stack-rs/mitosis/releases

## Build from source using Rust

### Dependencies

You have to install pkg-config, libssl-dev if you want to build the binary from source.

### Building

To build the `mito` executable from source, you will first need to install Rust and Cargo.
Follow the instructions on the [Rust installation page].

Once you have installed Rust, the following command can be used to build and install mito:

```sh
cargo install mito
```

This will automatically download mito from [crates.io], build it, and install it in Cargo's global binary directory (`~/.cargo/bin/` by default).

You can run `cargo install mito` again whenever you want to update to a new version.
That command will check if there is a newer version, and re-install mito if a newer version is found.

To uninstall, run the command `cargo uninstall mito`.

[Rust installation page]: https://www.rust-lang.org/tools/install
[crates.io]: https://crates.io/

### Installing the latest master version

The version published to crates.io will ever so slightly be behind the version hosted on GitHub.
If you need the latest version you can build the git version of mito yourself.
Cargo makes this ***super easy***!

```sh
cargo install --git https://github.com/stack-rs/mitosis.git mito
```

Again, make sure to add the Cargo bin directory to your `PATH`.

## Basic Workflow

The Mitosis CLI tool is a single binary that provides subcommands for starting the Coordinator, Worker and Client processes.

Users function as units for access control, while groups operate as units for tangible resource control.
Every user has an identically named group but also has the option to create or join additional groups.

Users can delegate tasks to various groups via the Client, which are then delivered to the Coordinator and subsequently executed by the corresponding Worker. Each Worker can be configured to permit specific groups and carry tags to denote its characteristics.

Tasks, once submitted, are distributed to different Workers based on their groups and tags. Every task is assigned a unique UUID, allowing users to track the status and results of their tasks.

## Modifying and contributing

If you are interested in making modifications to Mitosis itself, check out the [Contributing Guide] for more information.

[Contributing Guide]: https://github.com/stack-rs/mitosis/blob/master/CONTRIBUTING.md
