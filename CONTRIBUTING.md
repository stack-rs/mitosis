# CONTRIBUTING

We'd be glad for you to contribute to our source code and to make this project better!

Feel free to submit a pull request or an issue, but make sure to use the templates.

It is **required to follow** the **`Language Style`** rules.

## Language Style

Files of different languages should be checked locally according to the following conventions.

Commits should be made after all checks pass or with additional clarifications.

### Rust

Run `cargo fmt`(rustfmt) to format the code.

Run `cargo clippy` to lint the code.

Follow the official [naming convention](https://rust-lang.github.io/api-guidelines/naming.html).

## Building Mitosis

### Dependencies

You have to install pkg-config, libssl-dev if you want to build the binary from source.

### Building

Mitosis builds on stable Rust, if you want to build it from source, here are the steps to follow:

1. Navigate to the directory of your choice
2. Clone this repository with git.

   ```bash
   git clone https://github.com/stack-rs/mitosis.git
   ```

3. Navigate into the newly created `mitosis` directory
4. Run `cargo build`

The resulting binary can be found in `mitosis/target/debug/` under the name `mito` or `mito.exe`.
