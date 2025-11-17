# Implementation Plan - Task Suite and Node Manager System

This directory contains detailed, phase-by-phase implementation guides for the Task Suite and Node Manager feature set described in the RFC.

## Overview

The implementation is divided into **9 phases** spanning **11 weeks total**. Each phase has its own subdirectory with comprehensive step-by-step guides.

## Phase Structure

```
impl/
├── README.md (this file)
├── phase1/  - Database Schema (1 week)
├── phase2/  - API Schema and Coordinator Endpoints (1 week)
├── phase3/  - WebSocket Manager (1 week)
├── phase4/  - Node Manager Core (2 weeks)
├── phase5/  - Environment Hooks (1 week)
├── phase6/  - Worker Spawning and IPC (2 weeks)
├── phase7/  - Managed Worker Mode (1 week)
├── phase8/  - Integration and E2E Testing (1 week)
└── phase9/  - Documentation and Deployment (1 week)
```

## Implementation Timeline

| Phase | Description | Duration | Dependencies |
|-------|-------------|----------|--------------|
| **Phase 1** | Database Schema | 1 week | None |
| **Phase 2** | API Schema and Coordinator Endpoints | 1 week | Phase 1 |
| **Phase 3** | WebSocket Manager | 1 week | Phase 1, 2 |
| **Phase 4** | Node Manager Core | 2 weeks | Phase 1, 2, 3 |
| **Phase 5** | Environment Hooks | 1 week | Phase 1-4 |
| **Phase 6** | Worker Spawning and IPC | 2 weeks | Phase 1-5 |
| **Phase 7** | Managed Worker Mode | 1 week | Phase 1-6 |
| **Phase 8** | Integration and E2E Testing | 1 week | Phase 1-7 |
| **Phase 9** | Documentation and Deployment | 1 week | Phase 1-8 |

**Total Duration:** 11 weeks

## Quick Navigation

### [Phase 1: Database Schema](./phase1/01-database-schema.md)
**Duration:** 1 week
**Focus:** Create all database tables, indexes, triggers, and SeaORM entities

**Key Deliverables:**
- 5 new tables: `task_suites`, `node_managers`, `group_node_manager`, `task_suite_managers`, `task_execution_failures`
- Database triggers for auto-updating task counters
- SeaORM entity models
- State enums

**Start Here:** No dependencies, foundational work

---

### [Phase 2: API Schema and Coordinator Endpoints](./phase2/01-api-schema-coordinator.md)
**Duration:** 1 week
**Focus:** Implement HTTP REST APIs for suite and manager management

**Key Deliverables:**
- 11 new API endpoints (7 for suites, 4 for managers)
- Request/response types in `schema.rs`
- Permission model implementation
- Tag matching logic

**Requires:** Phase 1 (database schema must exist)

---

### [Phase 3: WebSocket Manager](./phase3/01-websocket-manager.md)
**Duration:** 1 week
**Focus:** Implement WebSocket server for real-time manager communication

**Key Deliverables:**
- WebSocket endpoint at `/ws/managers`
- Connection management with JWT authentication
- 6 manager message handlers
- Request-response multiplexing
- Reconnection and heartbeat handling

**Requires:** Phase 1, 2 (needs APIs and database)

---

### [Phase 4: Node Manager Core](./phase4/01-node-manager-core.md)
**Duration:** 2 weeks
**Focus:** Create the `mitosis-manager` binary with core functionality

**Key Deliverables:**
- New `mitosis-manager` binary
- Registration flow with JWT authentication
- WebSocket client implementation
- State machine (Idle → Preparing → Executing → Cleanup)
- Suite assignment logic
- Heartbeat system

**Requires:** Phase 1, 2, 3 (needs full coordinator infrastructure)

---

### [Phase 5: Environment Hooks](./phase5/01-environment-hooks.md)
**Duration:** 1 week
**Focus:** Implement suite environment preparation and cleanup

**Key Deliverables:**
- `EnvHookSpec` parsing and execution
- S3 resource download integration
- Context variable injection (MITOSIS_*)
- Hook timeout and error handling
- State machine integration (PREPARING, CLEANUP states)

