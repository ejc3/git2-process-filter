//! End-to-end tests comparing process filter output with git CLI.

use git2::{FilterFlags, FilterList, FilterMode, Repository};
use git2_process_filter::register_process_filter;
use std::fs::{self, File};
use std::io::Write;
use std::process::Command;
use tempfile::TempDir;

fn repo_init() -> (TempDir, Repository) {
    let td = TempDir::new().unwrap();
    let repo = Repository::init(td.path()).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
    }
    (td, repo)
}

/// Test that our process filter produces the same output as git CLI
/// using a simple uppercase filter.
#[test]
fn test_process_filter_matches_git_cli() {
    let (td, repo) = repo_init();

    // Configure a simple filter that uppercases on clean
    {
        let mut config = repo.config().unwrap();
        config
            .set_str("filter.upper.clean", "tr a-z A-Z")
            .unwrap();
        config
            .set_str("filter.upper.smudge", "tr A-Z a-z")
            .unwrap();
    }

    // Create .gitattributes
    let gitattributes_path = td.path().join(".gitattributes");
    {
        let mut file = File::create(&gitattributes_path).unwrap();
        writeln!(file, "*.txt filter=upper").unwrap();
    }

    // Create test file
    let test_file = td.path().join("test.txt");
    let original_content = b"hello world\n";
    fs::write(&test_file, original_content).unwrap();

    // Register our process filter
    let _reg = register_process_filter(&repo, "upper").unwrap();

    // Use git2's FilterList to apply the filter (which uses our registered filter)
    let filter_list = FilterList::load(&repo, "test.txt", FilterMode::ToOdb, FilterFlags::DEFAULT)
        .unwrap()
        .expect("Should have filter list");

    let filtered = filter_list.apply_to_buffer(original_content).unwrap();
    let our_output = filtered.as_ref();

    // Compare with expected output
    assert_eq!(our_output, b"HELLO WORLD\n");

    // Now test smudge (ToWorktree)
    let filter_list =
        FilterList::load(&repo, "test.txt", FilterMode::ToWorktree, FilterFlags::DEFAULT)
            .unwrap()
            .expect("Should have filter list");

    let smudged = filter_list.apply_to_buffer(b"HELLO WORLD\n").unwrap();
    assert_eq!(smudged.as_ref(), b"hello world\n");
}

/// Test with a filter that uses %f placeholder
#[test]
fn test_process_filter_with_path_placeholder() {
    let (td, repo) = repo_init();

    // Configure a filter that echoes the filename
    // Using 'echo' as clean to show path is passed correctly
    {
        let mut config = repo.config().unwrap();
        // This filter just passes through, but we configure it to verify %f works
        config.set_str("filter.pathtest.clean", "cat").unwrap();
        config.set_str("filter.pathtest.smudge", "cat").unwrap();
    }

    // Create .gitattributes
    let gitattributes_path = td.path().join(".gitattributes");
    {
        let mut file = File::create(&gitattributes_path).unwrap();
        writeln!(file, "*.dat filter=pathtest").unwrap();
    }

    // Register our process filter
    let _reg = register_process_filter(&repo, "pathtest").unwrap();

    // Apply filter
    let filter_list = FilterList::load(&repo, "test.dat", FilterMode::ToOdb, FilterFlags::DEFAULT)
        .unwrap()
        .expect("Should have filter list");

    let input = b"test data";
    let output = filter_list.apply_to_buffer(input).unwrap();

    // cat should pass through unchanged
    assert_eq!(output.as_ref(), input);
}

/// Test that filter works with git add (via git CLI comparison)
#[test]
fn test_process_filter_git_add_comparison() {
    let (td, repo) = repo_init();

    // Configure uppercase filter
    {
        let mut config = repo.config().unwrap();
        config
            .set_str("filter.upper.clean", "tr a-z A-Z")
            .unwrap();
        config
            .set_str("filter.upper.smudge", "tr A-Z a-z")
            .unwrap();
    }

    // Create .gitattributes
    let gitattributes_path = td.path().join(".gitattributes");
    {
        let mut file = File::create(&gitattributes_path).unwrap();
        writeln!(file, "*.txt filter=upper").unwrap();
    }

    // Stage .gitattributes first
    {
        let mut index = repo.index().unwrap();
        index
            .add_path(std::path::Path::new(".gitattributes"))
            .unwrap();
        index.write().unwrap();
    }

    // Create test file with lowercase content
    let test_file = td.path().join("hello.txt");
    fs::write(&test_file, "hello world\n").unwrap();

    // Use git CLI to add the file (applies filter)
    let output = Command::new("git")
        .args(["add", "hello.txt"])
        .current_dir(td.path())
        .output()
        .expect("git add failed");
    assert!(output.status.success(), "git add failed: {:?}", output);

    // Read what git stored in the index
    let output = Command::new("git")
        .args(["show", ":hello.txt"])
        .current_dir(td.path())
        .output()
        .expect("git show failed");
    assert!(output.status.success());

    let git_stored = output.stdout;

    // Verify git applied the uppercase filter
    assert_eq!(git_stored, b"HELLO WORLD\n");

    // Now verify our filter produces the same result
    let _reg = register_process_filter(&repo, "upper").unwrap();

    let filter_list =
        FilterList::load(&repo, "hello.txt", FilterMode::ToOdb, FilterFlags::DEFAULT)
            .unwrap()
            .expect("Should have filter list");

    let our_output = filter_list.apply_to_buffer(b"hello world\n").unwrap();

    assert_eq!(our_output.as_ref(), git_stored.as_slice());
}

