# Installation

The Mitosis project contains a CLI tool (named `mito`) that you can use to directly start a distributed platform,
and a SDK library (named `netmito`) that you can use to create your own client.

There are multiple ways to install the Mitosis CLI tool.
Choose any one of the methods below that best suit your needs.

## Pre-compiled binaries

Executable binaries are available for download on the [GitHub Releases page][releases].
Download the binary and extract the archive.
The archive contains an `mito` executable which you can run to start your distributed platform.

We have a installer script that you can use to install Mitosis (you may need to adjust the version number in the URL to the latest in the [releases] page).
You can also change the version number in the URL to install a specific version. This script will install the binary in the `$HOME/.cargo/bin` directory.

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/stack-rs/mitosis/releases/download/mito-v0.4.0/mito-installer.sh | sh
```

You can also download the binary directly from the [releases] page and install it manually.
To make it easier to run, put the path to the binary into your `PATH` or install it in a directory that is already in your `PATH`.
For example, do the following on Linux (you may need to adjust the version number to the latest in the URL):

```bash
wget https://github.com/stack-rs/mitosis/releases/download/mito-v0.4.0/mito-x86_64-unknown-linux-gnu.tar.xz
tar xf mito-x86_64-unknown-linux-gnu.tar.xz
cd mito-x86_64-unknown-linux-gnu
sudo install -m 755 mito /usr/local/bin/mito
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
Cargo makes this **_super easy_**!

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

If you encounter compilation errors on rustls or aws-lc-sys in older Linux distributions, check gcc version and consider updating it.
For example:

```bash
sudo apt update -y
sudo apt upgrade -y
sudo apt install -y build-essential
sudo apt install -y gcc-10 g++-10 cpp-10
sudo update-alternatives --install /usr/bin/gcc gcc /usr/bin/gcc-10 100 --slave /usr/bin/g++ g++ /usr/bin/g++-10 --slave /usr/bin/gcov gcov /usr/bin/gcov-10
```

## Modifying and contributing

If you are interested in making modifications to Mitosis itself, check out the [Contributing Guide] for more information.

[Contributing Guide]: https://github.com/stack-rs/mitosis/blob/master/CONTRIBUTING.md
