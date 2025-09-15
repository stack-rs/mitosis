# Use Manager to manage multiple workers

The Manager provides a convenient way to manage multiple Workers from a single command interface. It allows you to spawn, monitor, and terminate Workers efficiently.

## Overview

The manager component is designed to handle worker lifecycle management, including:

- **Status monitoring**: Check the status of all running workers
- **Worker spawning**: Launch multiple workers with configurable parameters
- **Worker termination**: Kill all running workers at once

## Commands

### Status

Check the status of all running workers:

```bash
mito manager status
```

This command:

- Lists all currently running `mito worker` processes
- Shows detailed process information (PID, CPU usage, memory, etc.)
- Displays the total count of active workers

### Spawn Workers

Launch multiple worker processes:

```bash
mito manager spawn <count> [worker-options]
```

- `count`: Number of workers to spawn (must be greater than 0)
- `worker-options` All available worker options can be passed to the spawn command.

**Example:**

```bash
# Spawn 5 workers with default configuration
mito manager spawn 5

# Spawn 3 workers with custom coordinator and tags
mito manager spawn 3 --coordinator "127.0.0.1:5000" --tags "gpu,cuda"

# Spawn workers with specific groups and file logging
mito manager spawn 2 --groups "batch-processing" --file-log --log-path "/var/log/mito"
```

**Spawning Process:**

- Shows a progress bar during worker creation
- Workers are spawned as detached processes (background execution)
- Each worker runs independently with its own process ID
- After spawning, displays the current total count of running workers

### Kill Workers

Terminate all running worker processes:

```bash
mito manager kill
```

This command:

- Finds all processes matching `mito worker`
- Terminates them using `pkill`
- Confirms successful termination or reports if no workers were found

## Environment Variables

When spawning workers, the manager automatically sets:

- `NO_COLOR=1`: Disables colored output for consistent logging
- `MITO_FILE_LOG_LEVEL`: Controls file logging level, if will inherit the current environment variable value and default to `info` if not set

## Best Practices

1. **Monitor before spawning**: Always check current worker status before spawning new ones
2. **Resource awareness**: Consider system resources when spawning multiple workers
3. **Configuration consistency**: Use consistent worker configurations across spawned instances
4. **Logging strategy**: Enable file logging for persistent worker logs

## Example Workflow

```bash
# Check current worker status
mito manager status

# Spawn 4 workers with GPU tags for ML workloads
mito manager spawn 4 --tags "gpu,ml" --groups "training" --file-log

# Monitor status after spawning
mito manager status

# When done, terminate all workers
mito manager kill
```

## Troubleshooting

**No workers showing in status:**

- Ensure workers are properly spawned
- Check if coordinator is running and accessible
- Verify worker configuration parameters

**Failed to spawn workers:**

- Check system resources (memory, CPU)
- Verify coordinator connectivity
- Ensure proper permissions for process creation

**Workers not terminating:**

- Check for zombie processes: `ps -aux | grep mito`
- Force kill if necessary: `sudo pkill -9 -f "mito worker"`
- Restart the manager if processes are stuck

