# Running a Client

A Client is a process that interact with the Coordinator.
It is responsible for creating tasks, querying their results, and managing workers or groups.
The Client is a short-lived process that is typically run on-demand.

## Starting a Client

While it's possible to provide a TOML configuration file to the client,
it's often unnecessary given the limited number of configuration items, all of which pertain to login procedures.

Typically, to start a Client, we can simply run the following command to enter interactive mode:

```bash
mito client -i
```

If a user has never logged in or if his/her session has expired, the Client will prompt them to re-input their username and password for authentication.
Alternatively, they can directly specify their username (`-u`) or password (`-p`) during execution.
Once authenticated, the Client will retain their credentials in a file for future use.

We recommend using the interactive mode for most operations, as it provides a more user-friendly experience. It will prompt you something like this:

```txt
[mito::client]>
```

You can press `CTRL-D` or type in `exit` to exit the interactive mode. `CTRL-C` will just clear the current line and prompt you again.

We can also directly run a command without entering interactive mode by specifying the command as an argument.
For example, to create a new user, we can run:

```bash
mito client create user -u new_user -p new_password
```

The full list of command-line arguments can be found by running `mito client --help`:

```txt
Run a mitosis client

Usage: mito client [OPTIONS] [COMMAND]

Commands:
  auth      Authenticate the user
  create    Create a new user or group
  get       Get the info of a task, artifact, attachment, or a list of tasks subject to the filters
  submit    Submit a task
  upload    Upload an artifact or attachment
  manage    Manage a worker, a task or a group
  shutdown  Shutdown the coordinator
  quit      Quit the client's interactive mode
  help      Print this message or the help of the given subcommand(s)

Options:
      --config <CONFIG>                    The path of the config file
  -c, --coordinator <COORDINATOR_ADDR>     The address of the coordinator
      --credential-path <CREDENTIAL_PATH>  The path of the user credential file
  -u, --user <USER>                        The username of the user
  -p, --password <PASSWORD>                The password of the user
  -i, --interactive                        Enable interactive mode
  -h, --help                               Print help
  -V, --version                            Print version
```

To know how each subcommand works, you can run `mito client <subcommand> --help`.
For example, to know how to create a new user, you can run `mito client create user --help`:

```txt
Create a new user

Usage: mito client create user [OPTIONS] --username <USERNAME> --password <PASSWORD>

Options:
  -u, --username <USERNAME>  The username of the user
  -p, --password <PASSWORD>  The password of the user
      --admin                Whether the user is an admin
  -h, --help                 Print help
  -V, --version              Print version
```

For the rest of this section, we will explain the common use cases of the Client on different scenarios.
For the sake of convenience, we will assume that the user is already in interactive mode.
And for the direct executing mode, it only requires adding "mito client" at the front.

## `Create` sub-commands

Input `help create` to show the help message of the `create` sub-commands:

```txt
Create a new user or group

Usage: create <COMMAND>

Commands:
  user   Create a new user
  group  Create a new group
  help   Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

We can create a new user by running the following command:

```txt
create user -u test_user_name -p test_user_password
```

We can create a new group by running the following command:

```txt
create group test_group
```

This will create a group called `test_group` containing the current logged in user.
This user will be granted the `Admin` role to this group to manage it.

## `Submit` sub-commands

Input `help submit` to show the help message of the `submit` sub-commands:

```txt
Submit a task

Usage: submit [OPTIONS] [-- <COMMAND>...]

Arguments:
  [COMMAND]...  The command to run

Options:
  -g, --group <GROUP_NAME>    The name of the group this task is submitted to
  -t, --tags [<TAGS>...]      The tags of the task, used to filter workers to execute the task
  -l, --labels [<LABELS>...]  The labels of the task, used for querying tasks
      --timeout <TIMEOUT>     The timeout of the task [default: 10min]
  -p, --priority <PRIORITY>   The priority of the task [default: 0]
  -e, --envs [<ENVS>...]      The environment variables to set
      --terminal              Whether to collect the terminal standard output and error of the executed task
      --watch <WATCH>         The UUID and the state of the task to watch before triggering this task. Should specify it as `UUID,STATE`, e.g. `123e4567-e89b-12d3-a456-426614174000,ExecSpawned`
  -h, --help                  Print help
