# Architecture Overview

This document provides a comprehensive overview of Mitosis's architecture, components, and data flow.

## System Components

### Coordinator

The Coordinator is the central management service that orchestrates the entire Mitosis system. It handles:

- **Task Management**: Receives, validates, and stores task submissions
- **User Authentication**: Manages user sessions and permissions using JWT tokens
- **Group Authorization**: Enforces group-based access controls
- **Worker Registration**: Tracks available workers and their capabilities
- **Scheduling**: Matches tasks with appropriate workers based on groups and tags
- **State Management**: Maintains task execution states and progress tracking
- **Artifact Storage**: Coordinates with S3-compatible storage for task outputs

**Key Dependencies:**

- PostgreSQL for persistent data storage
- S3-compatible storage for artifact management
- Redis (optional) for pub/sub notifications and caching
- Ed25519 key pair for JWT token signing

### Worker

Workers are the execution nodes that run tasks assigned by the Coordinator. Each worker:

- **Task Polling**: Regularly checks for available tasks matching its configuration
- **Environment Isolation**: Provides clean execution environments for tasks
- **Artifact Collection**: Gathers task outputs from designated directories
- **Heartbeat Reporting**: Sends periodic status updates to maintain liveness
- **Tag-based Matching**: Only accepts tasks compatible with its configured tags
- **Group Membership**: Serves tasks from groups it has been granted access to

**Execution Flow:**

1. Poll Coordinator for available tasks
2. Validate task compatibility (groups, tags)
3. Create isolated execution environment
4. Execute task command with configured environment variables
5. Collect artifacts from `MITO_RESULT_DIR`, `MITO_EXEC_DIR`
6. Upload results and update task status

### Client

The Client provides both interactive and programmatic interfaces for users to interact with the system:

- **Interactive Mode**: Shell-like interface for real-time system interaction
- **Batch Mode**: Direct command execution for scripting and automation
- **Task Management**: Submit, query, and manage task execution
- **User Administration**: Create and manage users (admin only)
- **Group Management**: Create groups and manage member permissions
- **Worker Management**: Monitor and control worker nodes
- **Artifact Operations**: Upload group attachments and download task results

## Data Flow

### Task Submission Flow

```
Client → Coordinator → Database
  │         │
  ├─→ Validates user credentials and permissions
  ├─→ Stores task specification in database
  └─→ Returns task UUID to client
```

### Task Execution Flow

```
Worker → Coordinator → Database → S3 Storage
  │         │             │
  ├─→ Polls for tasks based on groups/tags
  ├─→ Updates task status (pending → running → completed/failed)
  └─→ Uploads artifacts and execution logs
```

### Monitoring Flow (with Redis)

```
Coordinator → Redis → Client
     │         │       │
     ├─→ Publishes task status updates
     └─→ Client subscribes to real-time notifications
```

## Access Control Model

### Users and Groups

- Every user automatically gets a group with the same name
- Users can create additional groups and manage membership
- Group roles define access levels: `Read`, `Write`, `Admin`

### Worker Permissions

Workers are configured with group access levels:

- **Write**: Group members can submit tasks to this worker
- **Read**: Group members can query worker status
- **Admin**: Group members can manage worker configuration

### Task Routing

Tasks are routed to workers based on:

1. **Group Membership**: Worker must have access to the task's target group
2. **Tag Compatibility**: Worker tags must be empty or contain all task tags
3. **Availability**: Worker must be active and not at capacity

## Storage Architecture

### Database Schema (PostgreSQL)

- **Users**: Authentication and profile information
- **Groups**: Group definitions and membership
- **Tasks**: Task specifications, state, and metadata
- **Workers**: Worker registration and configuration
- **Artifacts / Attachments**: File metadata and S3 object references

### Object Storage (S3)

- **Task Artifacts**: Results, logs, and execution outputs
- **Group Attachments**: Shared files accessible to group members
- **Bucket Structure**: Organized by groups and artifact types

### Cache Layer (Redis)

- **Session Management**: JWT token validation and user sessions
- **Pub/Sub**: Real-time notifications for task status changes

## Security Model

### Authentication

- JWT tokens signed with Ed25519 private key
- Configurable token expiration (default: 7 days)
- Credential caching for user convenience

### Authorization

- Role-based access control at group level
- API endpoint protection based on user permissions
- Resource isolation between groups

## Scalability Considerations

### Horizontal Scaling

- **Multiple Workers**: Add workers to increase task execution capacity
- **Load Balancing**: Coordinator can handle multiple concurrent clients
- **Database Partitioning**: Tasks and artifacts can be partitioned by group

### Performance Optimization

- **Connection Pooling**: Database connections are pooled and reused
- **Batch Operations**: Multiple tasks can be submitted in batches
- **Async Processing**: Non-blocking I/O throughout the system

### Resource Management

- **Worker Tagging**: Allows targeting tasks to specific hardware capabilities
- **Heartbeat Monitoring**: Automatic worker health checking and cleanup
- **Configurable Timeouts**: Prevents resource leaks from stalled tasks

## Deployment Patterns

### Single-Node Development

- All components on one machine
- Docker Compose for external dependencies
- Suitable for testing and small workloads

### Multi-Node Production

- Coordinator on dedicated server
- Workers distributed across compute nodes
- Shared database and storage infrastructure
- Load balancer for coordinator high availability

