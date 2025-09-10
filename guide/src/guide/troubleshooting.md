# Troubleshooting Guide

This guide covers common issues you might encounter when setting up and running Mitosis, along with their solutions.

## Installation Issues

### Binary Not Found After Installation

**Problem**: `mito: command not found` after installation.

**Solution**:

1. Verify the binary location:

   ```bash
   which mito
   find / -name "mito" 2>/dev/null
   ```

2. Add to PATH if needed:

   ```bash
   export PATH="$HOME/.cargo/bin:$PATH"
   # Add to your shell profile (.bashrc, .zshrc, etc.)
   echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.bashrc
   ```

### Permission Denied

**Problem**: Permission errors when running the installer or binary.

**Solution**:

```bash
# Make binary executable
chmod +x mito

# Fix installer permissions
chmod +x mito-installer.sh
```

### SSL/TLS Certificate Issues

**Problem**: Certificate verification errors during download.

**Solution**:

```bash
# Update certificates
sudo apt-get update && sudo apt-get install ca-certificates

# Or bypass for known-safe sources (not recommended for production)
curl -k --proto '=https' --tlsv1.2 -LsSf [URL]
```

## Build Issues

### Missing Dependencies

**Problem**: Compilation fails due to missing system libraries.

**Solution**:

```bash
# Ubuntu/Debian
sudo apt install build-essential pkg-config libssl-dev

# CentOS/RHEL
sudo yum install gcc gcc-c++ openssl-devel pkgconfig
```

### Rust Version Issues

**Problem**: Compilation fails due to incompatible Rust version.

**Solution**:

```bash
# Update Rust
rustup update

# Check version (needs 1.76+)
rustc --version

# Set specific toolchain if needed
rustup default stable
```

### Link Errors on Older Systems

**Problem**: Linking errors with glibc or other system libraries.

**Solution**:
Use the musl build instead:

```bash
# Install musl target
rustup target add x86_64-unknown-linux-musl

# Build with musl
cargo build --target x86_64-unknown-linux-musl --release
```

## Configuration Issues

### Database Connection Failures

**Problem**: `FATAL: database "mitosis" does not exist`

**Solution**:

1. Create the database:

   ```sql
   psql -U postgres -c "CREATE DATABASE mitosis;"
   psql -U postgres -c "CREATE USER username WITH PASSWORD 'password';"
   psql -U postgres -c "GRANT ALL PRIVILEGES ON DATABASE mitosis TO useranme;"
   ```

2. Check connection string format:

   ```toml
   db_url = "postgres://username:password@host:port/mitosis"
   ```

### S3 Storage Connection Issues

**Problem**: S3 authentication or connection failures.

**Solution**:

1. Verify MinIO/S3 is running:

   ```bash
   # For MinIO
   docker ps | grep minio
   curl http://localhost:9000/minio/health/ready
   ```

2. Check credentials and bucket access:

   ```bash
   # Using AWS CLI to test
   aws --endpoint-url=http://localhost:9000 s3 ls
   ```

### Redis Connection Problems

**Problem**: Redis connection refused or authentication failures.

**Solution**:

1. Check Redis status:

   ```bash
   redis-cli ping
   # Should return PONG
   ```

2. Verify ACL rules (Redis 6.0+):

   ```bash
   redis-cli ACL LIST
   redis-cli ACL DELUSER default  # If needed
   ```

### SSL Key Generation Issues

**Problem**: Ed25519 key generation fails or keys not recognized.

**Solution**:

1. Ensure OpenSSL version supports Ed25519 (1.1.1+):

   ```bash
   openssl version
   ```

2. Generate keys correctly:

   ```bash
   openssl genpkey -algorithm ed25519 -out private.pem
   openssl pkey -in private.pem -pubout -out public.pem
   ```

3. Verify key format:

   ```bash
   openssl pkey -in private.pem -text -noout
   ```

## Network Issues

### Port Already in Use

**Problem**: `Address already in use` when starting coordinator.

**Solution**:

1. Find what's using the port:

   ```bash
   lsof -i :5000
   netstat -tulpn | grep 5000
   ```

2. Change the port:

   ```bash
   mito coordinator --bind 0.0.0.0:5001
   ```

### Firewall Blocking Connections

**Problem**: Workers can't connect to coordinator.

**Solution**:

1. Check firewall rules:

   ```bash
   # Ubuntu
   sudo ufw status
   sudo ufw allow 5000

   # CentOS/RHEL
   sudo firewall-cmd --list-ports
   sudo firewall-cmd --permanent --add-port=5000/tcp
   sudo firewall-cmd --reload
   ```

2. Test connectivity:

   ```bash
   telnet coordinator_host 5000
   curl http://coordinator_host:5000/health
   ```

## Performance Issues

### Slow Task Execution

**Problem**: Tasks taking longer than expected to start or complete.

**Solutions**:

1. Reduce polling interval for workers:

   ```toml
   polling_interval = "30s"  # Faster polling
   ```

2. Increase worker parallelism:

   ```bash
   # Run multiple workers on the same node
   mito worker &
   mito worker &
   ```

3. Monitor database performance:

   ```sql
   SELECT * FROM pg_stat_activity;
   SELECT * FROM pg_stat_user_tables;
   ```

### Database Lock Contention

**Problem**: High lock wait times or deadlocks.

**Solution**:

1. Monitor locks:

   ```sql
   SELECT * FROM pg_locks WHERE NOT granted;
   ```

2. Tune PostgreSQL settings:

   ```postgresql
   max_connections = 100
   shared_buffers = 256MB
   effective_cache_size = 1GB
   ```

## Debugging Tips

### Enable Debug Logging

```bash
RUST_LOG=debug mito coordinator
RUST_LOG=netmito=debug mito worker
RUST_LOG=debug mito client
```

### Health Checks

```bash
# Check coordinator health
curl http://localhost:5000/health

# Check database connection
psql "postgres://mitosis:mitosis@localhost/mitosis" -c "SELECT version();"

# Check S3 connection
aws --endpoint-url=http://localhost:9000 s3 ls
```

## Getting Help

If you continue to experience issues:

1. Check the [GitHub Issues](https://github.com/stack-rs/mitosis/issues) for similar problems
2. Run with debug logging and include logs in your issue report
3. Provide system information:

   ```bash
   mito --version
   rustc --version
   uname -a
   docker --version  # if using Docker
   ```

4. Include relevant configuration (sanitize sensitive data)
5. Describe the expected vs actual behavior
6. List steps to reproduce the issue

