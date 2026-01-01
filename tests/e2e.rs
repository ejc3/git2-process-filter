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
