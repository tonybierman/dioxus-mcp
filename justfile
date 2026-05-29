# Build the cockpit UI into ui-dist/, then build the dioxus-mcp binary (which bakes it in via include_dir!).
deploy:
    ./scripts/build-ui.sh
    cargo build -p dioxus-mcp
