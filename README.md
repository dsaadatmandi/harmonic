# Harmonic

A high-performance distributed file synchronization system built with Rust, gRPC, and Tokio. Built to replace rsync on a modern tech and algirithm stack.

## Development Roadmap

**Foundation**
- [ ] Add connectivity check on startup / periodically?
- [ ] Add retry logic with debounce

**Security Improvements**

- [ ] Investigate complexity of authentication/authorization (token-based or mTLS)
- [ ] Implement graceful shutdown handling

**Performance Enhancements**
- [ ] Add zero-copy I/O optimizations -> this should be implemented, particularly for larger files. use Bytes type?

**Usability**
- [ ] Improve tracing - currently a bit hard to follow. Improve how traces and spans are captured and what is instrumented
- [ ] Improve cli functionality -> allow more config to be directly passed as args

**Configurability**
- [ ] More sync modes that can trigger start -> May be redundant

**Completed**
- [x] Investigate replacing MD5 (BLAKE3!) -> now using faster, more secure BLAKE3 -> enabler for rolling hash partial updates?
- [x] Implement TLS encryption for network traffic -> ~~letsencrypt? requires internet and infra setup / ssh style "fingerprint" prompt~~ added bootstrap functionality to share self signed cert with clients via new grpc service
- [x] Overhaul synchronization algorithm -> rsync style rolling hash but faster algorithms and protocol + set and forget style sync with debounce
- [x] Add parallel file writes -> algorithm is now parallelised. Separate point on this further down
- [x] Implement weak hashing algorithm -> No actively maintained adler32 crate with rolling hash -> Implemented BuzHash in pure rust -> Potentially faster than adler32
- [x] Configurable debounce algorithm
- [x] Prevent directory traversal attacks -> should be resolved due to relative path creation
- [x] Collect performance metrics
- [x] Implement delta sync ~~(converging dynamic rolling hash idea)~~ -> Proper rolling hash algorithm implemented
- [x] Add some tests -> unit tests / file system integration tests
- [x] Add extensive integration tests -> file system, state generation
- [x] Implement compression (zstd?) -> zstd! should make this a feature / configurable
- [x] Use tracing crate for distributed trace
- [x] Fix `futures::lock::Mutex` to `tokio::sync::Mutex`
- [x] Improve error handling and propagation -> thiserror + anyhow
- [x] Better initial setup cli guidance -> prompt user to enter certain config rather than referring to config file?



### On the topic of parallel file writes
Very difficult to implement asynchronous, platform agnostic parallel file writes to the same file.  
Overview:  
Instructions may come in from other machine since file reads are very fast.  
A queue of messages with instructions to write to data to various locations will build up.  
Chunks from signature generation are actually cached, available in memory, hence no io limitation.  
Although the file api provided by tokio is asynchronous, underlying io operations are not necessarily.  
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

## Performance

### Rolling Hash Algorithm Comparison

Harmonic uses BuzHash, a pure Rust custom implementation of a rolling hash algorithm optimized for hash distribution and very fast byte rolling. Below is a comparison with rollsum, the weak hashing implementation in rsync:

| Feature | BuzHash (Harmonic) | Bup/Rollsum |
|---------|-------------------|-------------|
| **Hash Output** | 64-bit (u64) | 32-bit (u32) |
| **Collision Resistance** | 2^64 hash space | 2^32 hash space |
| **Birthday Paradox** | ~50% collision after 4.3B hashes | ~50% collision after 65K hashes |
| **Window Size** | Configurable (No set limitations) | Fixed at 64 bytes |
| **Rolling Performance** | ~95-98% of Bup throughput | Baseline |

**Key Performance Implications:**

- **Superior Collision Resistance**: BuzHash provides significantly (2^32x) more hash space, dramatically reducing the need for expensive strong hash calculation. Additionally, the XOR algorithm is optimised for better hash distribution
- **Competitive Speed**: Despite operating on 64-bit values, BuzHash achieves performance within a few percent of 32-bit alternatives
- **Flexibility**: Configurable window sizes allow tuning for different file types and chunk size requirements
- **Trade-off**: Slightly higher CPU/memory overhead (64-bit operations, 2KB lookup table) in exchange for significantly better hash quality


## How to Run

### Configuration

- 1. Download the applicable binary from release page
- 2. On first startup check the configuration file and make the necessary modifications
- 3. Modify config file as per requirements
- 4. Run server binary
- 5. Run client binary