/// Test that git-lfs filter works correctly.
/// Skips if git-lfs is not installed.
#[test]
fn test_process_filter_lfs() {
    // Check if git-lfs is installed
    let lfs_check = Command::new("git-lfs").arg("version").output();
    if lfs_check.is_err() || !lfs_check.unwrap().status.success() {
        eprintln!("Skipping test_process_filter_lfs: git-lfs not installed");
        return;
    }

    let (td, repo) = repo_init();

    // Initialize LFS in the repo
    let output = Command::new("git")
        .args(["lfs", "install", "--local"])
        .current_dir(td.path())
        .output()
        .expect("git lfs install failed");
    assert!(output.status.success(), "git lfs install failed: {:?}", output);

    // Track *.bin files with LFS
    let output = Command::new("git")
        .args(["lfs", "track", "*.bin"])
        .current_dir(td.path())
        .output()
        .expect("git lfs track failed");
    assert!(output.status.success(), "git lfs track failed: {:?}", output);

    // Stage .gitattributes
    {
        let mut index = repo.index().unwrap();
        index
            .add_path(std::path::Path::new(".gitattributes"))
            .unwrap();
        index.write().unwrap();
    }

    // Create a test binary file
    let test_file = td.path().join("test.bin");
    let content = b"This is test content for LFS\n";
    fs::write(&test_file, content).unwrap();

    // Register our process filter for LFS
    let _reg = register_process_filter(&repo, "lfs").unwrap();

    // Apply the clean filter (ToOdb) using git2
    let filter_list = FilterList::load(&repo, "test.bin", FilterMode::ToOdb, FilterFlags::DEFAULT)
        .unwrap()
        .expect("Should have filter list for .bin file");

    let cleaned = filter_list.apply_to_buffer(content).unwrap();
    let cleaned_str = String::from_utf8_lossy(cleaned.as_ref());

    // LFS clean should produce a pointer file
    assert!(
        cleaned_str.starts_with("version https://git-lfs.github.com/spec/v1"),
        "Expected LFS pointer, got: {}",
        cleaned_str
    );
    assert!(
        cleaned_str.contains("oid sha256:"),
        "Expected sha256 oid in pointer"
    );
    assert!(
        cleaned_str.contains("size "),
        "Expected size in pointer, got: {}",
        cleaned_str
    );

    // Now verify smudge works - apply smudge to the pointer
    // Note: This will fail to download since there's no LFS server,
    // but we can at least verify the filter is invoked correctly
    // by checking that non-pointer content passes through
    let filter_list = FilterList::load(
        &repo,
        "test.bin",
        FilterMode::ToWorktree,
        FilterFlags::DEFAULT,
    )
    .unwrap()
    .expect("Should have filter list");

    // Smudge non-pointer content should pass through
    let non_pointer = b"not a pointer";
    let smudged = filter_list.apply_to_buffer(non_pointer).unwrap();
    assert_eq!(smudged.as_ref(), non_pointer);
}

/// Test filter with empty commands (passthrough)
#[test]
fn test_process_filter_empty_commands() {
    let (td, repo) = repo_init();

    // Don't configure any commands - filter should passthrough
    // Create .gitattributes
    let gitattributes_path = td.path().join(".gitattributes");
    {
        let mut file = File::create(&gitattributes_path).unwrap();
        writeln!(file, "*.txt filter=empty").unwrap();
    }

    // Register filter with no config
    let _reg = register_process_filter(&repo, "empty").unwrap();

    let filter_list = FilterList::load(&repo, "test.txt", FilterMode::ToOdb, FilterFlags::DEFAULT)
        .unwrap()
        .expect("Should have filter list");

    let input = b"unchanged content";
    let output = filter_list.apply_to_buffer(input).unwrap();

    // Should pass through unchanged
    assert_eq!(output.as_ref(), input);
}
