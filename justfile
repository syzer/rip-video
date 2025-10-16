default := "run"

run:
    CLIPBOARD_TEXT="$(pbpaste)" cargo run

build:
    cargo build

clean:
    cargo clean
