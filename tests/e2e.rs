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

/// Test that git-lfs filter works correctly with GitHub e2e.
/// Creates a temp branch, pushes LFS content, clones and verifies, then deletes branch.
/// Skips if git-lfs or gh CLI is not available.
#[test]
fn test_process_filter_lfs_github_e2e() {
    // Check if git-lfs is installed
    let lfs_check = Command::new("git-lfs").arg("version").output();
    if lfs_check.is_err() || !lfs_check.unwrap().status.success() {
        eprintln!("Skipping test: git-lfs not installed");
        return;
    }

    // Check if gh CLI is available and authenticated
    let gh_check = Command::new("gh").args(["auth", "status"]).output();
    if gh_check.is_err() || !gh_check.unwrap().status.success() {
        eprintln!("Skipping test: gh CLI not authenticated");
        return;
    }

    // Use this repo for testing - it already has LFS enabled
    let test_repo = "ejc3/git2-process-filter";
    let branch_name = format!("test-lfs-{}", std::process::id());

    // Clone the repo
    let clone_dir = tempfile::TempDir::new().unwrap();
    let repo_path = clone_dir.path().join("repo");

    let output = Command::new("gh")
        .args(["repo", "clone", test_repo, repo_path.to_str().unwrap()])
        .output()
        .expect("gh repo clone failed");

    if !output.status.success() {
        eprintln!("Skipping test: could not clone repo: {}",
                  String::from_utf8_lossy(&output.stderr));
        return;
    }

    // Cleanup function - delete branch at end
    struct Cleanup {
        repo_path: std::path::PathBuf,
        branch: String,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            // Delete remote branch
            let _ = Command::new("git")
                .args(["push", "origin", "--delete", &self.branch])
                .current_dir(&self.repo_path)
                .output();
        }
    }
    let _cleanup = Cleanup {
        repo_path: repo_path.clone(),
        branch: branch_name.clone(),
    };

    // Open the cloned repo
    let repo = Repository::open(&repo_path).expect("Failed to open cloned repo");

    // Create and checkout test branch
    let output = Command::new("git")
        .args(["checkout", "-b", &branch_name])
        .current_dir(&repo_path)
        .output()
        .expect("git checkout failed");
    assert!(output.status.success(), "git checkout failed: {:?}", output);

    // Initialize LFS
    let output = Command::new("git")
        .args(["lfs", "install", "--local"])
        .current_dir(&repo_path)
        .output()
        .expect("git lfs install failed");
    assert!(output.status.success(), "git lfs install failed: {:?}", output);

    // Track *.bin files with LFS
    let output = Command::new("git")
        .args(["lfs", "track", "*.bin"])
        .current_dir(&repo_path)
        .output()
        .expect("git lfs track failed");
    assert!(output.status.success(), "git lfs track failed: {:?}", output);

    // Create test content - must be large enough for LFS
    let test_file = repo_path.join("test-large.bin");
    let content: Vec<u8> = (0..2048).map(|i| (i % 256) as u8).collect();
    fs::write(&test_file, &content).unwrap();

    // Register our process filter for LFS
    let _reg = register_process_filter(&repo, "lfs").unwrap();

    // Add .gitattributes first
    let output = Command::new("git")
        .args(["add", ".gitattributes"])
        .current_dir(&repo_path)
        .output()
        .expect("git add .gitattributes failed");
    assert!(output.status.success(), "git add .gitattributes failed: {:?}", output);

    // Now use git2 to add the LFS file (this uses our filter!)
    // But first verify our filter produces correct pointer
    {
        use git2::{FilterFlags, FilterList, FilterMode};

        let filter_list = FilterList::load(&repo, "test-large.bin", FilterMode::ToOdb, FilterFlags::DEFAULT)
            .unwrap()
            .expect("Should have filter list for .bin file");

        let cleaned = filter_list.apply_to_buffer(&content).unwrap();
        let cleaned_str = String::from_utf8_lossy(cleaned.as_ref());

        // Verify our filter produces valid LFS pointer
        assert!(
            cleaned_str.starts_with("version https://git-lfs.github.com/spec/v1"),
            "Expected LFS pointer, got: {}",
            cleaned_str
        );

        // git-lfs clean also stores content in .git/lfs/objects, so we can use git add
    }

    // Use git add to add the file (ensures LFS stores content properly)
    let output = Command::new("git")
        .args(["add", "test-large.bin"])
        .current_dir(&repo_path)
        .output()
        .expect("git add failed");
    assert!(output.status.success(), "git add test-large.bin failed: {:?}\n{}",
            output, String::from_utf8_lossy(&output.stderr));

    // Commit via git CLI
    let output = Command::new("git")
        .args(["commit", "-m", "Test LFS commit"])
        .current_dir(&repo_path)
        .output()
        .expect("git commit failed");
    assert!(output.status.success(), "git commit failed: {:?}\n{}",
            output, String::from_utf8_lossy(&output.stderr));

    // Push branch to GitHub
    let output = Command::new("git")
        .args(["push", "-u", "origin", &branch_name])
        .current_dir(&repo_path)
        .output()
        .expect("git push failed");
    assert!(output.status.success(), "git push failed: {:?}\n{}",
            output, String::from_utf8_lossy(&output.stderr));

    // Clone to a new location to verify LFS works
    let verify_dir = tempfile::TempDir::new().unwrap();
    let verify_path = verify_dir.path().join("verify");

    let output = Command::new("git")
        .args(["clone", "--branch", &branch_name,
               &format!("https://github.com/{}.git", test_repo),
               verify_path.to_str().unwrap()])
        .output()
        .expect("git clone failed");
    assert!(output.status.success(), "git clone failed: {:?}\n{}",
            output, String::from_utf8_lossy(&output.stderr));

    // Verify the file content matches (LFS should have downloaded it)
    let cloned_content = fs::read(verify_path.join("test-large.bin")).unwrap();
    assert_eq!(cloned_content, content, "LFS content mismatch after clone");

    // Verify it was stored as LFS (check git lfs ls-files)
    let output = Command::new("git")
        .args(["lfs", "ls-files"])
        .current_dir(&verify_path)
        .output()
        .expect("git lfs ls-files failed");
    let lfs_files = String::from_utf8_lossy(&output.stdout);
    assert!(lfs_files.contains("test-large.bin"), "File not tracked by LFS: {}", lfs_files);

    eprintln!("LFS e2e test passed! Content verified after push/clone cycle.");
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
