use futures::pin_mut;
use harmonic::{common::*, harmonic::FileStatus};
use std::{fs, path::PathBuf};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_stream::StreamExt;

mod common;

#[test]
fn test_generate_state_with_real_files() {
    // Create a temporary directory with some test files
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    // Create test files
    let file1 = root.join("file1.txt");
    let file2 = root.join("file2.md");

    fs::write(&file1, "This is file 1 content").unwrap();
    fs::write(&file2, "# This is file 2 content").unwrap();

    // Create a subdirectory with a file
    let subdir = root.join("subdir");
    fs::create_dir(&subdir).unwrap();
    let file3 = subdir.join("file3.txt");
    fs::write(&file3, "This is file 3 in subdirectory").unwrap();

    // Generate state
    let state = generate_state(&root).unwrap();

    // Verify state was created with a valid timestamp
    assert!(state.last_sync_timestamp_micros > 0);

    // Create another state from empty directory to compare
    let empty_dir = tempdir().unwrap();
    let empty_root = PathBuf::from(empty_dir.path());
    let empty_state = generate_state(&empty_root).unwrap();

    // Comparing empty state with our state should show 3 additions
    let diffs = compare_states(&empty_state, &state);
    assert_eq!(diffs.len(), 3);
    assert_eq!(
        diffs
            .iter()
            .filter(|d| matches!(d.change, ChangeType::Added))
            .count(),
        3
    );
}

#[test]
fn test_generate_state_empty_directory() {
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    let state = generate_state(&root).unwrap();

    // Verify timestamp is set
    assert!(state.last_sync_timestamp_micros > 0);

    // Verify empty by comparing states
    let state2 = generate_state(&root).unwrap();
    let diffs = compare_states(&state, &state2);
    assert_eq!(diffs.len(), 0);
}

#[test]
fn test_compare_states_with_real_file_addition() {
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    // Generate initial state
    let state1 = generate_state(&root).unwrap();

    // Add a new file
    let new_file = root.join("new_file.txt");
    fs::write(&new_file, "New content").unwrap();

    // Generate new state
    let state2 = generate_state(&root).unwrap();

    // Compare states
    let diffs = compare_states(&state1, &state2);

    assert_eq!(diffs.len(), 1);
    assert!(matches!(diffs[0].change, ChangeType::Added));

    // Verify path by converting diffs to FileStatus vec
    let file_statuses: Vec<FileStatus> =
        diffs
        .into_iter()
        .map(|d| FileStatus::try_from(d))
        .collect::<Result<Vec<FileStatus>, _>>()
        .unwrap();
    assert_eq!(file_statuses[0].path, "new_file.txt");
}

#[test]
fn test_compare_states_with_real_file_modification() {
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    // Create initial file
    let file = root.join("modified_file.txt");
    fs::write(&file, "Original content").unwrap();

    // Generate initial state
    let state1 = generate_state(&root).unwrap();

    // Wait a bit to ensure timestamp changes
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Modify the file
    fs::write(&file, "Modified content - different hash").unwrap();

    // Generate new state
    let state2 = generate_state(&root).unwrap();

    // Compare states
    let diffs = compare_states(&state1, &state2);

    assert_eq!(diffs.len(), 1);
    assert!(matches!(diffs[0].change, ChangeType::Modified));

    // Verify path by converting diffs to FileStatus vec
    let file_statuses: Vec<FileStatus> =
        diffs
        .into_iter()
        .map(|d| FileStatus::try_from(d))
        .collect::<Result<Vec<FileStatus>, _>>()
        .unwrap();
    assert_eq!(file_statuses[0].path, "modified_file.txt");
}

