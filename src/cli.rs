use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "heavydrop",
    author = "HeavyDrop Contributors",
    version,
    about = "⚡ Ultra-fast P2P file transfer tool optimized for huge files (50GB+) in Rust with native GUI and CLI."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Send a file to a peer over P2P network
    Send {
        /// Path to the file to send
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Port to listen on
        #[arg(short, long, default_value_t = 7878)]
        port: u16,

        /// Network interface to bind
        #[arg(short, long, default_value = "0.0.0.0")]
        bind: String,

        /// Chunk size in Megabytes (recommended 4 to 16 for 50GB files)
        #[arg(short, long, default_value_t = 4)]
        chunk_size_mb: u32,

        /// Optional secret security token to authenticate the receiver
        #[arg(short, long)]
        token: Option<String>,

        /// Skip upfront full-file hashing before transfer (chunks are still verified in transit)
        #[arg(long)]
        no_prehash: bool,
    },

    /// Receive a file from a peer over P2P network
    Recv {
        /// Peer sender address (e.g., 192.168.1.50:7878 or your-ip:7878)
        #[arg(value_name = "PEER_ADDRESS")]
        address: String,

        /// Directory where the received file will be saved
        #[arg(short, long, default_value = ".")]
        output_dir: PathBuf,

        /// Secret security token (if sender configured one)
        #[arg(short, long)]
        token: Option<String>,

        /// Force overwrite: ignore existing partial file and download from scratch
        #[arg(short, long)]
        force: bool,
    },

    /// Launch the native desktop window GUI
    Gui,
}
