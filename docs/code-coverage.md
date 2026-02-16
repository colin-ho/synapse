# Code Coverage

This repository uses [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) for coverage reports.

## Local usage

1. Install the tool once:
   - `cargo install cargo-llvm-cov --locked`
2. Generate reports:
   - `./scripts/coverage`
3. Open the HTML report:
   - `coverage/html/index.html`

The script also writes an LCOV file at `coverage/lcov.info`.

## CI usage

Coverage is generated in GitHub Actions (`.github/workflows/ci.yml`) in the `coverage` job.
The workflow uploads:

- `coverage-lcov` artifact (`coverage/lcov.info`)
- `coverage-html` artifact (`coverage/html`)
