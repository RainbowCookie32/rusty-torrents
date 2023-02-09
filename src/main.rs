mod ui;
mod engine;
mod bencode;

use std::path::PathBuf;

use clap::Parser;
use tokio::sync::oneshot;

use tracing::*;
use tracing_subscriber::FmtSubscriber;

#[derive(Debug, Parser)]
#[command(author, version)]
struct Args {
    #[arg(short, long)]
    torrent_path: PathBuf,

    #[arg(short, long)]
    output_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let output_path = args.output_path.unwrap_or_else(|| dirs::download_dir().expect("failed to get downloads dir"));

    let _subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .finish()
    ;

    /*tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to setup default subscriber!");*/

    let (stop_tx, stop_rx) = oneshot::channel();

    let torrent_engine = engine::Engine::init(args.torrent_path, output_path, stop_rx).await;
    let torrent_info = torrent_engine.info();
    let progress_rx = torrent_engine.get_progress_rx();

    tokio::spawn(async move {
        let mut torrent_engine = torrent_engine;
        torrent_engine.start_torrent().await;
    });

    let app = ui::App::new(stop_tx, torrent_info, progress_rx).await;
    app.draw();
}
