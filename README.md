# rip-video

This repository includes a TUI to download audio, split into parts, transcribe with ffmpeg-whisper, and optionally generate meeting minutes via the OpenAI API.

## Prerequisites

- jq (for CLI JSON handling): `brew install jq` or `apt-get install jq`.
- ollama (required): install and run a local Ollama server. For macOS: `brew install ollama` and start it with `ollama serve`. Pull a model as needed (e.g., `ollama pull llama3`).

## Ollama Setup

- Create the minutes model from your model file:
  `ollama create minutes -f ~/endress/ai/ollama/minutes.model`
