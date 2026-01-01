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

use git2::{
    filter_priority, filter_register, Error, Filter, FilterMode, FilterRegistration, FilterSource,
};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Default timeout for filter commands (5 minutes).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum buffer size before switching to streaming (64KB).
const STREAM_THRESHOLD: usize = 64 * 1024;

/// A filter that shells out to external commands configured in git config.
struct ProcessFilter {
    clean_cmd: String,
    smudge_cmd: String,
}

impl ProcessFilter {
    /// Parse a filter command, handling the `%f` placeholder and quoted arguments.
    ///
    /// Supports:
    /// - Simple commands: `cat`
    /// - Commands with args: `git-lfs clean -- %f`
    /// - Quoted arguments: `foo "arg with spaces" bar`
    /// - Single quotes: `foo 'arg with spaces' bar`
    fn parse_command(cmd: &str, path: &str) -> (String, Vec<String>) {
        let cmd = cmd.replace("%f", path);
        let mut args = Vec::new();
        let mut current = String::new();
        let mut in_double_quote = false;
        let mut in_single_quote = false;

        for c in cmd.chars() {
            match c {
                '"' if !in_single_quote => {
                    in_double_quote = !in_double_quote;
                }
                '\'' if !in_double_quote => {
                    in_single_quote = !in_single_quote;
                }
                ' ' | '\t' if !in_double_quote && !in_single_quote => {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                }
                _ => {
                    current.push(c);
                }
            }
        }
        if !current.is_empty() {
            args.push(current);
        }

        if args.is_empty() {
            return (String::new(), vec![]);
        }
        let program = args.remove(0);
        (program, args)
    }

    fn run_command(
        cmd: &str,
        path: &str,
        workdir: Option<&Path>,
        input: &[u8],
    ) -> Result<Vec<u8>, Error> {
        if cmd.is_empty() {
            return Ok(input.to_vec());
        }

        let (program, args) = Self::parse_command(cmd, path);
        if program.is_empty() {
            return Ok(input.to_vec());
        }

        let mut command = Command::new(&program);
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Set working directory so external tools like git-lfs can find .git
        if let Some(dir) = workdir {
            command.current_dir(dir);
        }

        let mut child = command
            .spawn()
            .map_err(|e| Error::from_str(&format!("failed to spawn '{}': {}", program, e)))?;

        // For large inputs, use streaming to avoid loading everything in memory
        let use_streaming = input.len() > STREAM_THRESHOLD;

        if use_streaming {
            Self::run_streaming(&program, &mut child, input)
        } else {
            Self::run_buffered(&program, &mut child, input)
        }
    }

    /// Run command with full buffering (for small inputs).
    fn run_buffered(
        program: &str,
        child: &mut std::process::Child,
        input: &[u8],
    ) -> Result<Vec<u8>, Error> {
        // Write input to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input)
                .map_err(|e| Error::from_str(&format!("failed to write to stdin: {}", e)))?;
        }

        // Read stdout and stderr
        let mut stdout_data = Vec::new();
        let mut stderr_data = Vec::new();

        if let Some(mut stdout) = child.stdout.take() {
            stdout
                .read_to_end(&mut stdout_data)
                .map_err(|e| Error::from_str(&format!("failed to read stdout: {}", e)))?;
        }
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_end(&mut stderr_data);
        }

        // Wait for process with timeout
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Log stderr as warning if present (even on success)
                    if !stderr_data.is_empty() {
                        let stderr_str = String::from_utf8_lossy(&stderr_data);
                        if status.success() {
                            eprintln!(
                                "[git2-process-filter] {} warning: {}",
                                program,
                                stderr_str.trim()
                            );
                        }
                    }

                    if status.success() {
                        return Ok(stdout_data);
                    } else {
                        let stderr_str = String::from_utf8_lossy(&stderr_data);
                        return Err(Error::from_str(&format!(
                            "'{}' failed: {}",
                            program,
                            stderr_str.trim()
                        )));
                    }
                }
                Ok(None) => {
                    if start.elapsed() > DEFAULT_TIMEOUT {
                        let _ = child.kill();
                        return Err(Error::from_str(&format!(
                            "'{}' timed out after {:?}",
                            program, DEFAULT_TIMEOUT
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    return Err(Error::from_str(&format!(
                        "failed to wait for '{}': {}",
                        program, e
                    )));
                }
            }
        }
    }

    /// Run command with streaming (for large inputs).
    fn run_streaming(
        program: &str,
        child: &mut std::process::Child,
        input: &[u8],
    ) -> Result<Vec<u8>, Error> {
        use std::thread;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::from_str("failed to open stdin"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::from_str("failed to open stdout"))?;
        let stderr = child.stderr.take();

        // Write input in a separate thread to avoid deadlock
        let input_owned = input.to_vec();
        let write_handle = thread::spawn(move || {
            let result = stdin.write_all(&input_owned);
            drop(stdin); // Close stdin to signal EOF
            result
        });

        // Read output
        let mut output = Vec::new();
        stdout
            .read_to_end(&mut output)
            .map_err(|e| Error::from_str(&format!("failed to read stdout: {}", e)))?;

        // Wait for write to complete
        write_handle
            .join()
            .map_err(|_| Error::from_str("write thread panicked"))?
            .map_err(|e| Error::from_str(&format!("failed to write to stdin: {}", e)))?;

        // Read stderr
        let mut stderr_output = Vec::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_end(&mut stderr_output);
        }

        // Wait for process
        let status = child
            .wait()
            .map_err(|e| Error::from_str(&format!("failed to wait for '{}': {}", program, e)))?;

        if status.success() {
            // Log stderr as warning if present
            if !stderr_output.is_empty() {
                let stderr_str = String::from_utf8_lossy(&stderr_output);
                eprintln!(
                    "[git2-process-filter] {} warning: {}",
                    program,
                    stderr_str.trim()
                );
            }
            Ok(output)
        } else {
            let stderr_str = String::from_utf8_lossy(&stderr_output);
            Err(Error::from_str(&format!(
                "'{}' failed: {}",
                program,
                stderr_str.trim()
            )))
        }
    }
}

