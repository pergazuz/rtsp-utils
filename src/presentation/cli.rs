use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::application::{PublishMedia, ServerConfig, StreamRegistry};
use crate::domain::media::{CodecParams, MediaSource};
use crate::domain::url::RtspUrl;
use crate::domain::{Error, Result};
use crate::infrastructure::mp4::{FileSampleReaderFactory, Mp4Probe};
use crate::infrastructure::rtsp::RtspServer;

const DEFAULT_BIND: &str = "0.0.0.0:8554";
const DEFAULT_HOST: &str = "127.0.0.1";

const USAGE: &str = "\
rtsp-utils — publish a local video file as a live RTSP stream

USAGE:
    rtsp-utils <FILE> [OPTIONS]

OPTIONS:
    --name <NAME>     Stream name used in the URL path (default: the file stem)
    --bind <ADDR>     Address to listen on (default: 0.0.0.0:8554)
    --host <HOST>     Host to advertise in the printed URL (default: 127.0.0.1)
    --no-loop         Stop at the end of the file instead of restarting it
    --probe           Print the media layout and the URL, then exit
    -h, --help        Show this help

EXAMPLE:
    rtsp-utils 91.mov
    rtsp-utils 91.mov --name cam1 --bind 0.0.0.0:8554 --host 192.168.1.20
";

struct Args {
    file: PathBuf,
    name: Option<String>,
    bind: SocketAddr,
    host: String,
    looping: bool,
    probe_only: bool,
}

/// Entry point; returns the process exit code.
pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return if args.is_empty() { 2 } else { 0 };
    }

    let args = match parse_args(&args) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return 2;
        }
    };

    match serve(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn parse_args(argv: &[String]) -> Result<Args> {
    let mut file = None;
    let mut name = None;
    let mut bind = DEFAULT_BIND.to_string();
    let mut host = DEFAULT_HOST.to_string();
    let mut looping = true;
    let mut probe_only = false;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        match arg {
            "--name" | "--bind" | "--host" => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| Error::Config(format!("{arg} needs a value")))?
                    .clone();
                match arg {
                    "--name" => name = Some(value),
                    "--bind" => bind = value,
                    _ => host = value,
                }
                i += 2;
            }
            "--no-loop" => {
                looping = false;
                i += 1;
            }
            "--probe" => {
                probe_only = true;
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(Error::Config(format!("unknown option {other}")));
            }
            other => {
                if file.is_some() {
                    return Err(Error::Config(format!("unexpected extra argument {other}")));
                }
                file = Some(PathBuf::from(other));
                i += 1;
            }
        }
    }

    let file = file.ok_or_else(|| Error::Config("no input file given".into()))?;
    // A bare port or host without a port are both common slips; accept them.
    let bind = normalize_bind(&bind)?;

    Ok(Args {
        file,
        name,
        bind,
        host,
        looping,
        probe_only,
    })
}

fn normalize_bind(value: &str) -> Result<SocketAddr> {
    if let Ok(addr) = value.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(port) = value.parse::<u16>() {
        return format!("0.0.0.0:{port}")
            .parse()
            .map_err(|_| Error::Config(format!("invalid bind address '{value}'")));
    }
    format!("{value}:8554")
        .parse()
        .map_err(|_| Error::Config(format!("invalid bind address '{value}'")))
}

fn serve(args: Args) -> Result<()> {
    let config = ServerConfig {
        bind: args.bind,
        advertised_host: args.host.clone(),
        looping: args.looping,
    };

    let probe = Mp4Probe;
    let registry = Arc::new(StreamRegistry::new());
    let publish = PublishMedia::new(&probe, registry.as_ref(), &config);
    let (source, url) = publish.execute(&args.file, args.name.as_deref())?;

    print_summary(&source, &url, &config);

    if args.probe_only {
        return Ok(());
    }

    let server = RtspServer::bind(
        Arc::clone(&registry),
        config,
        Arc::new(FileSampleReaderFactory),
    )?;
    println!(
        "Listening on rtsp://{} (Ctrl-C to stop)\n",
        server.local_addr()?
    );
    server.run()
}

fn print_summary(source: &MediaSource, url: &RtspUrl, config: &ServerConfig) {
    println!("\n{}", source.path.display());
    println!(
        "  duration  {:.1}s{}",
        source.duration_secs,
        if config.looping { " (looping)" } else { "" }
    );

    for track in &source.tracks {
        match &track.codec {
            CodecParams::H264(p) => {
                let fps = if track.duration_secs() > 0.0 {
                    track.samples.len() as f64 / track.duration_secs()
                } else {
                    0.0
                };
                println!(
                    "  video     H.264 {}x{}  {:.2} fps  {} samples  [{}]",
                    p.width,
                    p.height,
                    fps,
                    track.samples.len(),
                    track.control()
                );
            }
            CodecParams::Aac(p) => {
                println!(
                    "  audio     AAC {} Hz  {} ch  {} samples  [{}]",
                    p.sample_rate,
                    p.channels,
                    track.samples.len(),
                    track.control()
                );
            }
        }
    }

    println!("\nRTSP URL:\n  {url}\n");
    println!("Play it with:");
    println!("  ffplay -rtsp_transport tcp {url}");
    println!("  vlc {url}");
}
