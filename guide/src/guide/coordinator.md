# Running a Coordinator

A Coordinator is a process that manages the execution of a workflow. It is responsible for scheduling tasks, tracking their progress, and handling failures. The Coordinator is a long-running process that is typically deployed as a service.

## Starting a Coordinator

To start a Coordinator, you need to provide a TOML file that configures the Coordinator. The TOML file specifies the Coordinator's configuration, such as the address it binds to, the URL of the postgres database, and token expiry settings. All configuration options are optional and have default values.

Here is an example of a Coordinator configuration file:

```toml
[coordinator]
bind = "127.0.0.1:5000"
db_url = "postgres://mitosis:mitosis@localhost/mitosis"
s3_url = "http://127.0.0.1:9000"
s3_access_key = "mitosis_access"
s3_secret_key = "mitosis_secret"
# redis_url is not set. It should be in format like "redis://:mitosis@localhost"
# redis_worker_password is not set by default and will be generated randomly
# redis_client_password is not set by default and will be generated randomly
admin_user = "mitosis_admin"
admin_password = "mitosis_admin"
access_token_private_path = "private.pem"
access_token_public_path = "public.pem"
access_token_expires_in = "7d"
heartbeat_timeout = "600s"
file_log = false
# log_path is not set. It will use the default rolling log file path if file_log is set to true
```

For the private and public keys, you can generate them using the following commands:

```sh
openssl genpkey -algorithm ed25519 -out private.pem
openssl pkey -in private.pem -pubout -out public.pem
```

To start a Coordinator, run the following command:

```sh
mito coordinator --config /path/to/coordinator.toml
```

The Coordinator will start and listen for incoming requests on the specified address.

We can also override the configuration settings using command-line arguments.
Note that the names of command-line arguments may not be the same as those in the configuration file.
For example, to change the address the Coordinator binds to, you can run:

```sh
mito coordinator --config /path/to/coordinator.toml --bind 0.0.0.0:8000
```

The full list of command-line arguments can be found by running `mito coordinator --help`:

```txt
Run the mitosis coordinator

Usage: mito coordinator [OPTIONS]

Options:
  -b, --bind <BIND>
          The address to bind to
      --config <CONFIG>
          The path of the config file
      --db <DB_URL>
          The database URL
      --s3 <S3_URL>
          The S3 URL
      --s3-access-key <S3_ACCESS_KEY>
          The S3 access key
      --s3-secret-key <S3_SECRET_KEY>
          The S3 secret key
      --redis <REDIS_URL>
          The Redis URL
      --redis-worker-password <REDIS_WORKER_PASSWORD>
          The Redis worker password
      --redis-client-password <REDIS_CLIENT_PASSWORD>
          The Redis client password
      --admin-user <ADMIN_USER>
          The admin username
      --admin-password <ADMIN_PASSWORD>
          The admin password
      --access-token-private-path <ACCESS_TOKEN_PRIVATE_PATH>
          The path to the private key, default to `private.pem`
      --access-token-public-path <ACCESS_TOKEN_PUBLIC_PATH>
          The path to the public key, default to `public.pem`
      --access-token-expires-in <ACCESS_TOKEN_EXPIRES_IN>
          The access token expiration time, default to 7 days
      --heartbeat-timeout <HEARTBEAT_TIMEOUT>
          The heartbeat timeout, default to 600 seconds
      --log-path <LOG_PATH>
          The log file path. If not specified, then the default rolling log file path would be used. If specified, then the log file would be exactly at the path specified
      --file-log
          Enable logging to file
  -h, --help
          Print help
  -V, --version
          Print version
```
