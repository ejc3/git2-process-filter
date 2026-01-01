# git2-process-filter

External process filter support for git2, enabling seamless integration with tools like git-lfs, git-crypt, and other external filters.

## Usage

```rust
use git2::Repository;
use git2_process_filter::register_process_filter;

let repo = Repository::open(".")?;

// Register LFS filter - reads commands from git config:
//   filter.lfs.clean = git-lfs clean -- %f
//   filter.lfs.smudge = git-lfs smudge -- %f
let _reg = register_process_filter(&repo, "lfs")?;

// All git2 operations now automatically shell out to git-lfs
// for files with `filter=lfs` in .gitattributes
```

## How It Works

1. Reads `filter.<name>.clean` and `filter.<name>.smudge` from git config
2. Registers a git2 filter that shells out to those commands
3. Handles `%f` placeholder for file path substitution
4. Empty/missing commands pass through unchanged

## Test Strategy

### Unit Tests (6 tests)

Located in `src/lib.rs`:

| Test | Purpose |
|------|---------|
| `test_register_process_filter_no_config` | Filter registers even with no config |
| `test_register_process_filter_with_config` | Filter reads config correctly |
| `test_parse_command` | `%f` placeholder substitution works |
| `test_parse_command_no_placeholder` | Commands without `%f` work |
| `test_run_command_cat` | External process execution works |
| `test_run_command_empty` | Empty commands pass through |

### E2E Tests (5 tests)

Located in `tests/e2e.rs`, these compare our output with git CLI:

| Test | Purpose |
|------|---------|
| `test_process_filter_matches_git_cli` | Verify uppercase filter produces same output as expected |
| `test_process_filter_with_path_placeholder` | Verify `%f` path handling |
| `test_process_filter_git_add_comparison` | Compare with actual `git add` output |
| `test_process_filter_lfs` | Verify git-lfs produces valid pointer (skips if not installed) |
| `test_process_filter_empty_commands` | Verify passthrough behavior |

### Running Tests

```bash
# All tests
cargo test

# Just unit tests
cargo test --lib

# Just e2e tests
cargo test --test e2e

# With output
cargo test -- --nocapture
```

## Dependencies

- Requires `git2` with filter registration support (our fork at `github.com/ejc3/git2-rs`)
- Uses standard library only for process execution (no async)

## License

MIT OR Apache-2.0
