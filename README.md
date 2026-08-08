# rtsp-utils

Turns a local video file into a live RTSP stream and prints the URL to play it.

Pure Rust, no dependencies — not even ffmpeg. The MOV/MP4 container is parsed
directly, and the H.264 and AAC samples inside are packetised into RTP and
served over an RTSP server written from scratch.

## How to run

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
| `--bind <ADDR>` | `0.0.0.0:8554` | Address to listen on (a bare port works too) |
| `--host <HOST>` | `127.0.0.1` | Host to advertise in the printed URL |
| `--no-loop` | off | Stop at the end of the file instead of restarting |
| `--probe` | off | Print the media layout and URL, then exit |
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
| `application` | [src/application/](src/application/) | Use cases: publishing a file ([publish.rs](src/application/publish.rs)), the registry of live streams ([registry.rs](src/application/registry.rs)), and the paced playback loop ([session.rs](src/application/session.rs)). Knows nothing about MP4 boxes or sockets. |
| `infrastructure` | [src/infrastructure/](src/infrastructure/) | The concrete implementations: the container parser ([mp4/](src/infrastructure/mp4/)), the RTP payload formats ([rtp/](src/infrastructure/rtp/)), and the RTSP server, SDP and transports ([rtsp/](src/infrastructure/rtsp/)). |
| `presentation` | [src/presentation/](src/presentation/) | Argument parsing and terminal output. |

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

Threading is one thread per RTSP connection plus one playback thread per
playing session. The playback thread and the request handler share the control
socket behind a mutex, so interleaved media never cuts into a response.

## Tests

```sh
cargo test
```

Unit tests cover the sample-table reconstruction (`stsc`/`stco`/`stts`/`ctts`/
`stss`), `avcC` and `esds` parsing, both packetizers, transport negotiation and
RTSP message framing.

The end-to-end tests in [tests/rtsp_stream.rs](tests/rtsp_stream.rs) start the
real server and drive it with a real RTSP client over a socket, asserting that
SDP is well formed, that RTP sequence numbers are contiguous, that parameter
sets are repeated in band, that RTCP reports arrive, and that the frame rate
and the RTP clock both track real time.

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
