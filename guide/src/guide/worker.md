# Running a Worker

A Worker is a process that executes tasks.
It is responsible for fetching tasks from the Coordinator, executing them, and reporting the results back to the Coordinator.
The Worker is a long-running process that is typically deployed as a service.

## Environment inside a Worker

Before starting a Worker, we need to understand the environment inside a Worker.

The Worker will spawn a new process for each task it runs and set up the following environment variables:

- `MITO_TASK_UUID`: This will be set to the UUID of the task being executed.
- `MITO_NEW_TASK`: This will be set to the path a file where you can write a new task specification (i.e., SubmitTaskReq) in json format for the Worker to submit it on behalf of you, as a downstream task of the current task.
- `MITO_UPSTREAM_TASK_UUID`: This will be set to the UUID of the upstream task if the current task is submitted by another task while running.
- `MITO_RESOURCE_DIR`: This will be set to the path of a directory where you can find the resources (i.e., attachments) of the task.
- `MITO_RESULT_DIR`: This will be set to the path of a directory where you can store the results of the task. The Worker will pack the directory and upload it as the artifacts of the task if it is not empty.
- `MITO_EXEC_DIR`: This will be set to the path of a directory where you can store execution logs. The Worker will pack the directory and upload it as the artifacts of the task if it is not empty.

## Starting a Worker

To start a Worker, you need to provide a TOML file that configures the Worker.
The TOML file specifies the Worker's configuration, such as the polling (fetching) interval, the URL of the Coordinator, and the the groups allowed to submit tasks to it.
All configuration options are optional and have default values.

The Worker will merge the configuration from the file and the command-line arguments according to the following order (the latter overrides the former):

```md
DEFAULT <- `$CONFIG_DIR`/mitosis/config.toml <- config file specified by `cli.config` or loal `config.toml` <- env prefixed by `MITO_` <- cli arguments

`$CONFIG_DIR` will be different on different platforms:

- Linux: `$XDG_CONFIG_HOME` or `$HOME`/.config
- macOS: `$HOME`/Library/Application Support
- Windows: {FOLDERID_RoamingAppData}
```

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

You can also specify the groups and their roles to this Worker using the `--groups` argument.
The default roles for the groups are `Write`, meaning that the groups can submit tasks to this Worker.
Groups have `Read` roles can query the Worker for its status and tasks.
Groups have `Admin` roles can manage the Worker, such as stopping it or changing its configuration.

```bash
mito worker --config /path/to/worker.toml --groups group1,group2:write,group3:read,group4:admin
```

This will grant group1 and group2 `Write` roles, group3 `Read` role, and group4 `Admin` role to the Worker.
The user who creates the Worker will be automatically granted the `Admin` role of the Worker.

Another important argument is `--tags`, the tags of the Worker.
It defines the characteristics of the Worker, such as its capabilities or the type of tasks it can handle.
It is designed for some specific tasks who has special requirements on Workers.
Only when a Worker's tags are empty or are the subset of the task's tags, the Worker can fetch the task.

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
