mod types;
mod engine;

use clap::{Arg, App};

fn main() {
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

    let torrent_path = matches.value_of("path").expect("No value for torrent provided");
    let torrent_data = std::fs::read(torrent_path).expect("Couldn't open torrent file");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap()
    ;

    let torrent_engine = rt.block_on(engine::Engine::init(torrent_data));
    rt.block_on(torrent_engine.start_torrent());
}
