#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, allow(unused_features))]

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum, error::ErrorKind};
use clap_complete::{Shell, generate};

mod commands;
mod reference;

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
    /// Preview, apply, or roll back a content migration.
    Migrate(MigrateArgs),
    /// Generate shell completion definitions from the CLI command tree.
    Completions(CompletionsArgs),
    /// Write the generated Markdown command reference.
    Reference(ReferenceArgs),
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

#[derive(Args)]
struct CompletionsArgs {
    /// Shell to generate completions for.
    #[arg(value_enum)]
    shell: Shell,

    /// Output path for the completion definition.
    #[arg(long, default_value = "lynx-completions")]
    output: PathBuf,
}

#[derive(Args)]
struct ReferenceArgs {
    /// Output path for the generated Markdown reference.
    #[arg(long, default_value = "docs/cli-reference.md")]
    output: PathBuf,
}

#[derive(Args)]
struct MigrateArgs {
    #[command(subcommand)]
    command: MigrateCommand,
}

#[derive(Subcommand)]
enum MigrateCommand {
    /// Validate and preview a migration; pass --apply to commit it.
    Apply(MigrationApplyArgs),
    /// Restore the exact content snapshot retained by a prior apply.
    Rollback(MigrationRollbackArgs),
}

#[derive(Args)]
struct MigrationApplyArgs {
    /// Project directory containing content/manifest.json and content files.
    #[arg(long, default_value = ".")]
    project: PathBuf,

    /// Reviewed RON migration plan.
    #[arg(long)]
    plan: PathBuf,

    /// Commit the validated candidate; without this flag the command is a dry run.
    #[arg(long)]
    apply: bool,
}

#[derive(Args)]
struct MigrationRollbackArgs {
    /// Project directory containing content/ and .lynx/migrations/.
    #[arg(long, default_value = ".")]
    project: PathBuf,

    /// Migration ID whose retained source content should be restored.
    #[arg(long)]
    id: String,
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
    let cli = parse_cli();
    let command_name = command_name(&cli.command);
    let result = match cli.command {
        Commands::New(args) => commands::scaffold::new_project(&args),
        Commands::Doctor(args) => commands::doctor::doctor(&args),
        Commands::Validate(args) => commands::project::validate_project(&args),
        Commands::Content(args) => commands::content::content(&args),
        Commands::Migrate(args) => commands::migrate::migrate(&args),
        Commands::Build(args) => commands::project::project_command(&args, "build"),
        Commands::Run(args) => commands::project::project_command(&args, "run"),
        Commands::Test(args) => commands::project::project_command(&args, "test"),
        Commands::Completions(args) => generate_completions(&args),
        Commands::Reference(args) => generate_reference(&args),
    };
    let mut output = match result {
        Ok(output) => output,
        Err(error) if cli.json => serde_json::json!({
            "command": command_name,
            "ok": false,
            "exit_code": 1,
            "error": {
                "code": "command_failed",
                "message": error.to_string(),
            }
        }),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let exit_code = commands::output::exit_code(&output);
    output["exit_code"] = serde_json::Value::from(exit_code);

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", commands::output::render_human_output(&output));
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn parse_cli() -> Cli {
    match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if error.kind() != ErrorKind::DisplayHelp
                && error.kind() != ErrorKind::DisplayVersion
                && std::env::args_os()
                    .any(|argument| argument.as_os_str() == std::ffi::OsStr::new("--json")) =>
        {
            println!(
                "{}",
                serde_json::json!({
                    "command": "cli",
                    "ok": false,
                    "exit_code": 2,
                    "error": {
                        "code": "invalid_arguments",
                        "message": error.to_string(),
                    }
                })
            );
            std::process::exit(2);
        }
        Err(error) => error.exit(),
    }
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::New(_) => "new",
        Commands::Doctor(_) => "doctor",
        Commands::Validate(_) => "validate",
        Commands::Build(_) => "build",
        Commands::Run(_) => "run",
        Commands::Test(_) => "test",
        Commands::Completions(_) => "completions",
        Commands::Reference(_) => "reference",
        Commands::Content(args) => match &args.command {
            ContentCommand::Build(_) => "content build",
            ContentCommand::Hash(_) => "content hash",
            ContentCommand::Diff(_) => "content diff",
            ContentCommand::PublishCheck(_) => "content publish-check",
        },
        Commands::Migrate(args) => match &args.command {
            MigrateCommand::Apply(_) => "migrate apply",
            MigrateCommand::Rollback(_) => "migrate rollback",
        },
    }
}

fn generate_completions(args: &CompletionsArgs) -> Result<serde_json::Value> {
    let mut command = Cli::command();
    let mut script = Vec::new();
    generate(args.shell, &mut command, "lynx", &mut script);
    let path = commands::absolute_path(&args.output)?;
    std::fs::write(&path, script)?;
    Ok(serde_json::json!({
        "ok": true,
        "command": "completions",
        "shell": format!("{:?}", args.shell).to_ascii_lowercase(),
        "path": path,
    }))
}

fn generate_reference(args: &ReferenceArgs) -> Result<serde_json::Value> {
    let path = commands::absolute_path(&args.output)?;
    std::fs::write(&path, reference::render(Cli::command().term_width(100)))?;
    Ok(serde_json::json!({
        "ok": true,
        "command": "reference",
        "path": path,
    }))
}
