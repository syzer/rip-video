# rip-video

How to use (macOS)
- In your browser DevTools (Network tab), copy the media URL that contains the string `videomanifest` to your clipboard.
- If you haven't yet, create the local minutes model: `just create-minutes` (one-time).
- Run `just` (shorthand for `CLIPBOARD_TEXT="$(pbpaste)" cargo run`).
- The app will pick the URL from the clipboard, download audio, split, transcribe, and prepare Minutes.

This repository includes a TUI to download audio, split into parts, transcribe with ffmpeg-whisper, and optionally generate meeting minutes via the OpenAI API.

## Prerequisites

- jq (for CLI JSON handling): `brew install jq` or `apt-get install jq`.
- ollama (required): install and run a local Ollama server. For macOS: `brew install ollama` and start it with `ollama serve`. Pull a model as needed (e.g., `ollama pull llama3`).

## Ollama Setup

- Create the minutes model from the repo file:
  - `just create-minutes`
  - or `ollama create minutes -f minutes.model`
