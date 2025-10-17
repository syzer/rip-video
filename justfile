default := "run"

run:
    CLIPBOARD_TEXT="$(pbpaste)" cargo run

build:
    cargo build

clean:
    cargo clean

# Create the local Ollama model named "minutes" from minutes.model in repo
create-minutes:
    ollama create minutes -f minutes.model

# Remove generated artifacts
reset:
    rm -rf audio.m4a parts parts10 transcript.txt minutes.md
