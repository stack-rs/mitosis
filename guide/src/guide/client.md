# Running a Client

A Client is a process that interact with the Coordinator.
It is responsible for creating tasks, querying their results, and managing workers or groups.
The Client is a short-lived process that is typically run on-demand.

## Starting a Client

While it's possible to provide a TOML configuration file to the client,
it's often unnecessary given the limited number of configuration items, all of which pertain to login procedures.
But it can be useful if you want to set some default values for the client.

The Client will merge the configuration from the file and the command-line arguments according to the following order (the latter overrides the former):

```md
DEFAULT <- `$CONFIG_DIR`/mitosis/config.toml <- config file specified by `cli.config` or loal `config.toml` <- env prefixed by `MITO_` <- cli arguments

`$CONFIG_DIR` will be different on different platforms:

- Linux: `$XDG_CONFIG_HOME` or `$HOME`/.config
- macOS: `$HOME`/Library/Application Support
- Windows: {FOLDERID_RoamingAppData}
```

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

You can press `CTRL-D` or type in `exit` or `quit` to leave the interactive mode. `CTRL-C` will just clear the current line and prompt you again.

We can also directly run a command without entering interactive mode by specifying the command as an argument.
For example, to create a new user, we can run:

```bash
mito client admin users create new_user_name new_password
```

The full list of command-line arguments can be found by running `mito client --help`:

```txt
Run a mitosis client

Usage: mito client [OPTIONS] [COMMAND]

Commands:
  admin    Admin operations, including shutdown the coordinator, chaning user password, etc
  auth     Authenticate current user
  login    Login with username and password
  users    Manage users, including changing password, querying the accessible groups etc
  groups   Manage groups, including creating a group, querying groups, etc
  tasks    Manage tasks, including submitting a task, querying tasks, etc
  workers  Manage workers, including querying workers, cancel workers, etc
  cmd      Run an external command
  quit     Quit the client's interactive mode [aliases: exit]
  help     Print this message or the help of the given subcommand(s)

Options:
      --config <CONFIG>
          The path of the config file
  -c, --coordinator <COORDINATOR_ADDR>
          The address of the coordinator
      --credential-path <CREDENTIAL_PATH>
          The path of the user credential file
  -u, --user <USER>
          The username of the user
  -p, --password <PASSWORD>
          The password of the user
  -i, --interactive
          Enable interactive mode
      --retain
          Whether to retain the previous login state without refreshing the credential
  -h, --help
          Print help
  -V, --version
          Print version
```

To know how each subcommand works, you can run `mito client <subcommand> --help`.
For example, to know how to create a new user, you can run `mito client admin users create --help`:

```txt
Create a new user

Usage: mito client admin users create [OPTIONS] [USERNAME] [PASSWORD]

Arguments:
  [USERNAME]  The username of the user
  [PASSWORD]  The password of the user

Options:
      --admin    Whether to grant the new user as an admin user
  -h, --help     Print help
  -V, --version  Print version
```

For the rest of this section, we will explain the common use cases of the Client on different scenarios.
For the sake of convenience, we will assume that the user is already in interactive mode.
And for the direct executing mode, it only requires adding "mito client" at the front.

## `admin` sub-commands

Input `help admin` to show the help message of the `admin` sub-commands:

```txt
Admin operations, including shutdown the coordinator, chaning user password, etc

Usage: admin <COMMAND>

Commands:
  users     Manage users
  shutdown  Shutdown the coordinator
  groups    Manage groups
  tasks     Manage a task
  workers   Manage a worker
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

The admin operations are only available to admin users.

For example, we can create a new user by running the following command:

```txt
admin users create test_user_name test_user_password
```

We can change password of a user by running:

```txt
admin users change-password test_user_name new_test_user_password
```

## `groups` sub-commands

Input `help groups` to show the help message of the `groups` sub-commands:

```txt
Manage groups, including creating a group, querying groups, etc

Usage: groups <COMMAND>

Commands:
  create       Create a new group
  get          Get the information of a group
  update-user  Update the roles of users to a group
  remove-user  Remove the accessibility of users from a group
  attachments  Query, upload, download or delete an attachment
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

We can manage group related operations with the `groups` sub-commands, such as creating a new group, querying a group, managing users' access to a group, and managing attachments of a group.

We can create a new group by running the following command:

```txt
groups create test_group
```

This will create a group called `test_group` containing the current logged in user.
This user will be granted the `Admin` role to this group to manage it.

We can get the information of a group by running:

```txt
groups get test_group
```

### `attachments` sub-commands

Input `help groups attachments` or `groups attachments -h` to show the help message of the `attachments` sub-commands:

```txt
Query, upload, download or delete an attachment

Usage: groups attachments <COMMAND>

Commands:
  delete    Delete an attachment from a group
  upload    Upload an attachment to a group
  get       Get the metadata of an attachment
  download  Download an attachment of a group
  query     Query attachments subject to the filter
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

This is used to upload, download, delete or query attachments of a group.

For example, to upload an attachment to a group, we can run:

```txt
groups attachments upload -g test_group local.tar.gz attachment_key
```

You can also just run `groups attachments upload local.tar.gz`.
This will directly upload the file to the current group you are in and use the file name as the attachment key.

Also, you can specify the attachment key to be a directory-like string, ending with a `/`. This will smartly upload local file to `attachment_key/local_file_name`.

For example, to upload a file `local.tar.gz` to a directory `dir/` in the group, you can run:

```txt
groups attachments upload -g test_group local.tar.gz dir/
```

This will save the attachment with key `dir/local.tar.gz`.

To download an attachment of a group, you can just:

```txt
groups attachments download -g test_group dir/local.tar.gz
```

We also offer a smart mode to make downloading easier.

You can specify the group_name in the first segment of the attachment key, separated by a `/`, if no group_name specified by `-g`, and we will use the last segment of the attachment key as the local file name if no output path specified by `-o`. For example:

```txt
groups attachments download test_group/dir/local.tar.gz
```

This will download the attachment `dir/local.tar.gz` from group `test_group` and save it as `local.tar.gz` in the current directory.

## `tasks` sub-commands

Input `help tasks` to show the help message of the `tasks` sub-commands:

```txt
Manage tasks, including submitting a task, querying tasks, etc

Usage: tasks <COMMAND>

Commands:
  submit         Submit a task
  get            Get the info of a task
  query          Query tasks subject to the filter
  cancel         Cancel a task
  update-labels  Replace labels of a task
  change         Update the spec of a task
  artifacts      Query, upload, download or delete an artifact
  help           Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

Submit a task to the Coordinator can be as simple as running the following command:

```txt
tasks submit -- echo hello
```

The content after `--` is the command to run on the worker. It will return a UUID to identify the task.

You can also specify the group to submit the task to by using the `-g` option.

The `--labels` are used to mark the task for querying later, it won't affect how the task is fetched ans executed.

The `--tags` are used to define the characteristics of the task, such as its requirements on the Worker.
Only when a Worker's tags are empty or are the subset of the task's tags, the Worker can fetch the task.

You can also set some environment variables for the task by using the `-e` option.

```txt
tasks submit -g test_group -t wireless,4g -l mobile,video -e TEST_KEY=1,TEST_VAL=2 -- echo hello
```

For the output of the task, we allow 3 types of output to be collected:

1. **Result**: Files put under the directory specified by the environment variable `MITO_RESULT_DIR` will be packed into an artifact and uploaded to the Coordinator.
   If the directory is empty, no artifact will be created.
2. **Exec**: Files put under the directory specified by the environment variable `MITO_EXEC_DIR` will be packed into an artifact and uploaded to the Coordinator.
   If the directory is empty, no artifact will be created.
3. **Terminal**: If the `--terminal` option is specified, the standard output and error of the executed task will be collected and uploaded to the Coordinator.
   The terminal output will be stored in a file named `stdout.log` and `stderr.log` respectively in an artifact.

Now, we get a submitted task's information by providing its UUID:

```txt
tasks get e07a2bf2-166d-40b5-8bb6-a78104c072f9
```

Or we can just query a list of tasks with label `mobile`:

```txt
tasks query -l mobile
```

More filter options can be found in the help message by executing `tasks query -h`

For a task, we can also cancel it, update its labels or change its specification to run with its UUID provides. For example:

```txt
tasks cancel e07a2bf2-166d-40b5-8bb6-a78104c072f9
```

This will cancel the task if it is not started yet. It is not allowed to cancel a running or finished task.

To change how the task is executed (i.e., the spec of this task), we can run:

```txt
tasks change e07a2bf2-166d-40b5-8bb6-a78104c072f9 --terminal -- echo world
```

This will alter the task to collect standard output and error when finishes, and execute `echo world` instead of `echo hello`.

We can download the results (a collection of files generated by a task as output) collected by the task as an artifact.

It is easy to download an artifact of a task by providing its UUID. But you also have to specify the output type you want.
There are three types of output: `result`, `exec-log`, and `std-log`. You can also specify the output path to download the artifact to with `-o` argument.

```txt
tasks artifacts download e07a2bf2-166d-40b5-8bb6-a78104c072f9 result
```

## `workers` sub-commands

Input `help workers` to show the help message of the `workers` sub-commands:

```txt
Manage workers, including querying workers, cancel workers, etc

Usage: workers <COMMAND>

Commands:
  cancel        Cancel a worker
  update-tags   Replace tags of a worker
  update-roles  Update the roles of groups to a worker
  remove-roles  Remove the accessibility of groups from a worker
  get           Get information about a worker
  query         Query workers subject to the filter
  help          Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

We can manage a worker, and get relevant information about it with the `workers` sub-commands.

For example, we can stop a worker by running:

```txt
workers cancel b168dbe6-5c44-4529-a3b4-51940d6bb3c5
```

Or we can update the tags of a worker by running:

```txt
workers update-tags b168dbe6-5c44-4529-a3b4-51940d6bb3c5 wired file
```

And we can grant another group `Write` access to this worker (it means the group can submit tasks to this worker) by running:

```txt
workers update-roles b168dbe6-5c44-4529-a3b4-51940d6bb3c5 test_group:admin another_group:write
```

You can perform the opposite action to remove certain groups' access permissions to the Worker using the `remove-roles` subcommand.

## `cmd` sub-commands

Input `help cmd` to show the help message of the `cmd` sub-commands:

```txt
Run an external command

Usage: cmd [OPTIONS] [-- <COMMAND>...]

Arguments:
  [COMMAND]...  The command to run

Options:
  -s, --split  Do not merge the command into one string
  -h, --help   Print help
```

We can use this sub-command to run an external command. For example, to list files in the current directory, we can run:

```
cmd -- ls -hal
```
