# Harmonic

A high-performance distributed file synchronization system built with Rust, gRPC, and Tokio. Built to replace rsync on a modern tech and algirithm stack.

## Development Roadmap

**Foundation**
- [ ] Collect performance metrics
- [ ] Add connectivity check on startup / periodically?
- [ ] Add retry logic with debounce

**Security Improvements**
- [ ] Implement TLS encryption for network traffic -> letsencrypt? requires internet and infra setup / ssh style "fingerprint" prompt
- [ ] Investigate complexity of authentication/authorization (token-based or mTLS)
- [ ] Implement graceful shutdown handling

**Performance Enhancements**
- [ ] Add parallel file writes -> algorithm is now parallelised. Separate point on this further down
- [ ] Add zero-copy I/O optimizations -> this should be implemented, particularly for larger files. use Bytes type?

**Usability**
- [ ] Improve tracing - currently a bit hard to follow. Improve how traces and spans are captured and what is instrumented
- [ ] Better initial setup cli guidance -> prompt user to enter certain config rather than referring to config file?

**Configurability**
- [ ] More sync modes that can trigger start -> May be redundant

**Completed**
- [x] Investigate replacing MD5 (BLAKE3!) -> now using faster, more secure BLAKE3 -> enabler for rolling hash partial updates?
- [x] Overhaul synchronization algorithm -> rsync style rolling hash but faster algorithms and protocol + set and forget style sync with debounce
- [x] Implement weak hashing algorithm -> No actively maintained adler32 crate with rolling hash -> Implemented BuzHash in pure rust. Potentially faster than adler32, to be investigated
- [x] Configurable debounce algorithm
- [x] Prevent directory traversal attacks -> should be resolved due to relative path creation
- [x] Implement delta sync ~~(converging dynamic rolling hash idea)~~ -> Proper rolling hash algorithm implemented
- [x] Add some tests -> unit tests / file system integration tests
- [x] Add extensive integration tests -> file system, state generation
- [x] Implement compression (zstd?) -> zstd! should make this a feature / configurable
- [x] Use tracing crate for distributed trace
- [x] Fix `futures::lock::Mutex` to `tokio::sync::Mutex`
- [x] Improve cli functionality -> allow more config to be directly passed as args
- [x] Improve error handling and propagation -> thiserror + anyhow


### On the topic of parallel file writes
Very difficult to implement asynchronous, platform agnostic parallel file writes to the same file.
Overview:
Instructions may come in from other machine since file reads are very fast
A queue of messages with instructions to write to data to various locations will build up
Chunks from signature generation are actually cached, available in memory, hence no io limitation
Although the file api provided by tokio is asynchronous, underlying io operations are not necessarily
Difficult to implement cross-platform -> do macos and windows support this?

---


## Summary

Harmonic provides bidirectional file synchronization between a client and server using a three-phase protocol: state comparison, sync planning, and bidirectional streaming transfer. It uses hash-based change detection and timestamp comparison to efficiently determine which files need to be transferred and in which direction.

## Features

- **gRPC Bidirectional Streaming**: High-performance async communication using Tokio and tonic
- **Event-Driven Architecture**: File system watcher automatically triggers syncs on changes
- **Smart Debouncing**: Configurable smart debounce algorithm dynamically allocates points based on which event was triggered on the file system. This prevents excessive syncing during active editing
- **Multiple Sync Modes**:
  - `event-based` (default): Triggered by file system events
  - `schedule-based`: Periodic sync at configured intervals
  - `manual-only`: Daemon-less one-off manual synchronisation
- **Cross-Platform**: Exclusively uses cross-platform crates and code. Tested on latest versions of Windows, MacOS and Ubuntu
- **Chunked Transfers**: Smart chunking algorithm dynamically adjusts chunk size for rolling hash signature generation based on input data

## How to Run

### Configuration

- 1. Download the applicable binary from release page
- 2. On first startup check the configuration file and make the necessary modifications
- 3. Modify config file as per requirements
- 4. Run server binary
- 5. Run client binary


