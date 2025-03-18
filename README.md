# potsi

# Installation

## Pre-requisites

### asdf
```sh
brew install asdf
```

### Fastly
```sh
brew install fastly/tap/fastly
```

## rust
```sh
asdf plugin add rust
asdf install rust 1.83.0
asdf reshim
```

## viceroy (for running tests)
```sh
cargo install viceroy
```

## Build

```sh
cargo build
```

## Run

### Fastly
- Review configuration for [local_server](fastly.toml#L16)

- Run it with

```sh
fastly -i compute serve
```

## Test
```
cargo test
```

Note: if test fails `viceroy` will not display line number of the failed test. Rerun it with `cargo test_details`.

## Additional Rust Commands
- `cargo fmt`: Ensure uniform code formatting
- `cargo clippy`: Ensure idiomatic code
- `cargo check`: Ensure compilation succeeds on Linux, MacOS, Windows and WebAssembly
- `cargo bench`: Run all benchmarks
