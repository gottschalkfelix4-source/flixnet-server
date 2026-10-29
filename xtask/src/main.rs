//! Project automation that works the same on Windows and Linux: `cargo xtask <command>`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};

mod fixtures;

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate synthetic test media into fixtures/generated (needs ffmpeg on PATH).
    Fixtures {
        /// Regenerate files that already exist.
        #[arg(long)]
        force: bool,
    },
    /// Export the OpenAPI spec to web/openapi.json and regenerate the TypeScript client.
    Openapi,
}

fn main() -> anyhow::Result<()> {
    let root = workspace_root();
    match Cli::parse().command {
        Cmd::Fixtures { force } => fixtures::generate(&root.join("fixtures/generated"), force),
        Cmd::Openapi => openapi(&root),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the workspace")
        .to_path_buf()
}

fn openapi(root: &Path) -> anyhow::Result<()> {
    run(Command::new("cargo").current_dir(root).args([
        "run",
        "-q",
        "-p",
        "flixnet-server",
        "--",
        "openapi",
        "--output",
        "web/openapi.json",
    ]))?;
    run(npm().current_dir(root.join("web")).args(["run", "gen:api"]))
}

/// `npm` is a `.cmd` shim on Windows, which `Command` cannot start directly.
fn npm() -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "npm"]);
        cmd
    } else {
        Command::new("npm")
    }
}

pub(crate) fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("failed to start {:?}", cmd.get_program()))?;
    if !status.success() {
        bail!("{:?} exited with {status}", cmd.get_program());
    }
    Ok(())
}
