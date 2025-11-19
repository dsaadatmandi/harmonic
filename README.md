# Harmonic

A high-performance distributed file synchronization system built with Rust, gRPC, and Tokio.

## Development Roadmap

**Foundation**
- [x] Add some tests -> unit tests / file system integration tests
- [ ] Add extensive integration tests -> if required?
- [x] Improve error handling and propagation -> thiserror + anyhow
- [x] Fix `futures::lock::Mutex` to `tokio::sync::Mutex`
- [ ] Collect performance metrics
- [ ] Add connectivity check on startup / periodically?

**Security Improvements**
- [x] Investigate replacing MD5 (BLAKE3!) -> now using faster, more secure BLAKE3 -> enabler for rolling hash partial updates?
- [ ] Prevent directory traversal attacks
- [ ] Implement TLS encryption for network traffic
- [ ] Investigate complexity of authentication/authorization (token-based or mTLS)
- [ ] Use tracing for distributed trace
- [ ] Implement graceful shutdown handling
- [ ] Add retry logic with debounce

**Performance Enhancements**
- [x] Implement compression (zstd?) -> zstd! should make this a feature / configurable
- [ ] Add parallel file writes
- [ ] Implement delta sync (converging dynamic rolling hash idea)
- [ ] Add zero-copy I/O optimizations

**Configurability**
- [x] More control over debounce algorithm
- [ ] More sync modes that can trigger sync

---

## Summary

Harmonic provides bidirectional file synchronization between a client and server using a three-phase protocol: state comparison, sync planning, and bidirectional streaming transfer. It uses hash-based change detection and timestamp comparison to efficiently determine which files need to be transferred and in which direction.

## Features

- **gRPC Bidirectional Streaming**: High-performance async communication using Tokio and tonic
- **Event-Driven Architecture**: File system watcher automatically triggers syncs on changes
- **Smart Debouncing**: Point-based system prevents excessive syncing during active editing (Modify=1pt, Remove=5pts, Create=10pts, threshold=20pts)
- **Multiple Sync Modes**:
  - `event-based` (default): Triggered by file system events
  - `schedule-based`: Periodic sync at configured intervals
  - `manual-only`: No automatic triggering
- **Cross-Platform**: Supports Linux, macOS, Windows (x64 & ARM64)
- **Chunked Transfers**: Files streamed in 8KB chunks for memory efficiency

## How to Run

### Configuration

- 1. Download the applicable binary from release page
- 2. On first startup note the configuration file
- 3. Modify config file as per requirements


