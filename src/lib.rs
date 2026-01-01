//! External process filter support for git2.
//!
//! This crate provides [`register_process_filter`], which reads filter commands
//! from git config and shells out to them for clean/smudge operations.
//!
//! # Example
//!
//! ```no_run
//! use git2::Repository;
//! use git2_process_filter::register_process_filter;
//!
//! let repo = Repository::open(".")?;
//!
//! // Registers filter that runs commands from git config:
//! //   filter.lfs.clean = git-lfs clean -- %f
//! //   filter.lfs.smudge = git-lfs smudge -- %f
//! let _reg = register_process_filter(&repo, "lfs")?;
//!
//! // Filter is now active for files with `filter=lfs` in .gitattributes
//! // All git2 operations will automatically shell out to git-lfs
//! # Ok::<(), git2::Error>(())
//! ```

use git2::{filter_priority, filter_register, Error, Filter, FilterMode, FilterRegistration, FilterSource};
use std::io::Write;
use std::process::{Command, Stdio};

/// A filter that shells out to external commands configured in git config.
struct ProcessFilter {
    clean_cmd: String,
    smudge_cmd: String,
}

impl ProcessFilter {
    /// Parse a filter command, handling the `%f` placeholder for the file path.
    fn parse_command(cmd: &str, path: &str) -> (String, Vec<String>) {
        let cmd = cmd.replace("%f", path);
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return (String::new(), vec![]);
        }
        let program = parts[0].to_string();
        let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
        (program, args)
    }

    fn run_command(cmd: &str, path: &str, input: &[u8]) -> Result<Vec<u8>, Error> {
        if cmd.is_empty() {
            return Ok(input.to_vec());
        }

        let (program, args) = Self::parse_command(cmd, path);
        if program.is_empty() {
            return Ok(input.to_vec());
        }

        let mut child = Command::new(&program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::from_str(&format!("failed to spawn '{}': {}", program, e)))?;

        // Write input to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input)
                .map_err(|e| Error::from_str(&format!("failed to write to stdin: {}", e)))?;
        }

        // Wait for output
        let output = child
            .wait_with_output()
            .map_err(|e| Error::from_str(&format!("failed to wait for '{}': {}", program, e)))?;

        if output.status.success() {
            Ok(output.stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(Error::from_str(&format!(
                "'{}' failed: {}",
                program,
                stderr.trim()
            )))
        }
    }
}

impl Filter for ProcessFilter {
    fn apply(&self, src: &FilterSource<'_>, input: &[u8]) -> Result<Vec<u8>, Error> {
        let path = src.path().unwrap_or("");
        match src.mode() {
            FilterMode::ToOdb => Self::run_command(&self.clean_cmd, path, input),
            FilterMode::ToWorktree => Self::run_command(&self.smudge_cmd, path, input),
        }
    }
}

/// Register a filter that shells out to commands from git config.
///
/// Reads `filter.<name>.clean` and `filter.<name>.smudge` from the repository's
/// config and registers a filter that executes those commands.
///
/// # Arguments
///
/// * `repo` - The repository to read config from
/// * `name` - The filter name (e.g., "lfs" reads `filter.lfs.clean` and `filter.lfs.smudge`)
///
/// # Returns
///
/// A [`FilterRegistration`] handle. The filter remains active until this handle is dropped.
///
/// # Example
///
/// ```no_run
/// use git2::Repository;
/// use git2_process_filter::register_process_filter;
///
/// let repo = Repository::open(".")?;
///
/// // Registers filter that runs commands from:
/// //   filter.lfs.clean = git-lfs clean -- %f
/// //   filter.lfs.smudge = git-lfs smudge -- %f
/// let _reg = register_process_filter(&repo, "lfs")?;
///
/// // Filter is now active for files with `filter=lfs` in .gitattributes
/// # Ok::<(), git2::Error>(())
/// ```
pub fn register_process_filter(
    repo: &git2::Repository,
    name: &str,
) -> Result<FilterRegistration, Error> {
    let config = repo.config()?;

    let clean_key = format!("filter.{}.clean", name);
    let smudge_key = format!("filter.{}.smudge", name);

    let clean_cmd = config.get_string(&clean_key).unwrap_or_default();
    let smudge_cmd = config.get_string(&smudge_key).unwrap_or_default();

    let filter = ProcessFilter {
        clean_cmd,
        smudge_cmd,
    };

    let attributes = format!("filter={}", name);
    filter_register(name, &attributes, filter_priority::DRIVER, filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Repository;
    use tempfile::TempDir;

    fn repo_init() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let repo = Repository::init(td.path()).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "name").unwrap();
            config.set_str("user.email", "email").unwrap();
        }
        (td, repo)
    }

    #[test]
    fn test_register_process_filter_no_config() {
        let (_td, repo) = repo_init();

        // Register filter with no config - should succeed but commands are empty
        let result = register_process_filter(&repo, "testfilter");
        assert!(result.is_ok());
    }

    #[test]
    fn test_register_process_filter_with_config() {
        let (_td, repo) = repo_init();

        // Set up filter config
        {
            let mut config = repo.config().unwrap();
            config
                .set_str("filter.myfilter.clean", "cat")
                .unwrap();
            config
                .set_str("filter.myfilter.smudge", "cat")
                .unwrap();
        }

        let result = register_process_filter(&repo, "myfilter");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_command() {
        let (prog, args) = ProcessFilter::parse_command("git-lfs clean -- %f", "test.bin");
        assert_eq!(prog, "git-lfs");
        assert_eq!(args, vec!["clean", "--", "test.bin"]);
    }

    #[test]
    fn test_parse_command_no_placeholder() {
        let (prog, args) = ProcessFilter::parse_command("cat", "test.bin");
        assert_eq!(prog, "cat");
        assert!(args.is_empty());
    }

    #[test]
    fn test_run_command_cat() {
        let input = b"hello world";
        let result = ProcessFilter::run_command("cat", "", input);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), input);
    }

    #[test]
    fn test_run_command_empty() {
        let input = b"hello world";
        let result = ProcessFilter::run_command("", "", input);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), input);
    }
}
