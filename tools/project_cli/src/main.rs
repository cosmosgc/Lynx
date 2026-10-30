use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

mod commands;

#[derive(Parser)]
#[command(name = "lynx")]
#[command(version)]
#[command(about = "Lynx project scaffolding, diagnostics, and validation")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    New(NewArgs),
    Doctor(DoctorArgs),
    Validate(ValidateArgs),
    Build(ProjectCommandArgs),
    Run(ProjectCommandArgs),
    Test(ProjectCommandArgs),
    Content(ContentArgs),
}

#[derive(Args)]
struct NewArgs {
    name: String,

    #[arg(long, default_value = "minimal")]
    template: Template,

    #[arg(long, default_value = ".")]
    output: PathBuf,
}

#[derive(Args)]
struct DoctorArgs {
    #[arg(long)]
    project: Option<PathBuf>,

    #[arg(long)]
    backend: bool,
}

#[derive(Args)]
struct ValidateArgs {
    #[arg(long)]
    project: PathBuf,
}

#[derive(Args)]
struct ProjectCommandArgs {
    /// Project directory containing Cargo.toml and content/.
    #[arg(long, default_value = ".")]
    project: PathBuf,
}

#[derive(Args)]
struct ContentArgs {
    #[command(subcommand)]
    command: ContentCommand,
}

#[derive(Subcommand)]
enum ContentCommand {
    Build(ContentBuildArgs),
    Hash(HashArgs),
    Diff(DiffArgs),
    PublishCheck(PublishCheckArgs),
}

#[derive(Args)]
struct ContentBuildArgs {
    /// Project directory containing content/.
    #[arg(long, default_value = ".")]
    project: PathBuf,

    /// Manifest output path, relative to the project when not absolute.
    #[arg(long, default_value = "content/manifest.json")]
    output: PathBuf,

    /// Semantic content bundle version recorded in the manifest.
    #[arg(long, default_value = "0.1.0")]
    content_version: String,

    /// Compatible engine semantic-version requirement.
    #[arg(long, default_value = "^0.1")]
    engine: String,
}

#[derive(Args)]
struct HashArgs {
    path: PathBuf,
}

#[derive(Args)]
struct DiffArgs {
    /// Manifest of the content version clients already have.
    #[arg(long)]
    base: PathBuf,

    /// Candidate manifest to publish.
    #[arg(long)]
    next: PathBuf,
}

#[derive(Args)]
struct PublishCheckArgs {
    /// Candidate manifest to publish.
    #[arg(long)]
    manifest: PathBuf,

    /// Manifest of the content version clients already have.
    #[arg(long)]
    base: PathBuf,

    /// Publish without requiring a moderation approval.
    #[arg(long)]
    skip_review: bool,

    /// Allow the author to approve their own content.
    #[arg(long)]
    allow_self_review: bool,

    /// Publish without requiring a declared license.
    #[arg(long)]
    license_optional: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Template {
    Minimal,
    Mmorpg,
}

fn main() {
    if let Err(error) = run() {
        let message = error.to_string();
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    let output = match cli.command {
        Commands::New(args) => commands::scaffold::new_project(&args)?,
        Commands::Doctor(args) => commands::doctor::doctor(&args)?,
        Commands::Validate(args) => commands::project::validate_project(&args)?,
        Commands::Content(args) => commands::content::content(&args)?,
        Commands::Build(args) => commands::project::project_command(&args, "build")?,
        Commands::Run(args) => commands::project::project_command(&args, "run")?,
        Commands::Test(args) => commands::project::project_command(&args, "test")?,
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", commands::output::render_human_output(&output));
    }

    if matches!(output["command"].as_str(), Some("build" | "run" | "test")) && output["ok"] == false
    {
        std::process::exit(1);
    }
    Ok(())
}
