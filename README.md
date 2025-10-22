# rip-video

This repository includes a TUI to download audio, split into parts, transcribe with ffmpeg-whisper, generate meeting minutes using a local Ollama model.
![rip-video](rip-video.png)

## Install

Once published you will be able to install the binary directly:

```bash
cargo install rip-video
```

Until then you can still install from the repository:

```bash
cargo install --path .
# or
cargo install --git https://github.com/syzer/rip-video
```

## Usage

1. Grab the media link (e.g. open DevTools Network tab and copy the request URL that contains `videomanifest`).
2. Launch the TUI with the URL:
   ```bash
   rip-video <MEDIA_URL>
   # optionally override the output audio path (default: audio.m4a)
   rip-video --output downloads/audio.m4a <MEDIA_URL>
   ```
3. The app downloads audio via `yt-dlp`, splits and transcribes it with `ffmpeg-whisper`, then generates Minutes and a Summary using local Ollama models.

If you prefer to rely on the clipboard, set the `CLIPBOARD_TEXT` environment variable before launching:

```bash
CLIPBOARD_TEXT="$(pbpaste)" rip-video
```

macOS convenience targets are kept in the `justfile`:
- `just` runs the command above.
- `just create-minutes` creates the local minutes model (one-time).
- `just reset` removes generated artifacts (audio, parts, transcript, minutes).



## Prerequisites

- yt-dlp: `brew install yt-dlp`.
- just (task runner): `brew install just` or `apt-get install just`.
- ollama (required): install and run a local Ollama server. For macOS: `brew install ollama` and start it with `ollama serve`. Pull a model as needed (e.g., `ollama pull llama3`).

## Ollama Setup

- Start the Ollama server:
  - `ollama serve`

- Pull the model used for Summary (required):
  - `ollama pull deepseek-r1:14b`

- Create the minutes model from the repo file (required for the Minutes tab):
  - `just create-minutes`
  - or `ollama create minutes -f minutes.model`

- Verify models:
  - `echo "test" | ollama run minutes`
  - `echo "test" | ollama run deepseek-r1:14b`
