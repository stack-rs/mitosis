# Client SDK

The Mitosis project contains a SDK library (named `netmito`) that you can use to create your own client applications programmatically. The SDK provides a comprehensive Rust API for interacting with Mitosis coordinators.
For Python SDK, please refer to the [Mitosis Python SDK repository](https://github.com/stack-rs/mitosis-python-sdk).

## Installation

Add the following to your `Cargo.toml`:

```toml
[dependencies]
netmito = "0.6.5"
tokio = { version = "1.0", features = ["full"] }
uuid = { version = "1.18", features = ["v4"] }
```

## Basic Usage

### Client Setup

```rust,ignore
use netmito::client::MitoClient;
use netmito::config::{ClientConfig, client::LoginArgs};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create client configuration
    let mut config = ClientConfig::default();
    config.coordinator_addr = "http://localhost:5000".parse()?;

    // Create client instance
    let mut client = MitoClient::setup(config).await?;

    // Login with credentials
    let login_args = LoginArgs {
        username: Some("username".to_string()),
        password: Some("password".to_string()),
        retain: true,
    };
    client.user_login(login_args).await?;

    Ok(())
}
```

### Task Management

#### Submitting Tasks

```rust,ignore
use netmito::config::client::TaskSubmitArgs;

// Submit a simple task
let args = TaskSubmitArgs {
    command: vec!["echo".to_string(), "Hello World".to_string()],
    group: Some("my-group".to_string()),
    tags: vec!["test".to_string()],
    labels: vec!["example".to_string()],
    env: vec!["MY_VAR=value".to_string()],
    terminal: true, // Capture stdout/stderr
    ..Default::default()
};

let task_id = client.task_submit(args).await?;
println!("Submitted task: {}", task_id);
```

#### Querying Tasks

```rust,ignore
use netmito::config::client::TaskQueryArgs;

// Query all tasks
let args = TaskQueryArgs::default();
let tasks = client.task_query(args).await?;

// Query tasks with filters
let args = TaskQueryArgs {
    labels: Some(vec!["example".to_string()]),
    status: Some("completed".to_string()),
    limit: Some(10),
    ..Default::default()
};
let filtered_tasks = client.task_query(args).await?;
```

#### Getting Task Details

```rust,ignore
use uuid::Uuid;

let task_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")?;
let task_info = client.task_get(task_id).await?;

println!("Task status: {:?}", task_info.exec_state);
println!("Created at: {}", task_info.created_at);
```

#### Downloading Artifacts

```rust,ignore
use netmito::config::client::TaskArtifactDownloadArgs;

let args = TaskArtifactDownloadArgs {
    task_id,
    artifact_type: "result".to_string(), // "result", "exec-log", or "std-log"
    output_path: Some("./task-results.tar.gz".to_string()),
    ..Default::default()
};

client.task_artifact_download(args).await?;
```

### User and Group Management

#### Creating Users (Admin only)

```rust,ignore
use netmito::config::client::AdminCreateUserArgs;

let args = AdminCreateUserArgs {
    username: Some("new_user".to_string()),
    password: Some("secure_password".to_string()),
    admin: false,
};

client.admin_create_user(args).await?;
```

#### Group Operations

```rust,ignore
use netmito::config::client::{GroupCreateArgs, GroupUpdateUserArgs};

// Create a group
let args = GroupCreateArgs {
    group_name: "research-team".to_string(),
};
client.group_create(args).await?;

// Add user to group
let args = GroupUpdateUserArgs {
    group_name: "research-team".to_string(),
    username: "researcher".to_string(),
    role: "write".to_string(), // "read", "write", or "admin"
};
client.group_update_user(args).await?;
```

### Worker Management

#### Querying Workers

```rust,ignore
use netmito::config::client::WorkerQueryArgs;

let args = WorkerQueryArgs {
    group: Some("my-group".to_string()),
    tags: Some(vec!["gpu".to_string()]),
    ..Default::default()
};

let workers = client.worker_query(args).await?;
for worker in workers {
    println!("Worker {} is {}", worker.id, worker.status);
}
```

#### Managing Worker Tags

```rust,ignore
use netmito::config::client::WorkerUpdateTagsArgs;

let worker_id = Uuid::parse_str("worker-uuid-here")?;
let args = WorkerUpdateTagsArgs {
    worker_id,
    tags: vec!["gpu".to_string(), "cuda".to_string()],
};

client.worker_update_tags(args).await?;
```

### Group Attachments

#### Uploading Files

```rust,ignore
use netmito::config::client::GroupAttachmentUploadArgs;

let args = GroupAttachmentUploadArgs {
    group_name: Some("my-group".to_string()),
    local_path: "./dataset.tar.gz".to_string(),
    attachment_key: Some("datasets/experiment-1.tar.gz".to_string()),
    ..Default::default()
};

client.group_attachment_upload(args).await?;
```

#### Downloading Files

```rust,ignore
use netmito::config::client::GroupAttachmentDownloadArgs;

let args = GroupAttachmentDownloadArgs {
    group_name: Some("my-group".to_string()),
    attachment_key: "datasets/experiment-1.tar.gz".to_string(),
    output_path: Some("./downloaded-dataset.tar.gz".to_string()),
    ..Default::default()
};

client.group_attachment_download(args).await?;
```

### Batch Operations (v0.6.5+)

Starting from version 0.6.5, the SDK supports batch operations for improved efficiency when working with multiple resources:

#### Batch Task Submission

Submit multiple tasks at once:

```rust,ignore
use netmito::config::client::TaskSubmitArgs;
use netmito::schema::BatchTaskSubmitReq;

let tasks = vec![
    TaskSubmitArgs {
        command: vec!["echo".to_string(), "task1".to_string()],
        group: Some("my-group".to_string()),
        ..Default::default()
    },
    TaskSubmitArgs {
        command: vec!["echo".to_string(), "task2".to_string()],
        group: Some("my-group".to_string()),
        ..Default::default()
    },
];

let batch_req = BatchTaskSubmitReq { tasks };
let task_ids = client.batch_submit_tasks(batch_req).await?;
```

#### Batch Task Cancellation

Cancel multiple tasks by UUID or by filter:

```rust,ignore
use uuid::Uuid;
use netmito::schema::BatchCancelByUuidsReq;

// Cancel by specific UUIDs
let task_ids = vec![
    Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")?,
    Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001")?,
];
let req = BatchCancelByUuidsReq { uuids: task_ids };
client.tasks_batch_cancel_by_uuids(req).await?;

// Or cancel by filter
use netmito::config::client::TaskQueryArgs;
let filter = TaskQueryArgs {
    labels: Some(vec!["test".to_string()]),
    status: Some("pending".to_string()),
    ..Default::default()
};
client.tasks_batch_cancel(filter).await?;
```

#### Batch Artifact Download

Download multiple artifacts at once:

```rust,ignore
use netmito::config::client::{TaskQueryArgs, TaskArtifactDownloadArgs};

// Download artifacts by filter
let filter = TaskQueryArgs {
    labels: Some(vec!["experiment-1".to_string()]),
    status: Some("completed".to_string()),
    ..Default::default()
};
let download_args = TaskArtifactDownloadArgs {
    artifact_type: "result".to_string(),
    output_path: Some("./results/".to_string()),
    ..Default::default()
};
client.batch_download_artifacts_by_filter(filter, download_args).await?;

// Or download specific artifacts by task IDs
use netmito::schema::BatchDownloadArtifactsByListReq;
let req = BatchDownloadArtifactsByListReq {
    task_ids: vec![task_id1, task_id2],
    artifact_type: "result".to_string(),
};
client.batch_download_artifacts_by_list(req, "./results/".to_string()).await?;
```

#### Batch Attachment Operations

Download or delete multiple group attachments:

```rust,ignore
use netmito::config::client::GroupAttachmentQueryArgs;
use netmito::schema::BatchDownloadAttachmentsByListReq;

// Download attachments by filter
let filter = GroupAttachmentQueryArgs {
    group_name: Some("my-group".to_string()),
    key: Some("datasets/".to_string()), // prefix matching
    ..Default::default()
};
client.batch_download_attachments_by_filter(filter, "./downloads/".to_string()).await?;

// Delete attachments by list
let keys = vec!["old-data-1.tar.gz".to_string(), "old-data-2.tar.gz".to_string()];
let req = BatchDownloadAttachmentsByListReq {
    group_name: "my-group".to_string(),
    keys,
};
client.batch_delete_attachments_by_list(req).await?;
```

#### Batch Worker Cancellation

Cancel multiple workers at once:

```rust,ignore
use netmito::schema::BatchCancelByUuidsReq;

// Cancel workers by UUIDs
let worker_ids = vec![worker_uuid1, worker_uuid2];
let req = BatchCancelByUuidsReq { uuids: worker_ids };
client.workers_batch_cancel_by_uuids(req).await?;

// Or cancel by filter
use netmito::config::client::WorkerQueryArgs;
let filter = WorkerQueryArgs {
    tags: Some(vec!["deprecated".to_string()]),
    ..Default::default()
};
client.workers_batch_cancel(filter).await?;
```

## Advanced Usage

### Configuration Options

```rust,ignore
use netmito::config::ClientConfig;
use url::Url;
use figment::value::magic::RelativePathBuf;

let config = ClientConfig {
    coordinator_addr: Url::parse("https://coordinator.example.com")?,
    credential_path: Some(RelativePathBuf::from("/path/to/credentials")),
    user: Some("api-user".to_string()),
    password: Some("api-password".to_string()),
    retain: true, // Keep existing login state
};
```

### Error Handling

```rust,ignore
use netmito::error::{Error, ApiError, AuthError};

match client.task_get(task_id).await {
    Ok(task) => println!("Task found: {:?}", task),
    Err(Error::ApiError(ApiError::NotFound(_))) => println!("Task not found"),
    Err(Error::ApiError(ApiError::AuthError(AuthError::PermissionDenied))) => println!("Access denied"),
    Err(e) => println!("Other error: {}", e),
}
```

### Async Patterns

```rust,ignore
use futures::future::join_all;

// Submit multiple tasks concurrently
let tasks: Vec<_> = (0..10).map(|i| {
    let mut client = client.clone();
    let args = TaskSubmitArgs {
        command: vec!["sleep".to_string(), i.to_string()],
        ..Default::default()
    };
    async move { client.task_submit(args).await }
}).collect();

let results = join_all(tasks).await;
for result in results {
    match result {
        Ok(task_id) => println!("Submitted: {}", task_id),
        Err(e) => println!("Failed: {}", e),
    }
}
```

### Real-time Task Monitoring

```rust,ignore
use netmito::client::redis::MitoRedisClient;
use netmito::entity::state::TaskExecState;
use tokio::time::{interval, Duration};

// Set up Redis client for real-time updates
let mut redis_client = client.get_redis_client().await?;

// Poll task status periodically
let mut interval = interval(Duration::from_secs(5));
loop {
    interval.tick().await;

    let task = client.task_get(task_id).await?;
    println!("Task status: {:?}", task.exec_state);

    if matches!(task.exec_state, TaskExecState::Completed | TaskExecState::Failed) {
        break;
    }
}
```

## Integration Examples

### CI/CD Pipeline Integration

```rust,ignore
pub struct MitosisPipeline {
    client: MitoClient,
}

impl MitosisPipeline {
    pub async fn run_test_suite(&mut self, commit_hash: &str) -> Result<bool, Box<dyn std::error::Error>> {
        // Submit test tasks
        let test_args = TaskSubmitArgs {
            command: vec!["cargo".to_string(), "test".to_string(), "--release".to_string()],
            group: Some("ci-runners".to_string()),
            tags: vec!["rust".to_string(), "testing".to_string()],
            labels: vec![format!("commit:{}", commit_hash)],
            terminal: true,
            ..Default::default()
        };

        let task_id = self.client.task_submit(test_args).await?;

        // Wait for completion
        let result = self.wait_for_task(task_id).await?;

        // Download test results
        let artifact_args = TaskArtifactDownloadArgs {
            task_id,
            artifact_type: "std-log".to_string(),
            output_path: Some(format!("test-results-{}.tar.gz", commit_hash)),
            ..Default::default()
        };

        self.client.task_artifact_download(artifact_args).await?;

        Ok(result.exec_state == TaskExecState::Completed)
    }
}
```

### Research Computing Workflow

```rust,ignore
pub struct ResearchWorkflow {
    client: MitoClient,
    group: String,
}

impl ResearchWorkflow {
    pub async fn run_parameter_sweep(&mut self, parameters: Vec<Vec<String>>) -> Result<Vec<Uuid>, Box<dyn std::error::Error>> {
        let mut task_ids = Vec::new();

        for (i, params) in parameters.into_iter().enumerate() {
            let args = TaskSubmitArgs {
                command: vec!["python3".to_string(), "experiment.py".to_string()],
                env: params.into_iter().map(|p| format!("PARAM_{}", p)).collect(),
                group: Some(self.group.clone()),
                labels: vec![format!("sweep:experiment-{}", i)],
                tags: vec!["gpu".to_string(), "python".to_string()],
                terminal: true,
                ..Default::default()
            };

            let task_id = self.client.task_submit(args).await?;
            task_ids.push(task_id);
        }

        Ok(task_ids)
    }

    pub async fn collect_results(&mut self, task_ids: Vec<Uuid>) -> Result<(), Box<dyn std::error::Error>> {
        for task_id in task_ids {
            let args = TaskArtifactDownloadArgs {
                task_id,
                artifact_type: "result".to_string(),
                output_path: Some(format!("results/{}.tar.gz", task_id)),
                ..Default::default()
            };

            self.client.task_artifact_download(args).await?;
        }

        Ok(())
    }
}
```

## API Reference

For complete API documentation, including all available methods, configuration options, and error types, please refer to the [API documentation](https://docs.rs/netmito).
