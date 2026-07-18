# Build everything (sessiond included) and launch the freshly built binary
# directly. Bypassing `cargo run` keeps cargo's environment (CARGO_*,
# RUSTUP_*, LD_LIBRARY_PATH into target/debug) out of Horizon and sessiond —
# see docs/tasks/backlog.md item 12.
dev *ARGS:
    cargo build --workspace
    ./target/debug/horizon {{ARGS}}

# Same as `dev`, but release-optimized — halves redraw-processing cost
# versus a debug build (see the terminal keystroke latency investigation),
# at the cost of a slower build.
dev-release *ARGS:
    cargo build --workspace --release
    ./target/release/horizon {{ARGS}}
