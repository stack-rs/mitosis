# Running a Worker

A Worker is a process that executes tasks.
It is responsible for fetching tasks from the Coordinator, executing them, and reporting the results back to the Coordinator.
The Worker is a long-running process that is typically deployed as a service.

## Starting a Worker

To start a Worker, you need to provide a TOML file that configures the Worker.
The TOML file specifies the Worker's configuration, such as the polling (fetching) interval, the URL of the Coordinator, and the the groups allowed to submit tasks to it.
All configuration options are optional and have default values.

Here is an example of a Worker configuration file (you can also refer to `config.example.toml` in the repository):

```toml
[worker]
coordinator_addr = "http://127.0.0.1:5000"
polling_interval = "3m"
heartbeat_interval = "5m"
lifetime = "7d"
# credential_path is not set
# user is not set
# password is not set
# groups are not set, default to the user's group
# tags are not set
file_log = false
# log_path is not set. It will use the default rolling log file path if file_log is set to true
# lifetime is not set, default to the coordinator's setting
```

To start a Worker, run the following command:

```bash
mito worker --config /path/to/worker.toml
```

The Worker will start and fetch tasks from the Coordinator at the specified interval.

We can also override the configuration settings using command-line arguments.
Note that the names of command-line arguments may not be the same as those in the configuration file.
For example, to change the polling interval, you can run:

```bash
mito worker --config /path/to/worker.toml --polling-interval 5m
```

The full list of command-line arguments can be found by running `mito worker --help`:

```txt
Run a mitosis worker

Usage: mito worker [OPTIONS]

Options:
      --config <CONFIG>
          The path of the config file
  -c, --coordinator <COORDINATOR_ADDR>
          The address of the coordinator
      --polling-interval <POLLING_INTERVAL>
          The interval to poll tasks or resources
      --heartbeat-interval <HEARTBEAT_INTERVAL>
          The interval to send heartbeat
      --credential-path <CREDENTIAL_PATH>
          The path of the user credential file
  -u, --user <USER>
          The username of the user
  -p, --password <PASSWORD>
          The password of the user
  -g, --groups [<GROUPS>...]
          The groups allowed to submit tasks to this worker
  -t, --tags [<TAGS>...]
          The tags of this worker
      --log-path <LOG_PATH>
          The log file path. If not specified, then the default rolling log file path would be used. If specified, then the log file would be exactly at the path specified
      --file-log
          Enable logging to file
      --lifetime <LIFETIME>
          The lifetime of the worker to alive (e.g., 7d, 1year)
  -h, --help
          Print help
  -V, --version
          Print version
```
