# rtsp-utils

Turns a local video file into a live RTSP stream and prints the URL to play it.
Comes with an optional web UI to start and stop streams.

The server is pure Rust with no dependencies — not even ffmpeg. The MOV/MP4
container is parsed directly, and the H.264 and AAC samples inside are
packetised into RTP and served over an RTSP server written from scratch.

## Quick start

One command builds the UI and the server and opens the app — the same command,
with the same flags, on macOS, Linux and Windows:

```sh
bun run.mjs
```

You need [Rust](https://rustup.rs) and [Bun](https://bun.sh); the launcher says
so if either is missing. The browser opens at <http://127.0.0.1:8080>, and
Ctrl-C stops everything. Repeat runs skip the UI build unless something under
`web/` has changed.

If you would rather not type the runtime, there is a shim per platform. They
all forward to the same launcher, so the flags never change:

| | |
| --- | --- |
| macOS, Linux | `./run.sh` |
| Windows | `.\run.ps1`, or double-click `run.cmd` |

| Option | Does |
| --- | --- |
| `--file <PATH>` | Publish a video on startup |
| `--media-dir <DIR>` | Folder the file picker opens in |
| `--api-addr <ADDR>` | Control API address (default `127.0.0.1:8080`) |
| `--dev` | Vite dev server with hot reload, on port 5173 |
| `--debug` | Build the debug profile, which compiles faster |
| `--rebuild-ui` | Rebuild the UI even if it looks current |
| `--no-open` | Do not open a browser |

```sh
bun run.mjs --file clip.mov --media-dir ~/Movies
bun run.mjs --dev
```

Anything after `--` goes straight to `rtsp-utils`, so the full CLI below stays
available — including from PowerShell, which the shim keeps out of the way of:

```sh
bun run.mjs -- --name cam1 --host 192.168.1.20 --no-loop
```

```powershell
.\run.ps1 --media-dir D:\recordings -- --name cam1 --no-loop
```

## Running it by hand

### 1. Build

```sh
cargo build --release
```

### 2. Serve a file

```powershell
.\target\release\rtsp-utils.exe mock_video.mov --name 91
```

```sh
# macOS / Linux
./target/release/rtsp-utils mock_video.mov --name 91
```

It prints what it found and the URL, then serves until you press Ctrl-C:

```
mock_video.mov
  duration  337.5s (looping)
  video     H.264 1280x720  90.09 fps  30407 samples  [trackID=0]
  audio     AAC 48000 Hz  1 ch  15820 samples  [trackID=1]

RTSP URL:
  rtsp://127.0.0.1:8554/91

Listening on rtsp://0.0.0.0:8554 (Ctrl-C to stop)
```

Without `--name` the stream is named after the file, so `mock_video.mov`
would be served at `rtsp://127.0.0.1:8554/mock_video`.

### 3. Play it

Leave the server running and open the URL in any RTSP client:

```sh
ffplay -rtsp_transport tcp rtsp://127.0.0.1:8554/91
vlc rtsp://127.0.0.1:8554/91
```

If you have no player installed, VLC is the quickest to get:

```powershell
winget install VideoLAN.VLC
```

You don't strictly need one — `cargo test --release` drives the same code path
end to end with the RTSP client built into the test suite.

### Options

| Option | Default | Meaning |
| --- | --- | --- |
| `--name <NAME>` | the file stem | Stream name in the URL path |
| `--bind <ADDR>` | `0.0.0.0:8554` | RTSP address to listen on (a bare port works too) |
| `--host <HOST>` | `127.0.0.1` | Host to advertise in the printed URL |
| `--no-loop` | off | Stop at the end of the file instead of restarting |
| `--stopped` | off | Load the file but leave it off air until started |
| `--probe` | off | Print the media layout and URL, then exit |
| `--api [ADDR]` | off (`127.0.0.1:8080`) | Serve the control API and web UI |
| `--media-dir <DIR>` | `.` | Folder the file picker opens in |
| `--confine-media` | off | Restrict the picker to `--media-dir` instead of the whole machine |
| `--ui <DIR>` | `web/dist` | Directory holding the built web UI |
| `--no-ui` | off | Serve the API only, without any static files |
| `-h`, `--help` | | Show usage |

### Common variations

```sh
# inspect the file without serving it
rtsp-utils mock_video.mov --probe

# reachable from other machines: advertise the address they will dial
rtsp-utils mock_video.mov --name 91 --host 192.168.1.20
# -> rtsp://192.168.1.20:8554/91

# a different port, and play through once instead of looping
rtsp-utils mock_video.mov --bind 8555 --no-loop
```

### If the player stalls

A client that defaults to UDP may sit there with a black window, usually
because a firewall is dropping the inbound RTP ports. Force the interleaved
TCP transport instead — it is fully supported, and it needs no ports beyond
the RTSP connection itself:

- **ffplay**: `-rtsp_transport tcp`
- **VLC**: *Tools → Preferences → Input/Codecs → "RTP over RTSP (TCP)"*

`--host` matters too: the URL has to name an address the client can reach.
`127.0.0.1` only works on this machine, however you bound the listener.

## Web UI

A small React front end for starting and stopping streams, picking video files
from the server's disk, watching a live preview, and copying RTSP URLs.

`bun run.mjs` does all of this for you. By hand, build the UI once and then run
the server with `--api`:

```sh
cd web
bun install
bun run build
cd ..

rtsp-utils mock_video.mov --name 91 --api
```

```
RTSP    rtsp://0.0.0.0:8554 (Ctrl-C to stop)
Web UI  http://127.0.0.1:8080
```

The same binary serves the API and the built UI, so there is nothing else to
run. Open <http://127.0.0.1:8080>.

Starting with `--api` and no file at all is fine too — the UI can find files
itself:

```sh
rtsp-utils --api
```

### Picking files

**Browse for a video…** opens a picker over the whole machine: drive buttons
along the top, clickable breadcrumbs, and folders listed ahead of files. Only
`.mov`, `.mp4` and `.m4v` are offered, and folders with more than 1000 entries
are truncated with a note rather than silently cut short.

It opens in the working directory. Point it somewhere more useful with
`--media-dir`, which sets the starting folder without restricting where you can
go from there:

```sh
rtsp-utils --api --media-dir D:\recordings
```

To restore a hard boundary — nothing outside the starting folder can be listed
or published, and the drive buttons disappear — add `--confine-media`:

```sh
rtsp-utils --api --media-dir D:\recordings --confine-media
```

### Live preview

**Watch** on a running stream plays it in the page. Since no browser speaks
RTSP, the server repackages the same H.264 samples into fragmented MP4 and
streams them over a chunked HTTP response, which the page feeds to a
`SourceBuffer` through Media Source Extensions.

Nothing is transcoded — the container already stores AVCC-formatted samples,
which is exactly what an fMP4 `mdat` wants, so the bytes are copied through and
only the boxes around them are new. That keeps the preview honest: it is the
same coded video an RTSP client receives, not a re-encoding of it.

Two things worth knowing:

- **Video only.** Carrying AAC as well would mean a second track in every
  fragment, and a monitoring view that autoplays is muted anyway.
- **It is a second reader, not a mirror.** Each RTSP client gets its own
  playback starting from the top of the file, and the preview is one more of
  those. It shows what a client connecting right now would see, rather than
  echoing some other viewer's position. It counts toward the viewer total for
  the same reason.

The player chases the live edge, skipping forward if it drifts more than a
couple of seconds behind, and evicts old buffer as it goes.

### Working on the UI

`bun run.mjs --dev` starts the Vite dev server with hot reload on port 5173 and
the backend beside it, and stops both on Ctrl-C. By hand:

```sh
cd web
bun dev          # http://localhost:5173, proxies /api to the Rust server
bun run build    # type-check and emit web/dist
bun run lint
```

The dev server proxies `/api` through to `127.0.0.1:8080`, so run the backend
alongside it with `rtsp-utils --api --no-ui`.

Stack: Bun, Vite, React 19, TypeScript, Tailwind v4, shadcn/ui and lucide
icons. The UI polls `/api/streams` every two seconds — viewer counts and uptime
change without any action from the browser, so polling is both simpler than a
socket and sufficient at that cadence.

### Control API

Enabled by `--api`. Every response is JSON, and errors carry an `{"error": …}`
body.

| Method | Path | Does |
| --- | --- | --- |
| `GET` | `/api/health` | Server version, RTSP endpoint and media directory |
| `GET` | `/api/streams` | List every loaded stream |
| `POST` | `/api/streams` | Load a file: `{"path": …, "name"?: …, "start"?: true}` |
| `GET` | `/api/streams/{name}` | One stream's status |
| `POST` | `/api/streams/{name}/start` | Put it on air |
| `POST` | `/api/streams/{name}/stop` | Take it off air, disconnecting clients |
| `DELETE` | `/api/streams/{name}` | Unload it entirely |
| `GET` | `/api/files?path=…` | Browse folders and video files; empty path means the starting folder |
| `GET` | `/api/streams/{name}/preview.mp4` | Live fragmented MP4 of the video track |

```sh
curl -X POST http://127.0.0.1:8080/api/streams/91/stop
curl -X POST http://127.0.0.1:8080/api/streams \
  -H 'Content-Type: application/json' \
  -d '{"path":"mock_video.mov","name":"cam2"}'
```

A stopped stream stays loaded but is invisible over RTSP: clients already
playing it are cut off, and a `DESCRIBE` for it answers `404` exactly as it
would for a name that was never published.

The API binds to `127.0.0.1` by default, so it is not reachable from other
machines. That default matters, because there is no authentication and the
picker can reach any file the server process can read: anyone who can reach the
API can browse the machine and publish from it. Before binding it to anything
wider, add `--confine-media` — or put it behind something that authenticates.

## What it supports

- **Containers**: MOV / MP4 / QuickTime, with `moov` at either end of the file.
- **Video**: H.264 (`avc1` / `avc3`), packetised per RFC 6184 — single NAL unit
  packets with FU-A fragmentation. SPS and PPS are repeated before every
  keyframe so clients that join mid-stream can start decoding immediately.
- **Audio**: AAC (`mp4a`), packetised per RFC 3640 in `AAC-hbr` mode.
- **Transports**: RTP over UDP, and RTP interleaved on the RTSP connection
  (`RTP/AVP/TCP`) for clients behind a firewall.
- **RTSP methods**: `OPTIONS`, `DESCRIBE`, `SETUP`, `PLAY`, `PAUSE`,
  `TEARDOWN`, `GET_PARAMETER`, `SET_PARAMETER`.
- **RTCP**: periodic Sender Reports so receivers can sync audio against video.

Samples are delivered paced against a wall clock, not dumped as fast as the
disk can read, so the stream behaves like a live camera. Only `moov` is held in
memory; sample payloads are read from disk as they are sent, so a 450 MB file
costs a few hundred kilobytes of RAM.

### Not supported

- Seeking. `PLAY` always starts at the beginning, and `PAUSE` stops delivery
  rather than holding a position.
- Codecs other than H.264 and AAC. Other tracks (timecode, ProRes, subtitles)
  are skipped with a note rather than failing the whole file.
- Multicast, RTSP 2.0, TLS, and authentication.

## Architecture

Layered, with dependencies only ever pointing inwards.

```
presentation  ->  application  ->  domain  <-  infrastructure
```

| Layer | Path | Contains |
| --- | --- | --- |
| `domain` | [src/domain/](src/domain/) | Entities (`MediaSource`, `Track`, `Sample`), the `RtspUrl` value object, errors, and the ports every other layer talks through. No I/O. |
| `application` | [src/application/](src/application/) | Use cases: publishing a file ([publish.rs](src/application/publish.rs)), the start/stop control surface ([control.rs](src/application/control.rs)), the registry of loaded streams ([registry.rs](src/application/registry.rs)), the paced RTP playback loop ([session.rs](src/application/session.rs)) and its browser-preview counterpart ([preview.rs](src/application/preview.rs)). Knows nothing about MP4 boxes, sockets, or HTTP. |
| `infrastructure` | [src/infrastructure/](src/infrastructure/) | The concrete implementations: the container parser ([mp4/](src/infrastructure/mp4/)), the RTP payload formats ([rtp/](src/infrastructure/rtp/)), the fragmented-MP4 muxer ([fmp4/](src/infrastructure/fmp4/)), the RTSP server, SDP and transports ([rtsp/](src/infrastructure/rtsp/)), and the HTTP control API with its file browser ([http/](src/infrastructure/http/)). |
| `presentation` | [src/presentation/](src/presentation/) | Argument parsing and terminal output. |
| web UI | [web/](web/) | The React front end. Talks only to the HTTP control API. |

Both servers are adapters over the same core: the RTSP server and the control
API each drive the application layer, and neither knows the other exists. The
RTSP side reads the on-air flag; the HTTP side writes it.

[src/main.rs](src/main.rs) is the composition root: it is the only place that
picks which implementation satisfies which port.

The ports are declared in [src/domain/ports.rs](src/domain/ports.rs):

| Port | Implemented by |
| --- | --- |
| `MediaProbe` | `Mp4Probe` — reads `moov` and builds the sample tables |
| `SampleReaderFactory` / `SampleReader` | `FileSampleReader` — lazy disk reads |
| `Packetizer` | `H264Packetizer`, `AacPacketizer` |
| `RtpSink` | `SessionSink` — TCP interleaved or UDP, per track |
| `RtcpReporter` | `StandardRtcpReporter` |
| `MediaFragmenter` | `Fmp4Fragmenter` — init segment plus `moof`/`mdat` |
| `ByteSink` | `ChunkedSink` — HTTP chunked transfer |

Threading is one thread per RTSP connection, one per HTTP request, and one
playback thread per playing session. The playback thread and the RTSP request
handler share the control socket behind a mutex, so interleaved media never
cuts into a response. Playback threads check the on-air flag between samples,
which is how a stop from the UI reaches a client that is already watching.

## Tests

```sh
cargo test
```

Unit tests cover the sample-table reconstruction (`stsc`/`stco`/`stts`/`ctts`/
`stss`), `avcC` and `esds` parsing, both packetizers, transport negotiation,
RTSP message framing, the stream registry's start/stop and viewer accounting,
the JSON codec, the file browser's breadcrumbs and confinement, and the
fMP4 muxer —
the generated boxes are read back with the project's own atom parser, so the
parameter sets have to survive a round trip through the container.

[tests/preview_stream.rs](tests/preview_stream.rs) starts the control API and
pulls the preview over a real socket, checking that the response is a valid
init segment followed by `moof`/`mdat` pairs with monotonically advancing
decode times, and that it arrives at roughly one second of media per second of
wall clock rather than as fast as the disk allows.

The end-to-end tests in [tests/rtsp_stream.rs](tests/rtsp_stream.rs) start the
real server and drive it with a real RTSP client over a socket, asserting that
SDP is well formed, that RTP sequence numbers are contiguous, that parameter
sets are repeated in band, that RTCP reports arrive, and that the frame rate
and the RTP clock both track real time. One of them covers what the UI's
Start/Stop button relies on: stopping a stream cuts off a client that is
already playing, releases its viewer slot, and hides the stream until it is
started again.

They need a sample file, and pick up any `.mov` or `.mp4` sitting in the crate
root on their own. To point them at one somewhere else:

```sh
RTSP_UTILS_TEST_FILE=/path/to/video.mov cargo test
```

```powershell
$env:RTSP_UTILS_TEST_FILE = "D:\media\video.mov"; cargo test
```

They skip themselves, rather than fail, when no media is present — the sample
video is deliberately not committed.