#[test]
fn test_compare_states_with_real_file_removal() {
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    // Create initial file
    let file = root.join("to_be_removed.txt");
    fs::write(&file, "This will be removed").unwrap();

    // Generate initial state
    let state1 = generate_state(&root).unwrap();

    // Remove the file
    fs::remove_file(&file).unwrap();

    // Generate new state
    let state2 = generate_state(&root).unwrap();

    // Compare states
    let diffs = compare_states(&state1, &state2);

    assert_eq!(diffs.len(), 1);
    assert!(matches!(diffs[0].change, ChangeType::Removed));

    // Verify path by converting diffs to FileStatus vec
    let file_statuses: Vec<FileStatus> = diffs
        .into_iter()
        .map(|d| FileStatus::try_from(d))
        .collect::<Result<Vec<FileStatus>, _>>()
        .unwrap();
    assert_eq!(file_statuses[0].path, "to_be_removed.txt");
}

#[test]
fn test_compare_states_with_multiple_changes() {
    let dir = tempdir().unwrap();
    let root = PathBuf::from(dir.path());

    // Create initial files
    let file1 = root.join("keep_this.txt");
    let file2 = root.join("modify_this.txt");
    let file3 = root.join("remove_this.txt");

    fs::write(&file1, "Unchanged content").unwrap();
    fs::write(&file2, "Original content").unwrap();
    fs::write(&file3, "Will be removed").unwrap();

    // Generate initial state
    let state1 = generate_state(&root).unwrap();

    // Wait to ensure timestamp changes
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Make changes
    fs::write(&file2, "Modified content!").unwrap(); // Modified
    fs::remove_file(&file3).unwrap(); // Removed
    let file4 = root.join("new_file.txt");
    fs::write(&file4, "New file added").unwrap(); // Added

    // Generate new state
    let state2 = generate_state(&root).unwrap();

    // Compare states
    let diffs = compare_states(&state1, &state2);

    // Should have 3 changes: 1 modified, 1 removed, 1 added
    assert_eq!(diffs.len(), 3);

    let added_count = diffs
        .iter()
        .filter(|d| matches!(d.change, ChangeType::Added))
        .count();
    let modified_count = diffs
        .iter()
        .filter(|d| matches!(d.change, ChangeType::Modified))
        .count();
    let removed_count = diffs
        .iter()
        .filter(|d| matches!(d.change, ChangeType::Removed))
        .count();

    assert_eq!(added_count, 1);
    assert_eq!(modified_count, 1);
    assert_eq!(removed_count, 1);
}

#[tokio::test]
async fn test_get_file_creates_new_file() {
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let file_path = root_path.join("test_file.txt");

    let file_sync = harmonic::harmonic::FileSync {
        path: "test_file.txt".to_string(),
        chunk: vec![],
        offset: 0,
        is_final: false,
        file_size: 1024,
    };

    let file = get_file(&file_sync, &root_path).await.unwrap();

    // Verify file was created
    assert!(file_path.exists());

    // Verify file size was set
    let metadata = file.metadata().await.unwrap();
    assert_eq!(metadata.len(), 1024);
}

#[tokio::test]
async fn test_write_data_to_offset() {
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let file_path = root_path.join("offset_test.txt");
    let path = "offset_test.txt".to_string();

    // Create file with specific size
    let file_sync = harmonic::harmonic::FileSync {
        path: path.clone(),
        chunk: vec![],
        offset: 0,
        is_final: false,
        file_size: 100,
    };

    let mut file = get_file(&file_sync, &root_path).await.unwrap();

    // Write data at offset 0
    let data1 = harmonic::harmonic::FileSync {
        path: path.clone(),
        chunk: vec![1, 2, 3, 4, 5],
        offset: 0,
        is_final: false,
        file_size: 100,
    };
    write_data_to_offset(data1, &mut file).await.unwrap();

    // Write data at offset 10
    let data2 = harmonic::harmonic::FileSync {
        path: path.clone(),
        chunk: vec![6, 7, 8, 9, 10],
        offset: 10,
        is_final: false,
        file_size: 100,
    };
    write_data_to_offset(data2, &mut file).await.unwrap();

    // Flush and sync to ensure data is written
    file.sync_all().await.unwrap();

    // Close the write file and reopen for reading
    drop(file);

    // Open file for reading and verify data was written at correct offsets
    let mut read_file = tokio::fs::File::open(&file_path).await.unwrap();

    read_file.seek(std::io::SeekFrom::Start(0)).await.unwrap();
    let mut buffer = vec![0u8; 5];
    read_file.read_exact(&mut buffer).await.unwrap();
    assert_eq!(buffer, vec![1, 2, 3, 4, 5]);

    read_file.seek(std::io::SeekFrom::Start(10)).await.unwrap();
    let mut buffer2 = vec![0u8; 5];
    read_file.read_exact(&mut buffer2).await.unwrap();
    assert_eq!(buffer2, vec![6, 7, 8, 9, 10]);
}

