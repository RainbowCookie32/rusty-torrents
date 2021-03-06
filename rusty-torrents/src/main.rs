mod ui;
mod engine;

use clap::{Arg, App};
use tokio::sync::oneshot;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let matches = App::new("rusty-torrents")
        .version("0.1.0")
        .about("makes torrents go brr")
        .arg(
            Arg::with_name("path")
            .short("t")
            .long("torrent")
            .value_name("PATH")
            .help("The path to the torrent file to use")
            .takes_value(true)
            .required(true)
        )
        .get_matches()
    ;

    let (stop_tx, stop_rx) = oneshot::channel();
    
    let torrent_path = matches.value_of("path").expect("No value for torrent provided");
    let torrent_data = std::fs::read(torrent_path).expect("Couldn't open torrent file");

    let torrent_engine = engine::Engine::init(torrent_data, stop_rx).await;
    let torrent_info = torrent_engine.info();

    tokio::spawn(async move {
        let mut torrent_engine = torrent_engine;
        torrent_engine.start_torrent().await;
    });

    let app = ui::App::new(stop_tx, torrent_info);
    app.draw();
}