impl Filter for ProcessFilter {
    fn apply(&self, src: &FilterSource<'_>, input: &[u8]) -> Result<Vec<u8>, Error> {
        let path = src.path().unwrap_or("");
        let workdir = src.workdir();
        match src.mode() {
            FilterMode::ToOdb => {
                Self::run_command(&self.clean_cmd, path, workdir.as_deref(), input)
            }
            FilterMode::ToWorktree => {
                Self::run_command(&self.smudge_cmd, path, workdir.as_deref(), input)
            }
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

/// Register a filter with explicit clean and smudge commands.
///
/// This is useful when you want to specify commands directly without
/// reading from git config.
///
/// # Arguments
///
/// * `name` - The filter name (used in .gitattributes as `filter=<name>`)
/// * `clean_cmd` - Command to run for clean (worktree → ODB), or empty for passthrough
/// * `smudge_cmd` - Command to run for smudge (ODB → worktree), or empty for passthrough
///
/// # Returns
///
/// A [`FilterRegistration`] handle. The filter remains active until this handle is dropped.
///
/// # Example
///
/// ```no_run
/// use git2_process_filter::register_process_filter_with_commands;
///
/// // Register a custom uppercase filter
/// let _reg = register_process_filter_with_commands(
///     "upper",
///     "tr a-z A-Z",
///     "tr A-Z a-z",
/// )?;
///
/// // Filter is now active for files with `filter=upper` in .gitattributes
/// # Ok::<(), git2::Error>(())
/// ```
pub fn register_process_filter_with_commands(
    name: &str,
    clean_cmd: &str,
    smudge_cmd: &str,
) -> Result<FilterRegistration, Error> {
    let filter = ProcessFilter {
        clean_cmd: clean_cmd.to_string(),
        smudge_cmd: smudge_cmd.to_string(),
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
            config.set_str("filter.myfilter.clean", "cat").unwrap();
            config.set_str("filter.myfilter.smudge", "cat").unwrap();
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
        let result = ProcessFilter::run_command("cat", "", None, input);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), input);
    }

    #[test]
    fn test_run_command_empty() {
        let input = b"hello world";
        let result = ProcessFilter::run_command("", "", None, input);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), input);
    }

    #[test]
    fn test_parse_command_double_quotes() {
        let (prog, args) = ProcessFilter::parse_command(r#"echo "hello world" foo"#, "");
        assert_eq!(prog, "echo");
        assert_eq!(args, vec!["hello world", "foo"]);
    }

    #[test]
    fn test_parse_command_single_quotes() {
        let (prog, args) = ProcessFilter::parse_command("echo 'hello world' foo", "");
        assert_eq!(prog, "echo");
        assert_eq!(args, vec!["hello world", "foo"]);
    }

    #[test]
    fn test_parse_command_mixed_quotes() {
        let (prog, args) = ProcessFilter::parse_command(r#"cmd "arg 1" 'arg 2' arg3"#, "");
        assert_eq!(prog, "cmd");
        assert_eq!(args, vec!["arg 1", "arg 2", "arg3"]);
    }

    #[test]
    fn test_parse_command_placeholder_in_quotes() {
        let (prog, args) = ProcessFilter::parse_command(r#"git-lfs clean "%f""#, "my file.bin");
        assert_eq!(prog, "git-lfs");
        assert_eq!(args, vec!["clean", "my file.bin"]);
    }

    #[test]
    fn test_run_command_streaming_large_input() {
        // Create input larger than STREAM_THRESHOLD (64KB)
        let input: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        let result = ProcessFilter::run_command("cat", "", None, &input);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), input);
    }

    #[test]
    fn test_register_with_commands() {
        let result = register_process_filter_with_commands("testcmd", "cat", "cat");
        assert!(result.is_ok());
    }
}