#[tokio::test]
async fn test_file_to_chunked_file_sync() {
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let file_path = root_path.join("chunk_test.txt");

    // Create a file larger than chunk size (8192 bytes)
    let content = "x".repeat(20000); // 20KB file
    fs::write(&file_path, &content).unwrap();

    let relative_path = PathBuf::from("chunk_test.txt");
    let stream = file_to_chunked_file_sync(&relative_path, &root_path);
    pin_mut!(stream);

    let mut total_bytes = 0;
    let mut chunk_count = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        chunk_count += 1;
        total_bytes += chunk.chunk.len() as u64;

        // Verify chunk properties
        assert_eq!(chunk.path, "chunk_test.txt");
        assert_eq!(chunk.file_size, 20000);
        assert!(chunk.chunk.len() <= 8192);
    }

    // Verify all data was read
    assert_eq!(total_bytes, 20000);
    // Should be 3 chunks: 8192 + 8192 + 3616
    assert_eq!(chunk_count, 3);
}

#[tokio::test]
async fn test_file_to_chunked_file_sync_small_file() {
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let file_path = root_path.join("small_file.txt");

    // Create a small file (less than chunk size)
    let content = "Small file content";
    fs::write(&file_path, &content).unwrap();

    let relative_path = PathBuf::from("small_file.txt");
    let stream = file_to_chunked_file_sync(&relative_path, &root_path);
    pin_mut!(stream);

    let mut chunks = vec![];
    while let Some(chunk) = stream.next().await {
        chunks.push(chunk.unwrap());
    }

    // Should be exactly 1 chunk
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chunk, content.as_bytes());
    assert_eq!(chunks[0].file_size, content.len() as u64);
}

#[tokio::test]
async fn test_roundtrip_file_chunking_and_writing() {
    let dir = tempdir().unwrap();
    let root_path = PathBuf::from(dir.path());
    let source_file = root_path.join("source.txt");
    let dest_file = root_path.join("destination.txt");

    // Create source file with known content
    let original_content =
        "This is a test file with some content that will be chunked and reassembled.".repeat(200);
    fs::write(&source_file, &original_content).unwrap();

    // Get file size
    let file_size = fs::metadata(&source_file).unwrap().len();

    // Create destination file
    let file_sync_init = harmonic::harmonic::FileSync {
        path: "destination.txt".to_string(),
        chunk: vec![],
        offset: 0,
        is_final: false,
        file_size,
    };
    let mut dest = get_file(&file_sync_init, &root_path).await.unwrap();

    // Read source in chunks and write to destination
    let relative_source = PathBuf::from("source.txt");
    let stream = file_to_chunked_file_sync(&relative_source, &root_path);
    pin_mut!(stream);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        write_data_to_offset(chunk, &mut dest).await.unwrap();
    }

    // Close the file
    drop(dest);

    // Verify files are identical
    let source_content = fs::read(&source_file).unwrap();
    let dest_content = fs::read(&dest_file).unwrap();
    assert_eq!(source_content, dest_content);

    // Verify hashes match -> updated to blake3
    let source_hash: [u8; 32] = *blake3::hash(&source_content).as_bytes();
    let dest_hash: [u8; 32] = *blake3::hash(&dest_content).as_bytes();
    assert_eq!(source_hash, dest_hash);
}
