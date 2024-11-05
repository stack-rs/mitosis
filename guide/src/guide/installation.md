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
For example, do the following on Linux:

```bash
wget https://github.com/stack-rs/mitosis/releases/download/mito-v0.1.0/mito-x86_64-unknown-linux-gnu.tar.xz
tar xf mito-x86_64-unknown-linux-gnu.tar.xz
cd mito-x86_64-unknown-linux-gnu
sudo install -m 755 mito /usr/local/bin/mito
```

We also have a installer script that you can use to install the latest version of Mitosis. You can change the version number in the URL to install a specific version. This script will install the binary in the `$HOME/.cargo/bin` directory.

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/stack-rs/mitosis/releases/download/mito-v0.1.0/mito-installer.sh | sh
```

[releases]: https://github.com/stack-rs/mitosis/releases

## Build from source using Rust

### Dependencies

You have to install pkg-config, libssl-dev if you want to build the binary from source.

### Installing with Cargo

To build the `mito` executable from source, you will first need to install Rust and Cargo.
Follow the instructions on the [Rust installation page].

Once you have installed Rust, the following command can be used to build and install mito:

```bash
cargo install mito
```

This will automatically download mito from [crates.io], build it, and install it in Cargo's global binary directory (`~/.cargo/bin/` by default).

You can run `cargo install mito` again whenever you want to update to a new version.
That command will check if there is a newer version, and re-install mito if a newer version is found.

To uninstall, run the command `cargo uninstall mito`.

[Rust installation page]: https://www.rust-lang.org/tools/install
[crates.io]: https://crates.io/

### Installing the latest git version with Cargo

The version published to crates.io will ever so slightly be behind the version hosted on GitHub.
If you need the latest version you can build the git version of mito yourself.
Cargo makes this ***super easy***!

```bash
cargo install --git https://github.com/stack-rs/mitosis.git mito
```

Again, make sure to add the Cargo bin directory to your `PATH`.

### Building from source

If you want to build the binary from source, you can clone the repository and build it using Cargo.

```bash
git clone https://github.com/stack-rs/mitosis.git
cd mitosis
cargo build --release
```

Then you can find the binary in `target/release/mito` and install or run it as you like.

## Modifying and contributing

If you are interested in making modifications to Mitosis itself, check out the [Contributing Guide] for more information.

[Contributing Guide]: https://github.com/stack-rs/mitosis/blob/master/CONTRIBUTING.md