```

Submit a task to the Coordinator can be as simple as running the following command:

```txt
submit -- echo hello
```

The content after `--` is the command to run on the worker. It will return a UUID to identify the task.

You can also specify the group to submit the task to by using the `-g` option.

The `labels` are used to mark the task for querying later, it won't affect how the task is fetched ans executed.

The `tags` are used to define the characteristics of the task, such as its requirements on the Worker.
Only when a Worker's tags are empty or are the subset of the task's tags, the Worker can fetch the task.

You can also set some environment variables for the task by using the `-e` option.

```txt
submit -g test_group -t wireless,4g -l mobile,video -e TEST_KEY=1,TEST_VAL=2 -- echo hello
```

For the output of the task, we allow 3 types of output to be collected:

1. **Result**: Files put under the directory specified by the environment variable `MITO_RESULT_DIR` will be packed into an artifact and uploaded to the Coordinator.
   If the directory is empty, no artifact will be created.
2. **Exec**: Files put under the directory specified by the environment variable `MITO_EXEC_DIR` will be packed into an artifact and uploaded to the Coordinator.
   If the directory is empty, no artifact will be created.
3. **Terminal**: If the `--terminal` option is specified, the standard output and error of the executed task will be collected and uploaded to the Coordinator.
   The terminal output will be stored in a file named `stdout.log` and `stderr.log` respectively in an artifact.

## `Get` sub-commands

Input `help get` to show the help message of the `get` sub-commands:

```txt
Get the info of task, attachment, worker or group, or query a list of them subject to the filters. Download attachment and artifact is also supported

Usage: get <COMMAND>

Commands:
  task             Get the info of a task
  tasks            Query a list of tasks subject to the filter
  attachment-meta  Get the metadata of an attachment
  attachments      Query a list of attachments subject to the filter
  worker           Get the info of a worker
  workers          Query a list of workers subject to the filter
  group            Get the information of a group
  groups           Get all groups the user has access to
  artifact         Download an artifact of a task
  attachment       Download an attachment of a group
  help             Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help (see more with '--help')
```

Basically, the `get` sub-commands are used to query information or download files from the Coordinator.
For information, it can be a task, a worker, a group, or a list of them subject to the filters.

For example, you get a task's information by providing its UUID:

```txt
get task e07a2bf2-166d-40b5-8bb6-a78104c072f9
```

Or you can just query a list of tasks with label `mobile`:

```txt
get tasks -l mobile
```

More filter options can be found in the help message by executing `get tasks -h`

You can also get the information of a group with its name and that of a worker with its id.
Query a list of them is also supported with similar logic as querying tasks.

For downloading files, you can download an artifact of a task or an attachment of a group.
To make it clear, an artifact is a collection of files generated by a task (as output), while an attachment is a file uploaded to a group.

It is easy to download an artifact of a task by providing its UUID. But you also have to specify the the output type you want.
There are three types of output: `result`, `exec-log`, and `std-log`. You can also specify the output path to download the artifact to with `-o` argument.

```txt
get artifact e07a2bf2-166d-40b5-8bb6-a78104c072f9 result
```

To download an attachment of a group, you can provide the group name and the attachment key:

```txt
get attachment test_group attachment_key
```

## `Upload` sub-commands

Input `help upload` to show the help message of the `upload` sub-commands:

```txt
Upload an artifact or attachment

Usage: upload <COMMAND>

Commands:
  artifact    Upload an artifact to a task
  attachment  Upload an attachment to a group
  help        Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

Similar to how we download files with `get` sub-commands, we can upload an artifact to a task or an attachment to a group.

For example, to upload an artifact to a task as result, we can run:

```txt
upload artifact e07a2bf2-166d-40b5-8bb6-a78104c072f9 result local.tar.gz
```

Another example, to upload an attachment to a group, we can run:

```txt
upload attachment -g test_group local.tar.gz attachment_key
```

You can also just run `upload attachment local.tar.gz`.
This will directly upload the file to the current group you are in and use the file name as the attachment key.

## `Manage` sub-commands

Input `help manage` to show the help message of the `manage` sub-commands:

```txt
Manage a worker, a task or a group

Usage: manage <COMMAND>

Commands:
  worker  Manage a worker
  task    Manage a task
  group   Manage a group
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help0
```

We can manage a worker, a task, or a group with the `manage` sub-commands.

For example, we can stop a worker by running:

```txt
manage worker b168dbe6-5c44-4529-a3b4-51940d6bb3c5 cancel
```

Or we can update the tags of a worker by running:

```txt
manage worker b168dbe6-5c44-4529-a3b4-51940d6bb3c5 update-tags wired file
```

And we can grant another group `Write` access to this worker (it means the group can submit tasks to this worker) by running:

```txt
manage worker b168dbe6-5c44-4529-a3b4-51940d6bb3c5 update-roles test_group:admin another_group:write
```

You can perform the opposite action to remove certain groups' access permissions to the Worker using the `remove-roles` subcommand.

For a task, we can also cancel it, update its labels or change its specification to run with its UUID provides. For example:

```txt
manage task e07a2bf2-166d-40b5-8bb6-a78104c072f9 cancel
```

This will cancel the task if it is not started yet.

To change how the task is executed, we can run:

```txt
manage task e07a2bf2-166d-40b5-8bb6-a78104c072f9 change --terminal -- echo world
```

This will alter the task to collect standard output and error when finishes, and execute `echo world` instead of `echo hello`.
