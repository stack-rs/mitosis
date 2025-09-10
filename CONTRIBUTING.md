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

1. **Clone the repository**:

   ```bash
   git clone https://github.com/stack-rs/mitosis.git
   cd mitosis
   ```

2. **Install dependencies**:

   ```bash
   # Ubuntu/Debian
   sudo apt install build-essential pkg-config libssl-dev

   # CentOS/RHEL/Fedora
   sudo dnf install gcc gcc-c++ openssl-devel pkgconfig
   ```

3. **Build the project**:

   ```bash
   cargo build
   # Or for release build
   cargo build --release
   ```

The resulting binary can be found in `target/debug/mito` (or `target/release/mito` for release builds).

## Development Workflow

### Setting Up Development Environment

You should set up the environment according to our [Guide](https://docs.stack.rs/mitosis/)

### Making Changes

1. **Create a feature branch**:

   ```bash
   git checkout -b your-feature-name
   ```

2. **Make your changes** following the coding guidelines below

3. **Test your changes**:

   ```bash
   cargo clippy --workspace
   cargo fmt --all
   ```

4. **Update documentation** if needed:

   ```bash
   # Update user guide
   cd guide && mdbook build
   ```

5. **Commit with descriptive messages**:

   ```bash
   git commit -m "feat(coordinator): add task priority scheduling

   - Implement priority queue for task scheduling
   - Add priority field to task submission API
   - Update database schema with migration

   Closes #123"
   ```

### Code Quality Standards

Before submitting a pull request:

1. **Format code**: `cargo fmt`
2. **Fix linting issues**: `cargo clippy`
3. **Run tests**: `cargo test`
4. **Security audit**: `cargo audit`
5. **Check documentation**: `cargo doc`

### Commit Message Guidelines

We follow [Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` New features
- `fix:` Bug fixes
- `docs:` Documentation changes
- `style:` Code style changes (formatting, etc.)
- `refactor:` Code refactoring without functionality changes
- `test:` Adding or modifying tests
- `chore:` Maintenance tasks, dependency updates

**Examples**:

```
feat(client): add interactive task monitoring
fix(worker): resolve memory leak in task execution
docs(api): improve SDK examples and error handling
```

## Testing Guidelines

### Writing Tests

- **Unit tests**: Test individual functions and modules
- **Integration tests**: Test API endpoints and workflows
- **Documentation tests**: Ensure code examples work

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_task_submission() {
        // Test implementation
    }
}
```

### Running Tests

```bash
# All tests
cargo test

# Specific test
cargo test test_task_submission

# Integration tests only
cargo test --test integration

# With output
cargo test -- --nocapture

# Using nextest (faster)
cargo nextest run
```

## Documentation Standards

### Code Documentation

- Add rustdoc comments to public APIs
- Include examples in documentation
- Document error conditions and panics

````rust
/// Submits a new task to the coordinator
///
/// # Arguments
/// * `task` - The task specification to submit
///
/// # Returns
/// Returns the UUID of the created task
///
/// # Errors
/// Returns `MitoError::Unauthorized` if user lacks permissions
///
/// # Example
/// ```rust
/// let task_id = client.submit_task(task_spec).await?;
/// ```
pub async fn submit_task(&self, task: TaskSpec) -> Result<Uuid, MitoError> {
    // Implementation
}
````

### User Documentation

When adding new features, update:

- User guide in `guide/src/`
- API documentation and examples
- Configuration file examples
- Troubleshooting guide if applicable

## Performance Guidelines

### Optimization Principles

1. **Async/Await**: Use async for I/O operations
2. **Connection Pooling**: Reuse database connections
3. **Streaming**: Use streaming for large data transfers
4. **Caching**: Cache frequently accessed data
5. **Batch Operations**: Group operations where possible

### Profiling

```bash
# CPU profiling
cargo install flamegraph
cargo flamegraph --bin mito -- coordinator

# Memory profiling
valgrind --tool=massif ./target/debug/mito coordinator
```

## Getting Help

- **Questions**: Open a [Discussion](https://github.com/stack-rs/mitosis/discussions)
- **Bug Reports**: Create an [Issue](https://github.com/stack-rs/mitosis/issues)
- **Feature Requests**: Create an [Issue](https://github.com/stack-rs/mitosis/issues) with the "enhancement" label

## Recognition

Contributors are recognized in:

- Release notes and changelog
- Contributors section of documentation
- GitHub contributor graphs

Thank you for contributing to Mitosis! 🚀
