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

We recommend using the interactive mode for most operations, as it provides a more user-friendly experience.

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
  create  Create a new user or group
  get     Get the info of a task, artifact, attachment, or a list of tasks subject to the filters
  submit  Submit a task
  upload  Upload an attachment to a group
  cancel  Cancel a worker or a task
  quit    Quit the client's interactive mode
  help    Print this message or the help of the given subcommand(s)

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
