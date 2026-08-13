use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crate::application::{ServerConfig, StreamControl, StreamRegistry, StreamStatus};
use crate::domain::{Error, Result};
use crate::infrastructure::http::{ApiServer, FileBrowser};
use crate::infrastructure::mp4::{FileSampleReaderFactory, Mp4Probe};
use crate::infrastructure::rtsp::RtspServer;

const DEFAULT_BIND: &str = "0.0.0.0:8555";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_API_BIND: &str = "127.0.0.1:8556";
/// Where `bun run build` leaves the compiled UI.
const DEFAULT_UI_DIR: &str = "web/dist";

const USAGE: &str = "\
rtsp-utils — publish local video files as live RTSP streams

USAGE:
    rtsp-utils <FILE> [OPTIONS]
    rtsp-utils --api [OPTIONS]        Start with no file and manage streams from the web UI

OPTIONS:
    --name <NAME>     Stream name used in the URL path (default: the file stem)
    --bind <ADDR>     RTSP address to listen on (default: 0.0.0.0:8555)
    --host <HOST>     Host to advertise in the printed URL (default: 127.0.0.1)
    --no-loop         Stop at the end of the file instead of restarting it
    --stopped         Load the file but leave it off air until started
    --probe           Print the media layout and the URL, then exit

    --api [ADDR]      Serve the control API and web UI (default: 127.0.0.1:8556)
    --media-dir <DIR> Folder the web UI's file picker opens in (default: .)
    --confine-media   Restrict the picker to --media-dir instead of the whole machine
    --ui <DIR>        Directory holding the built web UI (default: web/dist)
    --no-ui           Serve the API only, without any static files

    -h, --help        Show this help

EXAMPLES:
    rtsp-utils 91.mov
    rtsp-utils 91.mov --api                     # stream plus web UI on :8556
    rtsp-utils --api 0.0.0.0:8556 --host 192.168.1.20
";

struct Args {
    file: Option<PathBuf>,
    name: Option<String>,
    bind: SocketAddr,
    host: String,
    looping: bool,
    start_on_air: bool,
    probe_only: bool,
    api: Option<SocketAddr>,
    ui_dir: Option<PathBuf>,
    media_dir: PathBuf,
    confine_media: bool,
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
    let mut start_on_air = true;
    let mut probe_only = false;
    let mut api: Option<String> = None;
    let mut ui_dir: Option<String> = None;
    let mut no_ui = false;
    let mut media_dir = ".".to_string();
    let mut confine_media = false;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        match arg {
            "--name" | "--bind" | "--host" | "--ui" | "--media-dir" => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| Error::Config(format!("{arg} needs a value")))?
                    .clone();
                match arg {
                    "--name" => name = Some(value),
                    "--bind" => bind = value,
                    "--host" => host = value,
                    "--media-dir" => media_dir = value,
                    _ => ui_dir = Some(value),
                }
                i += 2;
            }
            // The address is optional, so only consume the next argument when
            // it does not look like another flag.
            "--api" => {
                let value = argv.get(i + 1).filter(|v| !v.starts_with('-'));
                api = Some(value.cloned().unwrap_or_else(|| DEFAULT_API_BIND.into()));
                i += if value.is_some() { 2 } else { 1 };
            }
            "--no-loop" => {
                looping = false;
                i += 1;
            }
            "--stopped" => {
                start_on_air = false;
                i += 1;
            }
            "--probe" => {
                probe_only = true;
                i += 1;
            }
            "--no-ui" => {
                no_ui = true;
                i += 1;
            }
            "--confine-media" => {
                confine_media = true;
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

    if file.is_none() && api.is_none() {
        return Err(Error::Config(
            "give a file to publish, or --api to manage streams from the web UI".into(),
        ));
    }

    let api = match api {
        Some(value) => Some(normalize_addr(&value, DEFAULT_API_BIND)?),
        None => None,
    };

    // Only look for the UI when there is an API to pair it with.
    let ui_dir = match (api.is_some(), no_ui) {
        (true, false) => {
            let dir = PathBuf::from(ui_dir.as_deref().unwrap_or(DEFAULT_UI_DIR));
            dir.join("index.html").is_file().then_some(dir)
        }
        _ => None,
    };

    Ok(Args {
        file,
        name,
        bind: normalize_addr(&bind, DEFAULT_BIND)?,
        host,
        looping,
        start_on_air,
        probe_only,
        api,
        ui_dir,
        media_dir: PathBuf::from(media_dir),
        confine_media,
    })
}

/// Accepts `host:port`, a bare port, or a bare host.
fn normalize_addr(value: &str, default: &str) -> Result<SocketAddr> {
    if let Ok(addr) = value.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let default_port = default.rsplit(':').next().unwrap_or("8555");
    if let Ok(port) = value.parse::<u16>() {
        return format!("0.0.0.0:{port}")
            .parse()
            .map_err(|_| Error::Config(format!("invalid address '{value}'")));
    }
    format!("{value}:{default_port}")
        .parse()
        .map_err(|_| Error::Config(format!("invalid address '{value}'")))
}

fn serve(args: Args) -> Result<()> {
    let config = ServerConfig {
        bind: args.bind,
        advertised_host: args.host.clone(),
        looping: args.looping,
    };

    let registry = Arc::new(StreamRegistry::new());
    let control = Arc::new(StreamControl::new(
        Arc::new(Mp4Probe),
        Arc::clone(&registry),
        config.clone(),
    ));

    if let Some(file) = &args.file {
        let stream = control.add(file, args.name.as_deref(), args.start_on_air)?;
        print_summary(&control.status(stream.name())?);

        if args.probe_only {
            return Ok(());
        }
    } else {
        println!("\nNo file loaded yet — add one from the web UI.");
    }

    let rtsp = RtspServer::bind(
        Arc::clone(&registry),
        config,
        Arc::new(FileSampleReaderFactory),
    )?;
    println!("RTSP    rtsp://{} (Ctrl-C to stop)", rtsp.local_addr()?);

    // The control API, when enabled, runs alongside the RTSP server.
    if let Some(addr) = args.api {
        let browser = Arc::new(FileBrowser::new(&args.media_dir, args.confine_media)?);
        println!(
            "Files   {}{}",
            browser.start_display(),
            if args.confine_media {
                " (picker confined here)"
            } else {
                " (picker can reach the whole machine)"
            }
        );

        let api = ApiServer::bind(
            addr,
            Arc::clone(&control),
            browser,
            Arc::new(FileSampleReaderFactory),
            args.ui_dir.clone(),
        )?;
        let local = api.local_addr()?;
        if api.serves_ui() {
            println!("Web UI  http://{local}");
        } else {
            println!("API     http://{local}/api/streams");
            if args.ui_dir.is_none() {
                println!("        (no built UI found — run `bun run build` in web/)");
            }
        }
        thread::spawn(move || {
            if let Err(e) = api.run() {
                eprintln!("[api] stopped: {e}");
            }
        });
    }

    println!();
    rtsp.run()
}

fn print_summary(status: &StreamStatus) {
    println!("\n{}", status.path);
    println!(
        "  duration  {:.1}s{}",
        status.duration_secs,
        if status.looping { " (looping)" } else { "" }
    );
    for track in &status.tracks {
        println!(
            "  {:<9} {} {}  [{}]",
            track.kind, track.codec, track.detail, track.control
        );
    }

    println!("\nRTSP URL:\n  {}", status.url);
    if !status.active {
        println!("  (loaded but stopped — start it from the web UI)");
    }
    println!("\nPlay it with:");
    println!("  ffplay -rtsp_transport tcp {}", status.url);
    println!("  vlc {}\n", status.url);
}
