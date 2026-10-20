//! 使用专用 socket 验证与正式 Console 相同的终端前端。
use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    session: String,
}
fn main() -> Result<()> {
    let args = Args::parse();
    twig_cli::workbench::standalone(args.socket, args.session)
}
