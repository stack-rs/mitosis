# Netmito Logging & Tracing System Improvement Plan

**Version:** 1.0
**Date:** 2025-11-22
**Status:** Proposal for Review

---

## Executive Summary

This document outlines a comprehensive plan to improve the netmito library's logging and tracing system. The improvements will make the system more flexible, observable, and production-ready by:

1. **Eliminating code duplication** across 4+ initialization points
2. **Enabling flexible output combinations** (console AND/OR file AND/OR OpenTelemetry)
3. **Adding OpenTelemetry support** for distributed tracing
4. **Improving observability** with structured logging and semantic conventions
5. **Maintaining backward compatibility** with existing configurations

---

## Table of Contents

1. [Current State Analysis](#current-state-analysis)
2. [High-Level Architecture](#high-level-architecture)
3. [Detailed Design](#detailed-design)
4. [Implementation Phases](#implementation-phases)
5. [Migration Strategy](#migration-strategy)
6. [Testing Strategy](#testing-strategy)
7. [Documentation Updates](#documentation-updates)
8. [Appendices](#appendices)

---

## Current State Analysis

### Existing Implementation

**Dependencies:**
```toml
tracing = "0.1.41"
tracing-subscriber = { version = "0.3.20", features = ["env-filter"] }
tracing-appender = "0.2.3"
```

**Initialization Locations:**
- `netmito/src/coordinator.rs:25-31` + `config/coordinator.rs:368-432`
- `netmito/src/worker.rs:226-232` + `config/worker.rs:253-394`
- `netmito/src/client/mod.rs:32-38`
- `netmito/src/manager.rs:13-19`

**Current Configuration Options:**
```rust
// CoordinatorConfig / WorkerConfig
pub struct Config {
    file_log: bool,                          // Enable file logging
    log_path: Option<RelativePathBuf>,       // Custom log path
    shared_log: bool,                        // Workers only: shared log file
}
```

**Environment Variables:**
- `RUST_LOG` - Console log level (default: `netmito=info`)
- `MITO_FILE_LOG_LEVEL` - File log level (default: `info`)

### Problems Identified

1. **Code Duplication** - 260+ lines of duplicated tracing setup logic
2. **Binary Configuration** - File logging is either on or off, no granular control
3. **No Layer Composition** - Cannot combine outputs flexibly (e.g., console + file + OTLP)
4. **Missing OpenTelemetry** - No distributed tracing support
5. **Limited Observability** - Minimal use of structured fields and spans
6. **Hardcoded Defaults** - Rotation strategy, max files not configurable
7. **No Service Identity** - Missing resource attributes (service.name, version, instance.id)

---

## High-Level Architecture

### Core Concepts

#### 1. Centralized Tracing Module

Create a new `netmito/src/tracing.rs` module that:
- Provides a **builder pattern** for layer composition
- Supports **conditional layer activation**
- Handles **type erasure** for dynamic layer configuration
- Manages **lifecycle** (guards, shutdown)

```
┌─────────────────────────────────────────────────────────┐
│                    TracingBuilder                        │
│  ┌───────────────────────────────────────────────────┐  │
│  │  Service Metadata (name, version, instance_id)   │  │
│  └───────────────────────────────────────────────────┘  │
│                                                           │
│  ┌───────────────┐ ┌────────────┐ ┌──────────────────┐  │
│  │ Console Layer │ │ File Layer │ │ OpenTelemetry   │  │
│  │   (optional)  │ │ (optional) │ │ Layer (optional) │  │
│  └───────────────┘ └────────────┘ └──────────────────┘  │
│                                                           │
│  .with_console(config)                                   │
│  .with_file(config)                                      │
│  .with_opentelemetry(config)                             │
│  .build() → TracingGuard                                 │
└─────────────────────────────────────────────────────────┘
```

#### 2. Layer Architecture

The tracing-subscriber registry supports multiple concurrent layers:

```rust
Registry::default()
    .with(console_layer.with_filter(console_filter))    // Stdout/stderr
    .with(file_layer.with_filter(file_filter))          // Rolling files
    .with(otel_layer.with_filter(otel_filter))          // OTLP export
    .init()
```

**Key principle:** Each layer operates independently with its own filter.

#### 3. Configuration Schema

New flexible configuration structure:

```rust
pub struct TracingConfig {
    // Service identity
    service_name: Option<String>,
    service_version: Option<String>,
    service_instance_id: Option<String>,

    // Console output
    console: ConsoleConfig,

    // File output
    file: FileConfig,

    // OpenTelemetry
    opentelemetry: OpenTelemetryConfig,

    // Advanced options
    log_spans: bool,
    log_thread_ids: bool,
    log_thread_names: bool,
}
```

#### 4. OpenTelemetry Integration

```
┌──────────────┐      ┌─────────────────┐      ┌──────────────┐
│   Netmito    │ ---> │ tracing-        │ ---> │ OTLP         │
│   (tracing)  │      │ opentelemetry   │      │ Exporter     │
└──────────────┘      └─────────────────┘      └──────────────┘
                                                       │
                              ┌────────────────────────┴─────────────┐
                              │                                      │
                         ┌────▼─────┐                         ┌─────▼─────┐
                         │  Jaeger  │                         │ Collector │
                         │  (local) │                         │  (prod)   │
                         └──────────┘                         └───────────┘
```

**Resource Attributes:**
- `service.name` = "mitosis-coordinator" | "mitosis-worker" | "mitosis-manager"
- `service.version` = crate version from Cargo.toml
- `service.instance.id` = worker UUID or coordinator bind address
- `mitosis.component` = "coordinator" | "worker" | "manager" | "client"

---

## Detailed Design

### Phase 1: Core Tracing Module

#### File: `netmito/src/tracing.rs`

**Public API:**

```rust
/// Builder for configuring the tracing system
pub struct TracingBuilder {
    service_name: String,
    service_version: String,
    service_instance_id: Option<String>,
    console_config: Option<ConsoleConfig>,
    file_config: Option<FileConfig>,
    otel_config: Option<OpenTelemetryConfig>,
    log_spans: bool,
    log_thread_ids: bool,
}

impl TracingBuilder {
    /// Create a new builder with service metadata
    pub fn new(service_name: impl Into<String>) -> Self;

    /// Set service version (default: from CARGO_PKG_VERSION)
    pub fn with_version(mut self, version: impl Into<String>) -> Self;

    /// Set unique service instance ID
    pub fn with_instance_id(mut self, id: impl Into<String>) -> Self;

    /// Enable console output
    pub fn with_console(mut self, config: ConsoleConfig) -> Self;

    /// Enable file output
    pub fn with_file(mut self, config: FileConfig) -> Self;

    /// Enable OpenTelemetry
    pub fn with_opentelemetry(mut self, config: OpenTelemetryConfig) -> Self;

    /// Show span timings in logs
    pub fn with_span_events(mut self, enabled: bool) -> Self;

    /// Show thread IDs in logs
    pub fn with_thread_ids(mut self, enabled: bool) -> Self;

    /// Build and initialize the tracing subscriber
    pub fn build(self) -> Result<TracingGuard>;
}

/// Guard that maintains tracing subscriber lifecycle
pub struct TracingGuard {
    subscriber_guard: Option<DefaultGuard>,
    file_guard: Option<WorkerGuard>,
    #[cfg(feature = "opentelemetry")]
    otel_provider: Option<TracerProvider>,
}

impl Drop for TracingGuard {
    fn drop(&mut self) {
        // Graceful shutdown: flush buffers, shutdown OTLP exporter
    }
}
```

**Configuration Structures:**

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ConsoleConfig {
    /// Enable console output (default: true)
    pub enabled: bool,

    /// Log level filter (default: from RUST_LOG or "netmito=info")
    pub level: Option<String>,

    /// Show target module path (default: true)
    pub with_target: bool,

    /// Use ANSI colors (default: auto-detect TTY)
    pub with_ansi: Option<bool>,

    /// Compact vs. full format (default: full)
    pub compact: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileConfig {
    /// Enable file output (default: false)
    pub enabled: bool,

    /// Log level filter (default: from MITO_FILE_LOG_LEVEL or "info")
    pub level: Option<String>,

    /// Custom log file path
    pub path: Option<PathBuf>,

    /// Rotation strategy
    pub rotation: RotationStrategy,

    /// Maximum number of rotated files to keep
    pub max_files: Option<usize>,

    /// Add worker ID prefix (workers only)
    pub worker_prefix: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub enum RotationStrategy {
    Never,
    Minutely,
    Hourly,
    Daily,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg(feature = "opentelemetry")]
pub struct OpenTelemetryConfig {
    /// Enable OpenTelemetry (default: false)
    pub enabled: bool,

    /// OTLP endpoint (e.g., "http://localhost:4317")
    pub endpoint: String,

    /// Protocol: grpc or http/protobuf
    pub protocol: OtlpProtocol,

    /// Custom headers for authentication
    pub headers: Option<HashMap<String, String>>,

    /// Batch span processor timeout
    pub timeout: Option<Duration>,

    /// Override service name
    pub service_name_override: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg(feature = "opentelemetry")]
pub enum OtlpProtocol {
    Grpc,
    HttpProtobuf,
}
```

**Default Implementations:**

```rust
impl Default for ConsoleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level: None, // Will use RUST_LOG or "netmito=info"
            with_target: true,
            with_ansi: None, // Auto-detect
            compact: false,
        }
    }
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            level: None, // Will use MITO_FILE_LOG_LEVEL or "info"
            path: None,  // Will use platform cache dir
            rotation: RotationStrategy::Daily,
            max_files: Some(7), // Keep 1 week
            worker_prefix: None,
        }
    }
}
```

#### Internal Implementation Details

**Layer Composition with Type Erasure:**

```rust
use tracing_subscriber::{
    layer::SubscriberExt,
    util::SubscriberInitExt,
    Layer, Registry,
};

impl TracingBuilder {
    pub fn build(self) -> Result<TracingGuard> {
        let mut layers = Vec::new();
        let mut file_guard = None;

        // Console layer
        if let Some(console_cfg) = self.console_config {
            if console_cfg.enabled {
                let layer = self.build_console_layer(console_cfg)?;
                layers.push(layer.boxed());
            }
        }

        // File layer
        if let Some(file_cfg) = self.file_config {
            if file_cfg.enabled {
                let (layer, guard) = self.build_file_layer(file_cfg)?;
                layers.push(layer.boxed());
                file_guard = Some(guard);
            }
        }

        // OpenTelemetry layer
        #[cfg(feature = "opentelemetry")]
        if let Some(otel_cfg) = self.otel_config {
            if otel_cfg.enabled {
                let layer = self.build_otel_layer(otel_cfg)?;
                layers.push(layer.boxed());
            }
        }

        // Combine all layers
        let subscriber = layers.into_iter().fold(
            Registry::default().boxed(),
            |subscriber, layer| subscriber.with(layer).boxed()
        );

        let subscriber_guard = subscriber.set_default();

        Ok(TracingGuard {
            subscriber_guard: Some(subscriber_guard),
            file_guard,
            #[cfg(feature = "opentelemetry")]
            otel_provider: self.otel_provider,
        })
    }

    fn build_console_layer(
        &self,
        config: ConsoleConfig,
    ) -> Result<impl Layer<Registry>> {
        let filter = config.level
            .as_ref()
            .and_then(|level| EnvFilter::try_new(level).ok())
            .or_else(|| EnvFilter::try_from_default_env().ok())
            .unwrap_or_else(|| "netmito=info".into());

        let layer = tracing_subscriber::fmt::layer()
            .with_target(config.with_target)
            .with_thread_ids(self.log_thread_ids)
            .with_ansi(config.with_ansi.unwrap_or_else(atty::is));

        let layer = if config.compact {
            layer.compact().boxed()
        } else {
            layer.boxed()
        };

        Ok(layer.with_filter(filter))
    }

    fn build_file_layer(
        &self,
        config: FileConfig,
    ) -> Result<(impl Layer<Registry>, WorkerGuard)> {
        // Determine log path
        let log_path = config.path.clone().or_else(|| {
            dirs::cache_dir().map(|mut p| {
                p.push("mitosis");
                p.push(self.service_name.as_str());
                p
            })
        }).ok_or_else(|| Error::ConfigError("Cannot determine log path"))?;

        // Create appender with rotation
        let file_appender = match config.rotation {
            RotationStrategy::Never => {
                let dir = log_path.parent().ok_or(...)?;
                let file = log_path.file_name().ok_or(...)?;
                tracing_appender::rolling::never(dir, file)
            },
            RotationStrategy::Daily => {
                let prefix = self.instance_id_or_default();
                tracing_appender::rolling::daily(log_path, format!("{}.log", prefix))
            },
            RotationStrategy::Hourly => {
                let prefix = self.instance_id_or_default();
                tracing_appender::rolling::hourly(log_path, format!("{}.log", prefix))
            },
            // ... other strategies
        };

        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        // Wrap with worker prefix if configured
        let writer: Box<dyn io::Write + Send> = if let Some(prefix) = config.worker_prefix {
            Box::new(WorkerIdWriter::new(non_blocking, prefix))
        } else {
            Box::new(non_blocking)
        };

        let filter = config.level
            .as_ref()
            .and_then(|level| EnvFilter::try_new(level).ok())
            .or_else(|| EnvFilter::try_from_env("MITO_FILE_LOG_LEVEL").ok())
            .unwrap_or_else(|| "info".into());

        let layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_filter(filter);

        Ok((layer, guard))
    }

    #[cfg(feature = "opentelemetry")]
    fn build_otel_layer(
        &mut self,
        config: OpenTelemetryConfig,
    ) -> Result<impl Layer<Registry>> {
        use opentelemetry::{global, KeyValue};
        use opentelemetry_otlp::WithExportConfig;
        use opentelemetry_sdk::{
            trace::{self, Sampler},
            Resource,
        };

        // Build resource attributes
        let mut resource_attrs = vec![
            KeyValue::new("service.name", self.service_name.clone()),
            KeyValue::new("service.version", self.service_version.clone()),
        ];

        if let Some(instance_id) = &self.service_instance_id {
            resource_attrs.push(KeyValue::new("service.instance.id", instance_id.clone()));
        }

        let resource = Resource::new(resource_attrs);

        // Build OTLP exporter
        let exporter = opentelemetry_otlp::new_exporter()
            .tonic() // or .http() based on protocol
            .with_endpoint(&config.endpoint)
            .with_timeout(config.timeout.unwrap_or(Duration::from_secs(10)));

        // Build tracer provider
        let tracer_provider = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(exporter)
            .with_trace_config(
                trace::config()
                    .with_sampler(Sampler::AlwaysOn)
                    .with_resource(resource)
            )
            .install_batch(opentelemetry_sdk::runtime::Tokio)?;

        // Store provider for graceful shutdown
        self.otel_provider = Some(tracer_provider.clone());

        // Create tracing layer
        let tracer = tracer_provider.tracer("netmito");
        let layer = tracing_opentelemetry::layer().with_tracer(tracer);

        Ok(layer)
    }
}
```

### Phase 2: Configuration Integration

#### Update: `netmito/src/config/mod.rs`

```rust
pub mod tracing_config;

pub use tracing_config::{
    TracingConfig, ConsoleConfig, FileConfig,
    OpenTelemetryConfig, RotationStrategy
};

// Keep existing TracingGuard for backward compatibility
// but it now wraps the new crate::tracing::TracingGuard
#[deprecated(since = "0.7.0", note = "Use crate::tracing::TracingGuard")]
pub struct TracingGuard {
    inner: crate::tracing::TracingGuard,
}
```

#### New File: `netmito/src/config/tracing_config.rs`

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TracingConfig {
    /// Console output configuration
    pub console: ConsoleConfig,

    /// File output configuration
    pub file: FileConfig,

    /// OpenTelemetry configuration
    #[cfg(feature = "opentelemetry")]
    pub opentelemetry: OpenTelemetryConfig,

    /// Service name override
    pub service_name: Option<String>,

    /// Service version override
    pub service_version: Option<String>,

    /// Service instance ID override
    pub service_instance_id: Option<String>,

    /// Show span enter/exit events
    pub log_spans: bool,

    /// Show thread IDs
    pub log_thread_ids: bool,

    /// Show thread names
    pub log_thread_names: bool,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            console: ConsoleConfig::default(),
            file: FileConfig::default(),
            #[cfg(feature = "opentelemetry")]
            opentelemetry: OpenTelemetryConfig::default(),
            service_name: None,
            service_version: None,
            service_instance_id: None,
            log_spans: false,
            log_thread_ids: false,
            log_thread_names: false,
        }
    }
}

// ConsoleConfig, FileConfig, OpenTelemetryConfig as defined above
```

#### Update: `netmito/src/config/coordinator.rs`

Add `TracingConfig` field:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoordinatorConfig {
    // ... existing fields ...

    /// Tracing configuration (new)
    #[serde(default)]
    pub tracing: TracingConfig,

    /// Deprecated: Use tracing.file.enabled
    #[deprecated(since = "0.7.0", note = "Use tracing.file.enabled")]
    #[serde(default)]
    pub file_log: bool,

    /// Deprecated: Use tracing.file.path
    #[deprecated(since = "0.7.0", note = "Use tracing.file.path")]
    pub log_path: Option<RelativePathBuf>,
}

impl CoordinatorConfig {
    /// New unified setup method
    pub fn setup_tracing(&self) -> crate::error::Result<crate::tracing::TracingGuard> {
        let service_name = self.tracing.service_name
            .clone()
            .unwrap_or_else(|| "mitosis-coordinator".to_string());

        let service_version = self.tracing.service_version
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

        let instance_id = self.bind.to_string();

        let mut builder = crate::tracing::TracingBuilder::new(service_name)
            .with_version(service_version)
            .with_instance_id(instance_id)
            .with_span_events(self.tracing.log_spans)
            .with_thread_ids(self.tracing.log_thread_ids);

        // Console layer
        if self.tracing.console.enabled {
            builder = builder.with_console(self.tracing.console.clone());
        }

        // File layer (with backward compatibility)
        let file_config = self.migrate_file_config();
        if file_config.enabled {
            builder = builder.with_file(file_config);
        }

        // OpenTelemetry layer
        #[cfg(feature = "opentelemetry")]
        if self.tracing.opentelemetry.enabled {
            builder = builder.with_opentelemetry(self.tracing.opentelemetry.clone());
        }

        builder.build()
    }

    /// Migrate old file_log/log_path config to new format
    fn migrate_file_config(&self) -> FileConfig {
        let mut config = self.tracing.file.clone();

        // Backward compatibility: if old file_log is set, override new config
        #[allow(deprecated)]
        if self.file_log {
            config.enabled = true;
        }

        #[allow(deprecated)]
        if let Some(ref path) = self.log_path {
            config.path = Some(path.relative().to_path_buf());
        }

        config
    }

    /// Deprecated: Use setup_tracing()
    #[deprecated(since = "0.7.0", note = "Use setup_tracing()")]
    pub fn setup_tracing_subscriber(&self) -> crate::error::Result<TracingGuard> {
        // Legacy compatibility wrapper
        self.setup_tracing().map(|guard| TracingGuard { inner: guard })
    }
}
```

#### Update: `netmito/src/config/worker.rs`

Similar changes as coordinator, plus worker-specific handling:

```rust
impl WorkerConfig {
    pub fn setup_tracing<T, U>(&self, worker_id: U) -> crate::error::Result<crate::tracing::TracingGuard>
    where
        T: std::fmt::Display,
        U: Into<T>,
    {
        let id = worker_id.into();
        let id_str = id.to_string();

        let service_name = self.tracing.service_name
            .clone()
            .unwrap_or_else(|| "mitosis-worker".to_string());

        let mut builder = crate::tracing::TracingBuilder::new(service_name)
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_instance_id(id_str.clone());

        // ... similar to coordinator ...

        // File layer with worker-specific logic
        let mut file_config = self.migrate_file_config();
        if file_config.enabled {
            // Add worker ID prefix if shared_log is enabled
            #[allow(deprecated)]
            if self.shared_log {
                file_config.worker_prefix = Some(id_str);
            }
            builder = builder.with_file(file_config);
        }

        builder.build()
    }
}
```

### Phase 3: OpenTelemetry Support

#### Add Dependencies to `netmito/Cargo.toml`

```toml
[features]
default = []
opentelemetry = [
    "dep:opentelemetry",
    "dep:opentelemetry-otlp",
    "dep:opentelemetry_sdk",
    "dep:tracing-opentelemetry",
]

[dependencies]
# ... existing dependencies ...

# OpenTelemetry (optional)
opentelemetry = { version = "0.28", optional = true }
opentelemetry-otlp = { version = "0.28", features = ["grpc-tonic"], optional = true }
opentelemetry_sdk = { version = "0.28", features = ["rt-tokio"], optional = true }
tracing-opentelemetry = { version = "0.28", optional = true }
```

#### Conditional Compilation

Use `#[cfg(feature = "opentelemetry")]` throughout:

```rust
#[cfg(feature = "opentelemetry")]
use opentelemetry::{global, KeyValue};

#[cfg(feature = "opentelemetry")]
impl TracingBuilder {
    fn build_otel_layer(&mut self, config: OpenTelemetryConfig) -> Result<...> {
        // Implementation
    }
}
```

### Phase 4: Enhanced Instrumentation

#### Add `#[instrument]` to Key Functions

**Example: Worker Task Execution**

```rust
use tracing::{info, instrument, error};

#[instrument(
    name = "worker.execute_task",
    skip(pool, payload),
    fields(
        task.id = %task_id,
        task.type = %task_type,
        worker.id = %worker_id,
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
    ),
    level = "info"
)]
pub async fn execute_task(
    pool: &DbPool,
    worker_id: Uuid,
    task_id: Uuid,
    task_type: &str,
    payload: Vec<u8>,
) -> Result<TaskOutput> {
    let span = tracing::Span::current();

    info!(
        payload.size = payload.len(),
        "Starting task execution"
    );

    let result = match task_type {
        "compute" => execute_compute_task(payload).await,
        "io" => execute_io_task(payload).await,
        _ => {
            error!("Unknown task type");
            span.record("otel.status_code", "ERROR");
            return Err(Error::UnknownTaskType);
        }
    };

    match result {
        Ok(output) => {
            span.record("otel.status_code", "OK");
            info!("Task completed successfully");
            Ok(output)
        }
        Err(e) => {
            span.record("otel.status_code", "ERROR");
            error!(error = %e, "Task execution failed");
            Err(e)
        }
    }
}
```

**Example: API Handler with Structured Fields**

```rust
#[instrument(
    name = "api.create_task",
    skip(state, payload),
    fields(
        http.method = "POST",
        http.route = "/api/tasks",
        user.id = tracing::field::Empty,
        task.id = tracing::field::Empty,
    )
)]
async fn create_task(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Json(payload): Json<CreateTaskRequest>,
) -> Result<Json<Task>, ApiError> {
    let span = tracing::Span::current();
    span.record("user.id", user.id.to_string());

    info!(
        task.type = %payload.task_type,
        task.priority = payload.priority,
        "Creating new task"
    );

    let task_id = Uuid::new_v4();
    span.record("task.id", task_id.to_string());

    // ... implementation ...

    Ok(Json(task))
}
```

#### Define Semantic Field Names

Create `netmito/src/tracing/fields.rs`:

```rust
//! Semantic field name constants for consistent logging

/// Task-related fields
pub mod task {
    pub const ID: &str = "task.id";
    pub const TYPE: &str = "task.type";
    pub const STATE: &str = "task.state";
    pub const PRIORITY: &str = "task.priority";
    pub const WORKER_ID: &str = "task.worker_id";
}

/// Worker-related fields
pub mod worker {
    pub const ID: &str = "worker.id";
    pub const STATE: &str = "worker.state";
    pub const CAPACITY: &str = "worker.capacity";
}

/// User-related fields
pub mod user {
    pub const ID: &str = "user.id";
    pub const NAME: &str = "user.name";
}

/// HTTP-related fields
pub mod http {
    pub const METHOD: &str = "http.method";
    pub const ROUTE: &str = "http.route";
    pub const STATUS_CODE: &str = "http.status_code";
    pub const CLIENT_IP: &str = "http.client_ip";
}

/// OpenTelemetry standard fields
pub mod otel {
    pub const KIND: &str = "otel.kind";
    pub const STATUS_CODE: &str = "otel.status_code";
}
```

Usage:

```rust
use crate::tracing::fields::{task, worker, otel};

#[instrument(fields(
    {task::ID} = %task_id,
    {worker::ID} = %worker_id,
    {otel::KIND} = "internal",
))]
async fn process_task(...) { ... }
```

---

## Implementation Phases

### Phase 1: Core Refactor (Week 1)

**Goal:** Eliminate duplication, create centralized tracing module

**Tasks:**
1. ✅ Create `netmito/src/tracing/mod.rs`
2. ✅ Implement `TracingBuilder` with console + file support
3. ✅ Implement layer composition with type erasure
4. ✅ Add unit tests for builder API
5. ✅ Create `config/tracing_config.rs` with new config structs
6. ✅ Update `CoordinatorConfig::setup_tracing()`
7. ✅ Update `WorkerConfig::setup_tracing()`
8. ✅ Maintain backward compatibility with deprecated methods
9. ✅ Update coordinator/worker initialization to use new API
10. ✅ Integration tests for migration

**Acceptance Criteria:**
- [ ] All existing tests pass
- [ ] No behavior changes in console/file output
- [ ] Zero breaking changes to public API
- [ ] Code duplication reduced from 260+ to <50 lines

**Estimated Effort:** 2-3 days

---

### Phase 2: Flexible Configuration (Week 2)

**Goal:** Enable granular control over outputs

**Tasks:**
1. ✅ Add `TracingConfig` to `CoordinatorConfig` and `WorkerConfig`
2. ✅ Implement TOML deserialization for all config types
3. ✅ Add `RotationStrategy` enum (Never, Hourly, Daily)
4. ✅ Support `max_files` configuration
5. ✅ Add environment variable overrides for all settings
6. ✅ Create migration logic for old `file_log`/`log_path` fields
7. ✅ Update `config.example.toml` with examples
8. ✅ Add validation for log paths and endpoints
9. ✅ Integration tests for various config combinations

**Example Configuration:**

```toml
# config.toml
[tracing]
service_name = "my-coordinator"
log_spans = false
log_thread_ids = true

[tracing.console]
enabled = true
level = "debug"
with_target = true
compact = false

[tracing.file]
enabled = true
level = "info"
rotation = "Daily"
max_files = 14  # Keep 2 weeks

# OpenTelemetry (Phase 3)
# [tracing.opentelemetry]
# enabled = false
```

**Acceptance Criteria:**
- [ ] Can enable/disable each output independently
- [ ] Can set different log levels per output
- [ ] Old configs still work with deprecation warnings
- [ ] Config validation catches invalid combinations

**Estimated Effort:** 2-3 days

---

### Phase 3: OpenTelemetry Integration (Week 3)

**Goal:** Add distributed tracing support

**Tasks:**
1. ✅ Add `opentelemetry` feature flag to Cargo.toml
2. ✅ Implement `build_otel_layer()` in `TracingBuilder`
3. ✅ Add resource attributes (service.name, version, instance.id)
4. ✅ Support both gRPC and HTTP/protobuf protocols
5. ✅ Implement graceful shutdown in `TracingGuard::drop()`
6. ✅ Add OTLP exporter configuration (endpoint, headers, timeout)
7. ✅ Create example docker-compose with Jaeger
8. ✅ Add integration tests with local OTLP collector
9. ✅ Document OpenTelemetry setup in guide

**Example Docker Compose for Testing:**

```yaml
version: '3.8'
services:
  jaeger:
    image: jaegertracing/all-in-one:latest
    ports:
      - "16686:16686"  # Jaeger UI
      - "4317:4317"    # OTLP gRPC
      - "4318:4318"    # OTLP HTTP
    environment:
      - COLLECTOR_OTLP_ENABLED=true
```

**Configuration Example:**

```toml
[tracing.opentelemetry]
enabled = true
endpoint = "http://localhost:4317"
protocol = "Grpc"
timeout = "10s"

# Optional: custom headers for auth
# [tracing.opentelemetry.headers]
# "x-api-key" = "secret"
```

**Acceptance Criteria:**
- [ ] Spans visible in Jaeger UI
- [ ] Trace context propagates across services
- [ ] Resource attributes attached correctly
- [ ] Graceful shutdown flushes all spans
- [ ] Works with feature flag disabled

**Estimated Effort:** 3-4 days

---

### Phase 4: Enhanced Instrumentation (Week 4)

**Goal:** Improve observability with structured logging

**Tasks:**
1. ✅ Define semantic field constants in `tracing/fields.rs`
2. ✅ Add `#[instrument]` to task execution functions
3. ✅ Add `#[instrument]` to API handlers
4. ✅ Add structured fields to existing `tracing::info!()` calls
5. ✅ Add span events for important state changes
6. ✅ Create instrumentation guidelines document
7. ✅ Update 10-15 high-value functions as examples
8. ✅ Add span timing logs (optional via config)

**Target Functions for Instrumentation:**
- `worker::execute_task()`
- `worker::fetch_task()`
- `coordinator::assign_task()`
- API handlers in `api/tasks.rs`, `api/workers.rs`
- S3 operations in `service/s3.rs`
- Database operations in `service/db.rs`

**Guidelines:**

```rust
// ❌ Before
tracing::info!("Processing task {}", task_id);

// ✅ After
tracing::info!(
    task.id = %task_id,
    task.type = %task_type,
    "Processing task"
);

// ❌ Before
async fn execute_task(task_id: Uuid) -> Result<()> {
    tracing::debug!("Starting task {}", task_id);
    // ... work ...
}

// ✅ After
#[instrument(skip(payload), fields(task.id = %task_id))]
async fn execute_task(task_id: Uuid, payload: Vec<u8>) -> Result<()> {
    // Automatic span creation, timing, error recording
    // ... work ...
}
```

**Acceptance Criteria:**
- [ ] Semantic field names used consistently
- [ ] 80%+ coverage of critical paths with `#[instrument]`
- [ ] Span hierarchies visible in Jaeger
- [ ] Structured fields queryable in logs
- [ ] Documentation updated with examples

**Estimated Effort:** 3-4 days

---

### Phase 5: Documentation & Migration (Week 5)

**Goal:** Complete user-facing documentation

**Tasks:**
1. ✅ Update `guide/src/guide/troubleshooting.md` with new features
2. ✅ Create `guide/src/guide/observability.md` (new chapter)
3. ✅ Update `config.example.toml` with all options
4. ✅ Write migration guide from v0.6.x to v0.7.0
5. ✅ Add OpenTelemetry setup tutorial
6. ✅ Document semantic field naming conventions
7. ✅ Create runbook for common scenarios
8. ✅ Update API documentation
9. ✅ Add CHANGELOG.md entries

**New Documentation Sections:**

```markdown
# Observability Guide

## Configuring Logging

### Console Output
...

### File Logging
...

### OpenTelemetry Integration
...

## Instrumentation Best Practices

### Using Structured Fields
...

### Span Hierarchies
...

## Troubleshooting

### Debugging with Jaeger
...

### Log Correlation
...
```

**Acceptance Criteria:**
- [ ] Clear migration path documented
- [ ] All config options explained
- [ ] Working examples for all outputs
- [ ] Troubleshooting scenarios covered

**Estimated Effort:** 2-3 days

---

## Migration Strategy

### Backward Compatibility

**Principles:**
1. **No breaking changes** - All existing code continues to work
2. **Deprecation warnings** - Guide users to new API
3. **Automatic migration** - Old configs map to new structure
4. **Gradual adoption** - Users can migrate incrementally

**Deprecated APIs:**

```rust
// Deprecated in v0.7.0, will be removed in v0.8.0
#[deprecated(since = "0.7.0", note = "Use tracing.file.enabled")]
pub file_log: bool

#[deprecated(since = "0.7.0", note = "Use tracing.file.path")]
pub log_path: Option<RelativePathBuf>

#[deprecated(since = "0.7.0", note = "Use setup_tracing()")]
pub fn setup_tracing_subscriber(&self) -> Result<TracingGuard>
```

**Migration Path:**

**Step 1: v0.6.x → v0.7.0 (Compatible)**

Old config still works:
```toml
# v0.6.x style (still works, with deprecation warnings)
[coordinator]
file_log = true
log_path = "/var/log/mito/coordinator.log"
```

New config recommended:
```toml
# v0.7.0 style (recommended)
[coordinator.tracing.file]
enabled = true
path = "/var/log/mito/coordinator.log"
rotation = "Daily"
max_files = 7
```

**Step 2: v0.7.0 → v0.8.0 (Breaking)**

Old fields removed:
```toml
# v0.8.0: old style no longer supported
[coordinator.tracing.file]
enabled = true  # Required
path = "/var/log/mito/coordinator.log"
```

### Config Migration Logic

```rust
impl CoordinatorConfig {
    /// Migrate old config to new format
    fn migrate_file_config(&self) -> FileConfig {
        let mut config = self.tracing.file.clone();

        // Priority: new config > old config > defaults
        #[allow(deprecated)]
        {
            if self.file_log && !config.enabled {
                config.enabled = true;
                tracing::warn!(
                    "Using deprecated 'file_log' field. \
                     Please migrate to 'tracing.file.enabled'"
                );
            }

            if let Some(ref path) = self.log_path {
                if config.path.is_none() {
                    config.path = Some(path.relative().to_path_buf());
                    tracing::warn!(
                        "Using deprecated 'log_path' field. \
                         Please migrate to 'tracing.file.path'"
                    );
                }
            }
        }

        config
    }
}
```

### Runtime Behavior

**v0.6.x (current):**
- Console always enabled
- File conditionally enabled via `file_log`
- Two separate initializations (early + late)

**v0.7.0 (new):**
- All outputs configurable
- Single initialization via `setup_tracing()`
- Backward compatible with v0.6.x configs

**v0.8.0 (future):**
- Clean API, no deprecated fields
- Required migration

---

## Testing Strategy

### Unit Tests

**File:** `netmito/src/tracing/tests.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_default() {
        let builder = TracingBuilder::new("test-service");
        assert_eq!(builder.service_name, "test-service");
    }

    #[test]
    fn test_console_only() {
        let guard = TracingBuilder::new("test")
            .with_console(ConsoleConfig::default())
            .build()
            .unwrap();

        // Verify subscriber is active
        tracing::info!("test message");
        // Guard dropped, subscriber cleaned up
    }

    #[test]
    fn test_file_only() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_config = FileConfig {
            enabled: true,
            path: Some(temp_dir.path().join("test.log")),
            ..Default::default()
        };

        let guard = TracingBuilder::new("test")
            .with_file(file_config)
            .build()
            .unwrap();

        tracing::info!("test message");
        drop(guard);

        // Verify log file created
        assert!(temp_dir.path().join("test.log").exists());
    }

    #[test]
    fn test_multiple_layers() {
        let guard = TracingBuilder::new("test")
            .with_console(ConsoleConfig::default())
            .with_file(FileConfig { enabled: true, ..Default::default() })
            .build()
            .unwrap();

        // Both outputs active
        tracing::info!(key = "value", "test message");
    }

    #[test]
    #[cfg(feature = "opentelemetry")]
    fn test_opentelemetry_layer() {
        // Test OTLP layer creation
    }
}
```

### Integration Tests

**File:** `netmito/tests/tracing_integration.rs`

```rust
#[tokio::test]
async fn test_coordinator_tracing_setup() {
    let config = CoordinatorConfig {
        tracing: TracingConfig {
            console: ConsoleConfig { enabled: true, ..Default::default() },
            file: FileConfig { enabled: false, ..Default::default() },
            ..Default::default()
        },
        ..Default::default()
    };

    let guard = config.setup_tracing().unwrap();

    // Verify tracing works
    tracing::info!("integration test");

    drop(guard);
}

#[tokio::test]
async fn test_backward_compatibility() {
    let config = CoordinatorConfig {
        file_log: true,  // Old style
        log_path: Some("/tmp/test.log".into()),
        ..Default::default()
    };

    // Should still work with deprecation warnings
    let guard = config.setup_tracing().unwrap();

    // Verify file logging enabled
    tracing::info!("backward compatibility test");
}
```

### End-to-End Tests

**File:** `netmito/tests/e2e_observability.rs`

```rust
#[tokio::test]
#[cfg(feature = "opentelemetry")]
async fn test_distributed_tracing() {
    // Start local OTLP collector
    let collector = start_otlp_collector().await;

    // Configure coordinator with OTLP
    let coordinator_config = CoordinatorConfig {
        tracing: TracingConfig {
            opentelemetry: OpenTelemetryConfig {
                enabled: true,
                endpoint: collector.endpoint(),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };

    // Configure worker with OTLP
    let worker_config = WorkerConfig {
        tracing: TracingConfig {
            opentelemetry: OpenTelemetryConfig {
                enabled: true,
                endpoint: collector.endpoint(),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };

    // Start coordinator and worker
    let coordinator = start_coordinator(coordinator_config).await;
    let worker = start_worker(worker_config).await;

    // Create and execute task
    let task = coordinator.create_task(...).await;
    worker.execute_task(task).await;

    // Verify trace in collector
    let traces = collector.get_traces().await;
    assert_eq!(traces.len(), 1);

    let trace = &traces[0];
    assert_eq!(trace.spans.len(), 3); // create_task + fetch_task + execute_task

    // Verify span hierarchy
    assert_eq!(trace.root_span().name, "coordinator.create_task");
    assert_eq!(trace.root_span().children[0].name, "worker.execute_task");
}
```

### Performance Tests

**File:** `netmito/benches/tracing_overhead.rs`

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_no_tracing(c: &mut Criterion) {
    c.bench_function("no_tracing", |b| {
        b.iter(|| {
            black_box(do_work());
        });
    });
}

fn bench_tracing_disabled(c: &mut Criterion) {
    let _guard = setup_tracing_with_filter("off");

    c.bench_function("tracing_disabled", |b| {
        b.iter(|| {
            tracing::info!("test");
            black_box(do_work());
        });
    });
}

fn bench_tracing_enabled(c: &mut Criterion) {
    let _guard = setup_tracing_with_filter("info");

    c.bench_function("tracing_enabled", |b| {
        b.iter(|| {
            tracing::info!("test");
            black_box(do_work());
        });
    });
}

fn bench_instrumented_function(c: &mut Criterion) {
    let _guard = setup_tracing_with_filter("info");

    c.bench_function("instrumented_function", |b| {
        b.iter(|| {
            instrumented_work(black_box(42));
        });
    });
}

#[instrument]
fn instrumented_work(value: i32) -> i32 {
    value * 2
}

criterion_group!(
    benches,
    bench_no_tracing,
    bench_tracing_disabled,
    bench_tracing_enabled,
    bench_instrumented_function
);
criterion_main!(benches);
```

---

## Documentation Updates

### 1. User Guide: `guide/src/guide/observability.md` (New)

```markdown
# Observability

Mitosis provides comprehensive logging and tracing capabilities through the
`tracing` ecosystem, with optional OpenTelemetry support for distributed tracing.

## Quick Start

### Console Logging

By default, console logging is enabled. Control the log level via the `RUST_LOG`
environment variable:

```bash
# Info level (default)
RUST_LOG=info mito coordinator

# Debug level for netmito
RUST_LOG=netmito=debug mito coordinator

# Trace level for specific module
RUST_LOG=netmito::worker=trace mito worker
```

### File Logging

Enable file logging in your configuration:

```toml
[coordinator.tracing.file]
enabled = true
level = "info"
rotation = "Daily"
max_files = 7
```

Logs are written to:
- **Linux**: `~/.cache/mitosis/coordinator/{address}.log`
- **macOS**: `~/Library/Caches/mitosis/coordinator/{address}.log`
- **Windows**: `%LOCALAPPDATA%\mitosis\coordinator\{address}.log`

### OpenTelemetry (Distributed Tracing)

Enable the `opentelemetry` feature and configure OTLP export:

```toml
[coordinator.tracing.opentelemetry]
enabled = true
endpoint = "http://localhost:4317"
protocol = "Grpc"
```

Start Jaeger for local development:

```bash
docker run -d \
  -e COLLECTOR_OTLP_ENABLED=true \
  -p 16686:16686 \
  -p 4317:4317 \
  jaegertracing/all-in-one:latest
```

Access Jaeger UI at http://localhost:16686

## Configuration Reference

### Console Output

```toml
[coordinator.tracing.console]
enabled = true           # Enable/disable console output
level = "debug"          # Override RUST_LOG
with_target = true       # Show module path
compact = false          # Use compact format
```

### File Output

```toml
[coordinator.tracing.file]
enabled = true                           # Enable file logging
level = "info"                           # File log level
path = "/var/log/mitosis/coordinator.log"  # Custom path (optional)
rotation = "Daily"                       # Never, Hourly, Daily
max_files = 14                           # Keep 2 weeks
```

### OpenTelemetry

```toml
[coordinator.tracing.opentelemetry]
enabled = true
endpoint = "http://otel-collector:4317"
protocol = "Grpc"  # or "HttpProtobuf"
timeout = "10s"

# Optional: authentication headers
[coordinator.tracing.opentelemetry.headers]
"x-api-key" = "secret"
```

### Service Identity

Override service metadata:

```toml
[coordinator.tracing]
service_name = "my-mitosis-coordinator"
service_version = "1.0.0"
service_instance_id = "prod-coord-01"
```

## Advanced Usage

### Multiple Outputs

Enable any combination of outputs:

```toml
# Console + File + OpenTelemetry
[coordinator.tracing.console]
enabled = true
level = "info"

[coordinator.tracing.file]
enabled = true
level = "debug"  # More verbose in files

[coordinator.tracing.opentelemetry]
enabled = true
endpoint = "http://localhost:4317"
```

### Structured Logging

Use structured fields for better queryability:

```rust
// ❌ String formatting
tracing::info!("User {} logged in", username);

// ✅ Structured fields
tracing::info!(
    user.name = %username,
    user.id = user_id,
    "User logged in"
);
```

### Span Instrumentation

The `#[instrument]` macro automatically creates spans:

```rust
use tracing::instrument;

#[instrument(skip(db), fields(task.id = %task_id))]
async fn execute_task(db: &Database, task_id: Uuid) -> Result<()> {
    // Automatic span with timing and error recording
    Ok(())
}
```

## Troubleshooting

### No Logs Appearing

1. Check `RUST_LOG` environment variable
2. Verify `tracing.console.enabled = true`
3. Ensure log level is appropriate (info, debug, trace)

### File Not Created

1. Check directory permissions
2. Verify `tracing.file.enabled = true`
3. Check logs for "log path not valid" errors

### OpenTelemetry Not Working

1. Verify OTLP endpoint is reachable
2. Check collector logs for connection errors
3. Ensure `opentelemetry` feature is enabled in Cargo.toml
4. Verify resource attributes in Jaeger UI

### Performance Impact

Logging overhead is minimal when using appropriate log levels:

- **Disabled spans**: <1% overhead
- **Enabled spans**: 2-5% overhead
- **File I/O**: Buffered, negligible impact
- **OTLP export**: Batched, configurable timeout

For production:
- Use `info` or `warn` level
- Enable sampling if needed
- Monitor OTLP batch sizes
```

### 2. Update `guide/src/guide/troubleshooting.md`

Add section on debugging with enhanced logging:

```markdown
## Debugging with Tracing

### Enable Debug Logging

```bash
RUST_LOG=netmito=debug mito coordinator
```

### Show Span Timings

```toml
[coordinator.tracing]
log_spans = true
```

### Correlate Logs Across Services

With OpenTelemetry enabled, logs automatically include trace and span IDs:

```
INFO netmito::worker [trace_id=abc123 span_id=def456] task.id=789 Task completed
```

Search logs by trace ID to see all events in a distributed operation.
```

### 3. Update `config.example.toml`

```toml
# Tracing and Observability Configuration
[coordinator.tracing]
# Service identity (optional, auto-detected if not set)
# service_name = "mitosis-coordinator"
# service_version = "0.7.0"
# service_instance_id = "instance-01"

# Show span enter/exit events in logs
log_spans = false

# Show thread IDs in log output
log_thread_ids = false

# Console output (stdout/stderr)
[coordinator.tracing.console]
enabled = true
level = "info"  # Override RUST_LOG
with_target = true
compact = false

# File output (rotating logs)
[coordinator.tracing.file]
enabled = false
level = "info"
# path = "/var/log/mitosis/coordinator.log"  # Custom path
rotation = "Daily"  # Never, Hourly, Daily
max_files = 7       # Keep 1 week

# OpenTelemetry (requires 'opentelemetry' feature)
[coordinator.tracing.opentelemetry]
enabled = false
endpoint = "http://localhost:4317"
protocol = "Grpc"  # or "HttpProtobuf"
timeout = "10s"

# Optional: authentication headers
# [coordinator.tracing.opentelemetry.headers]
# "x-api-key" = "your-api-key"

# Worker tracing (similar to coordinator)
[worker.tracing]
# ... same structure as coordinator ...

[worker.tracing.file]
enabled = false
# For shared logs across workers, set worker_prefix
# worker_prefix = "worker-{uuid}"  # Automatic when shared_log = true
```

### 4. Migration Guide: `MIGRATION.md` (New)

```markdown
# Migration Guide: v0.6.x → v0.7.0

## Overview

Version 0.7.0 introduces a new unified tracing configuration system. The old
`file_log` and `log_path` fields are deprecated but still supported for
backward compatibility.

## Breaking Changes

**None.** This is a fully backward-compatible release.

## Deprecations

The following fields are deprecated and will be removed in v0.8.0:

- `CoordinatorConfig::file_log` → Use `tracing.file.enabled`
- `CoordinatorConfig::log_path` → Use `tracing.file.path`
- `WorkerConfig::file_log` → Use `tracing.file.enabled`
- `WorkerConfig::log_path` → Use `tracing.file.path`
- `WorkerConfig::shared_log` → Use `tracing.file.worker_prefix`

## Migration Steps

### 1. Update Configuration File

**Before (v0.6.x):**

```toml
[coordinator]
bind = "127.0.0.1:50051"
file_log = true
log_path = "/var/log/mitosis/coordinator.log"
```

**After (v0.7.0):**

```toml
[coordinator]
bind = "127.0.0.1:50051"

[coordinator.tracing.file]
enabled = true
path = "/var/log/mitosis/coordinator.log"
rotation = "Daily"
max_files = 7
```

### 2. Enable OpenTelemetry (Optional)

**New in v0.7.0:**

Add to `Cargo.toml`:

```toml
[dependencies]
netmito = { version = "0.7", features = ["opentelemetry"] }
```

Add to `config.toml`:

```toml
[coordinator.tracing.opentelemetry]
enabled = true
endpoint = "http://localhost:4317"
```

### 3. Update Code (If Using Programmatic Config)

**Before:**

```rust
let config = CoordinatorConfig {
    file_log: true,
    log_path: Some("/var/log/mito.log".into()),
    ..Default::default()
};

let guard = config.setup_tracing_subscriber()?;
```

**After:**

```rust
let config = CoordinatorConfig {
    tracing: TracingConfig {
        file: FileConfig {
            enabled: true,
            path: Some("/var/log/mito.log".into()),
            ..Default::default()
        },
        ..Default::default()
    },
    ..Default::default()
};

let guard = config.setup_tracing()?;
```

### 4. Test Migration

Run your application with the new config:

```bash
mito coordinator --config config.toml
```

You should see deprecation warnings if using old fields:

```
WARN netmito::config: Using deprecated 'file_log' field.
     Please migrate to 'tracing.file.enabled'
```

## New Features

### Multiple Output Combinations

Enable console + file + OpenTelemetry simultaneously:

```toml
[coordinator.tracing.console]
enabled = true
level = "info"

[coordinator.tracing.file]
enabled = true
level = "debug"

[coordinator.tracing.opentelemetry]
enabled = true
endpoint = "http://localhost:4317"
```

### Configurable Rotation

```toml
[coordinator.tracing.file]
rotation = "Daily"  # or "Hourly", "Never"
max_files = 14      # Keep 2 weeks
```

### Service Identity

```toml
[coordinator.tracing]
service_name = "prod-coordinator-us-east"
service_version = "1.0.0"
service_instance_id = "i-0123456789"
```

## Timeline

- **v0.7.0** (Current): Old config supported with deprecation warnings
- **v0.8.0** (Future): Old config removed, migration required

We recommend migrating before v0.8.0 release.
```

---

## Appendices

### Appendix A: Semantic Conventions Reference

Based on [OpenTelemetry Semantic Conventions](https://opentelemetry.io/docs/specs/semconv/):

#### Resource Attributes

| Attribute | Type | Required | Example |
|-----------|------|----------|---------|
| `service.name` | string | Yes | `"mitosis-coordinator"` |
| `service.version` | string | No | `"0.7.0"` |
| `service.namespace` | string | No | `"production"` |
| `service.instance.id` | string | No | `"worker-abc123"` |

#### Span Attributes (Tasks)

| Attribute | Type | Description |
|-----------|------|-------------|
| `task.id` | string | Task UUID |
| `task.type` | string | Task type/name |
| `task.state` | string | pending, running, completed, failed |
| `task.priority` | int | Priority value |
| `task.worker_id` | string | Assigned worker UUID |
| `task.duration_ms` | int | Execution time |

#### Span Attributes (HTTP)

| Attribute | Type | Description |
|-----------|------|-------------|
| `http.method` | string | GET, POST, etc. |
| `http.route` | string | `/api/tasks/:id` |
| `http.status_code` | int | 200, 404, etc. |
| `http.client_ip` | string | Client IP address |

#### Span Attributes (Database)

| Attribute | Type | Description |
|-----------|------|-------------|
| `db.system` | string | `"postgresql"` |
| `db.operation` | string | SELECT, INSERT, UPDATE |
| `db.statement` | string | SQL query (sanitized) |

### Appendix B: Estimated File Sizes

**Log Rotation Planning:**

Typical log volume per component:

- **Coordinator** (info level): ~10 MB/day
- **Worker** (info level): ~5 MB/day per worker
- **Coordinator** (debug level): ~100 MB/day
- **Worker** (debug level): ~50 MB/day per worker

**Recommended retention:**

- Production: 7-14 days (info level)
- Development: 1-3 days (debug level)
- CI/CD: 1 day (trace level)

### Appendix C: Performance Benchmarks

Expected overhead based on testing:

| Configuration | CPU Overhead | Memory Overhead | Latency Impact |
|---------------|--------------|-----------------|----------------|
| Console only | <1% | ~1 MB | <100μs |
| Console + File | 1-2% | ~5 MB | <200μs |
| Console + File + OTLP | 2-5% | ~10 MB | <500μs |
| With `#[instrument]` | +1-2% | +2 MB | +50-100μs |

*Benchmarks conducted with 1000 req/s, info level logging*

### Appendix D: Example Span Hierarchies

**Task Creation Flow:**

```
coordinator.create_task [200ms]
├─ db.insert_task [50ms]
│  └─ db.execute_query [45ms]
├─ worker_queue.enqueue [30ms]
└─ api.respond [5ms]
```

**Task Execution Flow:**

```
worker.execute_task [5000ms]
├─ worker.fetch_task [100ms]
│  └─ http.get_task [95ms]
├─ worker.download_inputs [2000ms]
│  ├─ s3.get_object [1000ms]
│  └─ s3.get_object [1000ms]
├─ worker.run_computation [2500ms]
└─ worker.upload_results [400ms]
   └─ s3.put_object [380ms]
```

### Appendix E: Common Troubleshooting Scenarios

**Scenario 1: High Log Volume**

*Problem:* Logs filling disk rapidly

*Solution:*
```toml
[coordinator.tracing.file]
rotation = "Daily"
max_files = 3  # Reduce retention
level = "warn"  # Reduce verbosity
```

**Scenario 2: Missing Spans in Jaeger**

*Problem:* Spans not appearing in Jaeger UI

*Checklist:*
1. ✅ `opentelemetry` feature enabled
2. ✅ Endpoint reachable: `curl http://localhost:4317`
3. ✅ Collector running: `docker ps | grep jaeger`
4. ✅ Service name matches filter in UI
5. ✅ Graceful shutdown called (flushes spans)

**Scenario 3: Performance Degradation**

*Problem:* Application slower after enabling tracing

*Diagnosis:*
```bash
# Check log level
RUST_LOG=info  # Should be info/warn in prod, not debug/trace

# Disable expensive operations
[coordinator.tracing]
log_spans = false  # Reduces overhead
```

**Scenario 4: Trace Context Not Propagating**

*Problem:* Spans from different services not linked

*Solution:*
Ensure HTTP headers propagated:

```rust
use opentelemetry::global;
use opentelemetry::propagation::TextMapPropagator;

// Inject context into HTTP request
let cx = tracing::Span::current().context();
let mut headers = HeaderMap::new();
global::get_text_map_propagator(|propagator| {
    propagator.inject_context(&cx, &mut headers);
});

// Extract context from HTTP request
let parent_cx = global::get_text_map_propagator(|propagator| {
    propagator.extract(&headers)
});
```

---

## Summary

This plan provides a comprehensive roadmap for improving netmito's tracing system:

1. **Phase 1**: Centralize and refactor (eliminate duplication)
2. **Phase 2**: Flexible configuration (enable and/or combinations)
3. **Phase 3**: OpenTelemetry support (distributed tracing)
4. **Phase 4**: Enhanced instrumentation (better observability)
5. **Phase 5**: Documentation and migration guides

**Total Estimated Timeline:** 4-5 weeks

**Risks & Mitigations:**
- **Risk**: Breaking changes → **Mitigation**: Full backward compatibility
- **Risk**: Performance impact → **Mitigation**: Comprehensive benchmarks
- **Risk**: Complex configuration → **Mitigation**: Sensible defaults
- **Risk**: OTLP integration issues → **Mitigation**: Feature flag, extensive testing

**Success Metrics:**
- [ ] Zero breaking changes in v0.7.0
- [ ] 80% reduction in code duplication
- [ ] OpenTelemetry integration working with Jaeger
- [ ] Comprehensive documentation
- [ ] Performance overhead <5%

---

**Document Version:** 1.0
**Last Updated:** 2025-11-22
**Review Status:** Awaiting approval

**Next Steps:**
1. Review this plan
2. Approve or suggest modifications
3. Begin Phase 1 implementation