**Requires:** Phase 1-4 (manager must exist to execute hooks)

---

### [Phase 6: Worker Spawning and IPC](./phase6/01-worker-spawning-ipc.md)
**Duration:** 2 weeks
**Focus:** Spawn managed workers and handle IPC communication

**Key Deliverables:**
- iceoryx2 IPC setup
- Worker process spawning with CPU binding
- Worker health monitoring and auto-recovery
- Task pre-fetching buffer
- Failure tracking (3-strike abort policy)
- IPC request handlers (FetchTask, ReportTask)

**Requires:** Phase 1-5 (most complex phase, needs all prior infrastructure)

---

### [Phase 7: Managed Worker Mode](./phase7/01-managed-worker-mode.md)
**Duration:** 1 week
**Focus:** Update worker binary to support managed mode

**Key Deliverables:**
- `--managed` CLI flag
- `WorkerComm` trait abstraction
- `IpcWorkerComm` implementation
- Control message handling (CancelTask, Shutdown)
- Dual-mode worker (independent or managed)

**Requires:** Phase 1-6 (needs IPC infrastructure from Phase 6)

---

### [Phase 8: Integration and E2E Testing](./phase8/01-integration-e2e-testing.md)
**Duration:** 1 week
**Focus:** Comprehensive testing of the entire system

**Key Deliverables:**
- End-to-end suite execution tests
- Multi-manager scenarios (tag matching, concurrent suites)
- Failure scenario testing (crashes, disconnections, hook failures)
- Performance benchmarks (100K+ tasks)
- Backward compatibility validation (independent workers)

**Requires:** Phase 1-7 (tests the complete system)

---

### [Phase 9: Documentation and Deployment](./phase9/01-documentation-deployment.md)
**Duration:** 1 week
**Focus:** Production readiness with docs, monitoring, and deployment

**Key Deliverables:**
- User documentation (suite creation, manager deployment)
- API documentation (OpenAPI spec updates)
- Internal documentation (architecture, runbooks)
- Monitoring and alerts (Prometheus, Grafana)
- Deployment procedures (phased rollout, rollback plan)

**Requires:** Phase 1-8 (finalizes the production-ready system)

---

## Key Design Changes (Applied to RFC)

The implementation guides reflect the following design refinements made to the original RFC:

### 1. **Labels: HashMap → HashSet**
- **Old:** `labels: HashMap<String, String>` (key-value pairs)
- **New:** `labels: HashSet<String>` (simple tags like `["project:resnet", "phase:training"]`)
- **Reason:** Simpler querying, consistent with tags pattern

### 2. **Auto-Close Timeout: Configurable → Fixed**
- **Old:** `auto_close_timeout: Option<Duration>` (user-configurable)
- **New:** Fixed at 3 minutes (not exposed in API)
- **Reason:** Simplifies design, 3 minutes is sufficient for most use cases

### 3. **Task Counters: Single → Dual**
- **Old:** `pending_task_count: i32` (only pending tasks)
- **New:**
  - `total_tasks: i32` (total tasks ever submitted)
  - `pending_tasks: i32` (currently active tasks)
- **Reason:** Better observability, users want to track total vs. pending separately

## How to Use These Guides

### For Developers

1. **Start at Phase 1** and work sequentially (phases have strict dependencies)
2. **Read the entire guide** for each phase before starting implementation
3. **Use the Testing Checklist** to validate each phase before moving to the next
4. **Refer to the RFC** (../rfc.md) for detailed design rationale and architecture

### For Project Managers

1. **Use the timeline** to plan sprints and resource allocation
2. **Track progress** using the Success Criteria in each guide
3. **Note dependencies** when planning parallel work (e.g., Phase 2 & 3 can overlap slightly)
4. **Expect Phase 6** to be the most complex (2 weeks, requires most expertise)

### For Reviewers

1. **Each phase has clear Success Criteria** for code review acceptance
2. **Testing Checklists** define what must be validated before merge
3. **Dependencies** section shows what must be reviewed first

## File Structure

Each phase directory contains:
- `01-*.md` - Main implementation guide with:
  - Overview and timeline
  - Design references (extracted from RFC)
  - Step-by-step implementation tasks
  - Complete code examples
  - Testing checklist
  - Success criteria
  - Dependencies and next steps

## Code Examples

All implementation guides include:
- **Complete struct definitions** ready to copy/paste
- **Database queries** with proper indexing
- **API handlers** with permission checks
- **State machine logic** with transition rules
- **Error handling** patterns
- **Testing examples** (unit, integration, E2E)

## Getting Help

- **Architecture Questions:** See ../rfc.md Section 5 (Architecture Design)
- **API Questions:** See ../rfc.md Section 7 (API Design)
- **Database Questions:** See Phase 1 guide and ../rfc.md Section 6 (Data Models)
- **State Machines:** See ../rfc.md Section 9 (State Machines and Workflows)

## Progress Tracking

Use this checklist to track overall implementation progress:

- [ ] **Phase 1:** Database Schema
  - [ ] Migrations created and tested
  - [ ] SeaORM entities generated
  - [ ] Triggers validated

- [ ] **Phase 2:** API Endpoints
  - [ ] 11 endpoints implemented
  - [ ] Permission model working
  - [ ] Tag matching tested

- [ ] **Phase 3:** WebSocket Manager
  - [ ] WebSocket server running
  - [ ] 6 message handlers implemented
  - [ ] Reconnection tested

- [ ] **Phase 4:** Node Manager Core
  - [ ] `mitosis-manager` binary created
  - [ ] Registration and heartbeat working
  - [ ] State machine validated

- [ ] **Phase 5:** Environment Hooks
  - [ ] Preparation hooks working
  - [ ] Cleanup hooks working
  - [ ] S3 downloads integrated

- [ ] **Phase 6:** Worker Spawning and IPC
  - [ ] iceoryx2 IPC working
  - [ ] Workers spawning correctly
  - [ ] Auto-recovery validated
  - [ ] CPU binding tested

- [ ] **Phase 7:** Managed Worker Mode
  - [ ] `--managed` flag working
  - [ ] IPC communication validated
  - [ ] Dual-mode tested

- [ ] **Phase 8:** Integration Testing
  - [ ] E2E tests passing
  - [ ] Failure scenarios validated
  - [ ] Performance benchmarks met
  - [ ] Backward compatibility confirmed

- [ ] **Phase 9:** Documentation and Deployment
  - [ ] User docs published
  - [ ] Monitoring configured
  - [ ] Production deployment successful

## Estimated Effort

| Role | Estimated Hours |
|------|----------------|
| Backend Engineer | 320 hours (8 weeks full-time) |
| Database Engineer | 40 hours (1 week for Phase 1) |
| DevOps Engineer | 40 hours (1 week for Phase 9) |
| QA Engineer | 80 hours (2 weeks for Phase 8 + ongoing) |

**Total:** ~480 engineering hours across 11 calendar weeks

## Success Metrics

After completing all 9 phases, the system should achieve:

### Functional
- ✅ Task suites with environment hooks working end-to-end
- ✅ Node managers spawning and managing workers
- ✅ WebSocket-based push communication
- ✅ Independent workers still functioning (backward compatible)

### Performance
- ✅ Task fetch latency: P95 < 10ms (with prefetching)
- ✅ Suite throughput: 100K+ tasks in < 1 hour
- ✅ Manager registration: < 500ms
- ✅ Heartbeat processing: < 50ms

### Reliability
- ✅ Worker auto-recovery on crash
- ✅ WebSocket reconnection with exponential backoff
- ✅ Zero data loss on failures
- ✅ Graceful degradation under load

## Related Documentation

- [../rfc.md](../rfc.md) - Full RFC with architecture and design rationale
- [../README.md](../README.md) - Project README
- [../CONTRIBUTING.md](../CONTRIBUTING.md) - Contribution guidelines

---

**Last Updated:** 2025-11-17
**RFC Version:** 1.0.0 (with applied design changes)
**Status:** Ready for Implementation
