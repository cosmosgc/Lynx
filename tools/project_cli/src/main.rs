use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use backend_nakama::{NakamaClient, NakamaConfig};
use clap::{Args, Parser, Subcommand, ValueEnum};
use engine_content::{
    ContentLoader, ContentManifest, ContentSet, PublishingPolicy, check_publish, diff, sha256_hex,
};
use engine_core::schema::{
    ComponentKey, ComponentRegistry, ComponentSchema, FieldSchema, FieldType, PersistencePolicy,
    SchemaVersion, Visibility,
};
use game_api::HealthService;
use serde_json::{Value, json};

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

impl Template {
    fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Mmorpg => "mmorpg",
        }
    }

    fn content_files(self) -> Vec<(&'static str, &'static str)> {
        let mut files = vec![
            ("content/maps/overworld.ron", WORLD_MAP),
            ("content/zones/starter.ron", STARTER_ZONE),
            ("content/zones/market.ron", MARKET_ZONE),
            ("content/entities/tavern_keeper.ron", TAVERN_KEEPER),
            ("content/items/torch.ron", TORCH),
            ("content/ui/inventory.ron", INVENTORY_PANEL),
            (
                "content/localization/pt-BR.ron",
                match self {
                    Self::Minimal => MINIMAL_CATALOG,
                    Self::Mmorpg => MMORPG_CATALOG,
                },
            ),
        ];
        if matches!(self, Self::Mmorpg) {
            files.extend([
                ("content/entities/training_master.ron", TRAINING_MASTER),
                ("content/ui/practice.ron", PRACTICE_PANEL),
                ("content/minigames/practice.ron", PRACTICE_MINIGAME),
            ]);
        }
        files
    }
}

const STARTER_ZONE: &str = r#"(
    id: "zone.starter",
    version: (major: 1, minor: 0),
    display: (name: "zone.starter.name", description: "zone.starter.description"),
    components: {
        "core.zone": {"max_players": 64, "cell_size_m": 1.0},
    },
)
"#;

const MARKET_ZONE: &str = r#"(
    id: "zone.market",
    version: (major: 1, minor: 0),
    display: (name: "zone.market.name", description: "zone.market.description"),
    components: {
        "core.zone": {"max_players": 64, "cell_size_m": 1.0},
    },
)
"#;

const WORLD_MAP: &str = r#"(
    id: "map.overworld",
    version: (major: 1, minor: 0),
    display: (name: "map.overworld.name", description: "map.overworld.description"),
    zones: [
        (zone: "zone.starter", position: (x: 0, y: 0)),
        (zone: "zone.market", position: (x: 1, y: 0)),
    ],
    routes: [
        (from: "zone.starter", to: "zone.market", bidirectional: true, kind: passage),
    ],
)
"#;

const TAVERN_KEEPER: &str = r#"(
    id: "entity.tavern_keeper",
    version: (major: 1, minor: 0),
    display: (name: "entity.tavern_keeper.name", description: "entity.tavern_keeper.description"),
    components: {
        "core.interactable": {
            "verbs": ["inspect", "talk"],
            "range_m": 2.0,
            "requires_line_of_sight": true,
        },
    },
)
"#;

const TORCH: &str = r#"(
    id: "item.torch",
    version: (major: 1, minor: 0),
    display: (name: "item.torch.name", description: "item.torch.description"),
    components: {
        "core.item": {"stack_max": 16, "weight_kg": 0.5},
    },
)
"#;

const INVENTORY_PANEL: &str = r#"(
    id: "ui.inventory",
    version: (major: 1, minor: 0),
    display: (name: "ui.inventory.title", description: "ui.inventory.description"),
    schema: "ui.inventory",
    actions: [
        (name: "close", label: "ui.action.close"),
        (name: "inspect", label: "ui.action.inspect"),
    ],
    fields: [
        (key: "selected_item", source: "inventory.selected_item"),
    ],
)
"#;

const TRAINING_MASTER: &str = r#"(
    id: "entity.training_master",
    version: (major: 1, minor: 0),
    display: (name: "entity.training_master.name", description: "entity.training_master.description"),
    components: {
        "core.interactable": {
            "verbs": ["inspect", "talk", "practice"],
            "range_m": 2.0,
            "requires_line_of_sight": true,
        },
    },
)
"#;

const PRACTICE_PANEL: &str = r#"(
    id: "ui.practice",
    version: (major: 1, minor: 0),
    display: (name: "ui.practice.title", description: "ui.practice.description"),
    schema: "ui.practice",
    actions: [
        (name: "practice", label: "ui.action.practice"),
        (name: "close", label: "ui.action.close"),
    ],
    fields: [
        (key: "progress", source: "minigame.progress", formatter: Some("percentage")),
    ],
)
"#;

const PRACTICE_MINIGAME: &str = r#"(
    id: "minigame.practice",
    version: (major: 1, minor: 0),
    display: (name: "minigame.practice.name", description: "minigame.practice.description"),
    panel_schema: "ui.practice",
    authority: "server",
    rewards: ["item.torch"],
)
"#;

const MINIMAL_CATALOG: &str = r#"(
    locale: "pt-BR",
    messages: {
        "map.overworld.name": "Mapa da Encruzilhada",
        "map.overworld.description": "As rotas conhecidas ao redor da taverna.",
        "zone.starter.name": "Taverna da Encruzilhada",
        "zone.starter.description": "Um ponto seguro para começar a aventura.",
        "zone.market.name": "Mercado da Encruzilhada",
        "zone.market.description": "Uma rota comercial ligada à taverna.",
        "entity.tavern_keeper.name": "Dona Rosa",
        "entity.tavern_keeper.description": "A responsável pela taverna. Peça informações.",
        "item.torch.name": "Tocha",
        "item.torch.description": "Ilumina caminhos escuros.",
        "ui.inventory.title": "Inventário",
        "ui.inventory.description": "Itens carregados pelo personagem.",
        "ui.action.close": "Fechar",
        "ui.action.inspect": "Inspecionar",
    },
)
"#;

const MMORPG_CATALOG: &str = r#"(
    locale: "pt-BR",
    messages: {
        "map.overworld.name": "Mapa da Encruzilhada",
        "map.overworld.description": "As rotas conhecidas ao redor da taverna.",
        "zone.starter.name": "Taverna da Encruzilhada",
        "zone.starter.description": "Um ponto seguro para começar a aventura.",
        "zone.market.name": "Mercado da Encruzilhada",
        "zone.market.description": "Uma rota comercial ligada à taverna.",
        "entity.tavern_keeper.name": "Dona Rosa",
        "entity.tavern_keeper.description": "A responsável pela taverna. Peça informações.",
        "item.torch.name": "Tocha",
        "item.torch.description": "Ilumina caminhos escuros.",
        "ui.inventory.title": "Inventário",
        "ui.inventory.description": "Itens carregados pelo personagem.",
        "ui.practice.title": "Treinamento",
        "ui.practice.description": "Pratique uma ação validada pelo servidor.",
        "entity.training_master.name": "Mestre de treino",
        "entity.training_master.description": "Oferece exercícios controlados pelo servidor.",
        "minigame.practice.name": "Treino de precisão",
        "minigame.practice.description": "Complete o exercício para receber uma recompensa.",
        "ui.action.close": "Fechar",
        "ui.action.inspect": "Inspecionar",
        "ui.action.practice": "Praticar",
    },
)
"#;

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
        Commands::New(args) => new_project(&args)?,
        Commands::Doctor(args) => doctor(&args)?,
        Commands::Validate(args) => validate_project(&args)?,
        Commands::Content(args) => content(&args)?,
        Commands::Build(args) => project_command(&args, "build")?,
        Commands::Run(args) => project_command(&args, "run")?,
        Commands::Test(args) => project_command(&args, "test")?,
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", render_human_output(&output));
    }

    if matches!(output["command"].as_str(), Some("build" | "run" | "test")) && output["ok"] == false
    {
        std::process::exit(1);
    }
    Ok(())
}

fn new_project(args: &NewArgs) -> Result<Value> {
    let output_dir = absolute_path(args.output.as_path())?;
    let project_name = sanitize_name(&args.name)?;

    if output_dir.exists() && !output_dir.is_dir() {
        bail!(
            "output path exists and is not a directory: {}",
            output_dir.display()
        );
    }

    let mut created_files = vec![
        create_file(
            output_dir.join("Cargo.toml").as_path(),
            cargo_toml_for(&project_name, repo_root().as_path(), args.template).as_bytes(),
        )?,
        create_file(
            output_dir.join("README.md").as_path(),
            format!(
                "# {project_name}\n\n\
                 This project was generated by the Lynx project CLI.\n\n\
                 Authored game data lives in `content/`. The world map, two \
                 connected starter zones, interactable tavern keeper, torch \
                 item, inventory panel, and Brazilian Portuguese source catalog \
                 are ready for `lynx validate --project .`. Use `lynx build --project .`, \
                 `lynx run --project .`, or `lynx test --project .` to validate and invoke Cargo. This \
                 starter executable is not yet a playable game server.\n\n\
                 Interactions describe client intent only; the authoritative \
                 game server must validate each request and determine its outcome.\n"
            )
            .as_bytes(),
        )?,
        create_file(
            output_dir.join("src/main.rs").as_path(),
            project_main(&project_name).as_bytes(),
        )?,
        create_directory(output_dir.join("content/definitions").as_path())?,
        create_file(
            output_dir.join("content/definitions/README.md").as_path(),
            b"Add authored definitions here.\n",
        )?,
        create_file(
            output_dir.join("content/README.md").as_path(),
            b"Content is data, not authority. The server validates every interaction and owns all gameplay outcomes.\n",
        )?,
        create_file(
            output_dir.join(".env.example").as_path(),
            b"LYNX_PG_HOST=127.0.0.1\nLYNX_PG_PORT=5432\nLYNX_PG_DATABASE=lynx\nLYNX_PG_USER=lynx\nLYNX_PG_PASSWORD=change-me\n",
        )?,
    ];
    for (relative_path, contents) in args.template.content_files() {
        created_files.push(create_file(
            output_dir.join(relative_path).as_path(),
            contents.as_bytes(),
        )?);
    }

    Ok(json!({
        "ok": true,
        "command": "new",
        "name": project_name,
        "template": args.template.as_str(),
        "path": output_dir.to_string_lossy(),
        "files": created_files,
    }))
}

fn doctor(args: &DoctorArgs) -> Result<Value> {
    let mut report = json!({
        "ok": true,
        "command": "doctor",
        "project": null,
        "backend": null,
    });

    let has_project = args.project.is_some();
    if let Some(project_path) = args.project.as_ref() {
        let dir = absolute_path(project_path.as_path())?;
        let project_status = validate_dir(dir.as_path())?;
        report["project"] = project_status;
        report["ok"] = Value::Bool(
            report["ok"].as_bool().unwrap() && report["project"]["ok"].as_bool().unwrap(),
        );
    }

    if args.backend {
        let backend_status = check_backend();
        report["backend"] = backend_status;
        report["ok"] = Value::Bool(
            report["ok"].as_bool().unwrap() && report["backend"]["ok"].as_bool().unwrap(),
        );
    }

    if !has_project && !args.backend {
        let dir = absolute_path(Path::new("."))?;
        let project_status = validate_dir(dir.as_path())?;
        report["project"] = project_status;
        report["ok"] = Value::Bool(
            report["ok"].as_bool().unwrap() && report["project"]["ok"].as_bool().unwrap(),
        );
    }

    Ok(report)
}

fn validate_project(args: &ValidateArgs) -> Result<Value> {
    let project_dir = absolute_path(args.project.as_path())?;
    let mut report = validate_dir(project_dir.as_path())?;
    report["command"] = json!("validate");
    report["project"] = json!(project_dir.to_string_lossy());
    Ok(report)
}

fn project_command(args: &ProjectCommandArgs, command: &str) -> Result<Value> {
    let project = absolute_path(&args.project)?;
    let validation = validate_dir(&project)?;
    if validation["ok"] != true {
        return Ok(json!({
            "ok": false, "command": command, "project": project.to_string_lossy(),
            "validation": validation, "cargo": null,
        }));
    }

    let manifest = project.join("Cargo.toml");
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .arg(command)
        .arg("--manifest-path")
        .arg(&manifest)
        .current_dir(&project)
        .output()
        .with_context(|| format!("failed to start cargo {command}"))?;
    Ok(json!({
        "ok": output.status.success(), "command": command,
        "project": project.to_string_lossy(), "validation": validation,
        "cargo": {
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        },
    }))
}

fn content(args: &ContentArgs) -> Result<Value> {
    match &args.command {
        ContentCommand::Build(build_args) => build_content(build_args),
        ContentCommand::Hash(hash_args) => hash_file(hash_args.path.as_path()),
        ContentCommand::Diff(diff_args) => diff_manifests(diff_args),
        ContentCommand::PublishCheck(check_args) => check_publish_manifest(check_args),
    }
}

fn build_content(args: &ContentBuildArgs) -> Result<Value> {
    let project = absolute_path(&args.project)?;
    let validation = validate_dir(&project)?;
    let output = if args.output.is_absolute() {
        args.output.clone()
    } else {
        project.join(&args.output)
    };
    if validation["ok"] != true {
        return Ok(json!({
            "ok": false,
            "command": "content build",
            "project": project.to_string_lossy(),
            "output": output.to_string_lossy(),
            "validation": validation,
            "manifest": null,
        }));
    }
    let version = semver::Version::parse(&args.content_version)
        .context("--content-version must be a semantic version")?;
    let engine = semver::VersionReq::parse(&args.engine)
        .context("--engine must be a semantic-version requirement")?;
    let content_root = project.join("content");
    let (set, report) =
        ContentLoader::new(project_component_registry()?).load_project(&content_root);
    if report.has_errors() {
        bail!("content validation changed while building manifest");
    }
    let manifest = manifest_for(&content_root, &set, version, engine)?;
    manifest
        .write(&output)
        .with_context(|| format!("failed to write {}", output.display()))?;
    Ok(json!({
        "ok": true,
        "command": "content build",
        "project": project.to_string_lossy(),
        "output": output.to_string_lossy(),
        "validation": validation,
        "manifest": manifest,
    }))
}

fn manifest_for(
    root: &Path,
    set: &ContentSet,
    content_version: semver::Version,
    engine: semver::VersionReq,
) -> Result<ContentManifest> {
    let mut manifest = ContentManifest::new(content_version, engine);
    for (id, definition) in &set.definitions {
        let path = &set.sources[id];
        manifest.add_definition(path, definition, &fs::read(root.join(path))?)?;
    }
    for (id, panel) in &set.panels {
        let path = &set.panel_sources[id];
        manifest.add_panel(path, panel, &fs::read(root.join(path))?)?;
    }
    for (id, minigame) in &set.minigames {
        let path = &set.minigame_sources[id];
        manifest.add_minigame(path, minigame, &fs::read(root.join(path))?)?;
    }
    for (id, map) in &set.maps {
        let path = &set.map_sources[id];
        manifest.add_map(path, map, &fs::read(root.join(path))?)?;
    }
    for (locale, path) in &set.catalog_paths {
        manifest.add_locale(path, locale.clone(), &fs::read(root.join(path))?)?;
    }
    Ok(manifest)
}

fn hash_file(path: &Path) -> Result<Value> {
    let path = absolute_path(path)?;
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let digest = sha256_hex(&bytes);
    Ok(json!({
        "ok": true,
        "command": "content hash",
        "path": path.to_string_lossy(),
        "sha256": digest,
    }))
}

fn diff_manifests(args: &DiffArgs) -> Result<Value> {
    let base_path = absolute_path(args.base.as_path())?;
    let next_path = absolute_path(args.next.as_path())?;
    let base = ContentManifest::read(&base_path)
        .with_context(|| format!("failed to read {}", base_path.display()))?;
    let next = ContentManifest::read(&next_path)
        .with_context(|| format!("failed to read {}", next_path.display()))?;

    let result = diff(&base, &next);
    let mut output = serde_json::to_value(&result)?;
    output["ok"] = Value::Bool(result.is_compatible());
    output["command"] = Value::String("content diff".to_string());
    output["base"] = Value::String(base_path.to_string_lossy().into_owned());
    output["next"] = Value::String(next_path.to_string_lossy().into_owned());
    Ok(output)
}

fn check_publish_manifest(args: &PublishCheckArgs) -> Result<Value> {
    let manifest_path = absolute_path(args.manifest.as_path())?;
    let base_path = absolute_path(args.base.as_path())?;
    let candidate = ContentManifest::read(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let base = ContentManifest::read(&base_path)
        .with_context(|| format!("failed to read {}", base_path.display()))?;

    let Some(record) = &candidate.publishing else {
        bail!(
            "manifest has no publishing record: {}",
            manifest_path.display()
        );
    };
    let policy = PublishingPolicy {
        require_review: !args.skip_review,
        allow_self_review: args.allow_self_review,
        require_license: !args.license_optional,
    };
    let result = diff(&base, &candidate);
    let report = check_publish(record, &result, &policy);

    let mut output = serde_json::to_value(&report)?;
    output["ok"] = Value::Bool(report.is_ok());
    output["command"] = Value::String("content publish-check".to_string());
    output["manifest"] = Value::String(manifest_path.to_string_lossy().into_owned());
    output["base"] = Value::String(base_path.to_string_lossy().into_owned());
    output["author"] = Value::String(record.author.to_string());
    output["state"] = Value::String(record.state.as_str().to_string());
    output["revision"] = Value::from(record.revision);
    output["actual_bump"] = serde_json::to_value(result.actual_bump)?;
    output["required_bump"] = serde_json::to_value(result.required_bump)?;
    Ok(output)
}

fn validate_dir(project_dir: &Path) -> Result<Value> {
    if !project_dir.exists() {
        return Ok(json!({
            "ok": false,
            "path": project_dir.to_string_lossy(),
            "errors": [format!("project missing: {}", project_dir.display())],
            "warnings": [],
            "definitions": 0,
            "panels": 0,
            "minigames": 0,
            "maps": 0,
            "catalogs": 0,
        }));
    }

    if !project_dir.is_dir() {
        return Ok(json!({
            "ok": false,
            "path": project_dir.to_string_lossy(),
            "errors": [format!("project path is not a directory: {}", project_dir.display())],
            "warnings": [],
            "definitions": 0,
            "panels": 0,
            "minigames": 0,
            "maps": 0,
            "catalogs": 0,
        }));
    }

    let content_dir = project_dir.join("content");
    let content_root = if content_dir.is_dir() {
        content_dir.as_path()
    } else {
        project_dir
    };
    let registry = project_component_registry()?;
    let loader = ContentLoader::new(registry);
    let (set, report) = loader.load_project(content_root);

    let errors: Vec<String> = report.errors().map(ToString::to_string).collect();
    let warnings: Vec<String> = report.warnings().map(ToString::to_string).collect();

    let ok = errors.is_empty();

    Ok(json!({
        "ok": ok,
        "path": project_dir.to_string_lossy(),
        "errors": errors,
        "warnings": warnings,
        "definitions": set.definitions.len(),
        "panels": set.panels.len(),
        "minigames": set.minigames.len(),
        "maps": set.maps.len(),
        "catalogs": set.catalogs.len(),
    }))
}

fn project_component_registry() -> Result<ComponentRegistry> {
    let mut registry = ComponentRegistry::new();
    let component_schemas = [
        ComponentSchema::new(
            ComponentKey::new("core.zone")?,
            SchemaVersion::new(1, 0),
            vec![
                FieldSchema::required("max_players", FieldType::Int { min: 1, max: 1000 }),
                FieldSchema::required(
                    "cell_size_m",
                    FieldType::Float {
                        min: 0.25,
                        max: 64.0,
                    },
                ),
            ],
            Visibility::ServerOnly,
            PersistencePolicy::StaticDefinition,
        ),
        ComponentSchema::new(
            ComponentKey::new("core.interactable")?,
            SchemaVersion::new(1, 0),
            vec![
                FieldSchema::required(
                    "verbs",
                    FieldType::List {
                        item: Box::new(FieldType::Enum {
                            values: ["inspect", "talk", "practice"]
                                .into_iter()
                                .map(str::to_owned)
                                .collect(),
                        }),
                        max_len: 8,
                    },
                ),
                FieldSchema::required(
                    "range_m",
                    FieldType::Float {
                        min: 0.1,
                        max: 20.0,
                    },
                ),
                FieldSchema::required("requires_line_of_sight", FieldType::Bool),
            ],
            Visibility::Client,
            PersistencePolicy::StaticDefinition,
        ),
        ComponentSchema::new(
            ComponentKey::new("core.item")?,
            SchemaVersion::new(1, 0),
            vec![
                FieldSchema::required("stack_max", FieldType::Int { min: 1, max: 999 }),
                FieldSchema::required(
                    "weight_kg",
                    FieldType::Float {
                        min: 0.0,
                        max: 1000.0,
                    },
                ),
            ],
            Visibility::Client,
            PersistencePolicy::StaticDefinition,
        ),
    ];
    for schema in component_schemas {
        registry
            .register(schema)
            .context("failed to register starter component schema")?;
    }
    Ok(registry)
}

fn check_backend() -> Value {
    let url = backend_url();
    let client = NakamaClient::new(NakamaConfig::new(url));

    let mut result = match client {
        Ok(client) => match client.health() {
            Ok(_) => json!({
                "ok": true,
                "healthy": true,
                "message": "Nakama health check passed",
            }),
            Err(_) => json!({
                "ok": false,
                "healthy": false,
                "message": "Nakama health check failed",
            }),
        },
        Err(_) => json!({
            "ok": false,
            "healthy": false,
            "message": "Nakama configuration is invalid",
        }),
    };
    match lynx_world::schema::inspect_env() {
        Ok(status) => {
            result["ok"] = json!(result["ok"].as_bool().unwrap_or(false) && status.compatible());
            result["schema"] = json!(status);
        }
        Err(message) => {
            result["ok"] = json!(false);
            result["schema"] = json!({"error": message});
        }
    }
    result
}

fn backend_url() -> String {
    std::env::var("LYNX_NAKAMA_URL")
        .or_else(|_| std::env::var("NAKAMA_URL"))
        .or_else(|_| std::env::var("NAKAMA_BASE_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:7350".to_string())
}

fn render_human_output(output: &Value) -> String {
    if matches!(output["command"].as_str(), Some("build" | "run" | "test")) {
        let command = output["command"].as_str().unwrap_or("");
        let mut text = format!(
            "Project {command}: {}",
            if output["ok"] == true {
                "passed"
            } else {
                "failed"
            }
        );
        if let Some(cargo) = output["cargo"].as_object() {
            for key in ["stdout", "stderr"] {
                if let Some(contents) = cargo[key].as_str().filter(|s| !s.is_empty()) {
                    text.push('\n');
                    text.push_str(contents.trim_end());
                }
            }
        } else {
            for error in output["validation"]["errors"]
                .as_array()
                .into_iter()
                .flatten()
            {
                text.push_str("\n- ");
                text.push_str(error.as_str().unwrap_or(""));
            }
        }
        return text;
    }
    if output["command"] == "new" {
        let ok = output["ok"].as_bool().unwrap_or(false);
        let path = output["path"].as_str().unwrap_or(".");
        let template = output["template"].as_str().unwrap_or("unknown");
        let name = output["name"].as_str().unwrap_or("unknown");
        let empty_files = Vec::new();
        let files = output["files"].as_array().unwrap_or(&empty_files);
        if ok {
            let mut lines = vec![format!(
                "Created project '{name}' from template '{template}' at {path}"
            )];
            for file in files {
                lines.push(format!("- {}", file.as_str().unwrap_or("")));
            }
            return lines.join("\n");
        }
        return format!(
            "Project creation failed: {}",
            output["message"].as_str().unwrap_or("unknown error")
        );
    }

    if output["command"] == "content hash" {
        let sha = output["sha256"].as_str().unwrap_or("unknown");
        return format!("SHA-256: {sha}");
    }

    if output["command"] == "content build" {
        return render_content_build(output);
    }

    if output["command"] == "content diff" {
        return render_content_diff(output);
    }

    if output["command"] == "content publish-check" {
        return render_publish_check(output);
    }

    if output["command"] == "doctor" {
        let ok = output["ok"].as_bool().unwrap_or(false);
        let backend = output["backend"].as_object();
        let project = output["project"].as_object();
        let mut lines = vec![format!(
            "Doctor status: {}",
            if ok { "ok" } else { "issues found" }
        )];
        if let Some(backend) = backend {
            lines.push(format!(
                "- backend: {} ({})",
                backend["healthy"],
                backend["message"].as_str().unwrap_or("")
            ));
            lines.push(format!("- schema: {}", backend["schema"]));
        }
        if let Some(project) = project {
            lines.push(format!(
                "- project: {} ({} errors, {} warnings)",
                project["ok"],
                project["errors"].as_array().map_or(0, Vec::len),
                project["warnings"].as_array().map_or(0, Vec::len)
            ));
        }
        return lines.join("\n");
    }

    if output["ok"].as_bool().unwrap_or(false) {
        let path = output["path"].as_str().unwrap_or(".");
        return format!("Validation passed for {path}");
    }

    let errors = output["errors"].as_array().map_or(0, Vec::len);
    format!("Validation failed with {errors} error(s)")
}

fn render_content_build(output: &Value) -> String {
    format!(
        "Content build {}: {}",
        if output["ok"] == true {
            "passed"
        } else {
            "failed"
        },
        output["output"].as_str().unwrap_or("manifest not written")
    )
}

fn render_content_diff(output: &Value) -> String {
    let ok = output["ok"].as_bool().unwrap_or(false);
    let base = output["base_version"].as_str().unwrap_or("unknown");
    let next = output["next_version"].as_str().unwrap_or("unknown");
    let actual = output["actual_bump"].as_str().unwrap_or("unknown");
    let required = output["required_bump"].as_str().unwrap_or("unknown");
    let mut lines = vec![format!(
        "Content diff {base} -> {next}: {} (actual {actual} bump, required {required} bump)",
        if ok { "compatible" } else { "incompatible" }
    )];
    for key in ["added", "removed"] {
        for entry in output[key].as_array().into_iter().flatten() {
            if let Some(id) = entry["id"].as_str() {
                lines.push(format!("- {key}: {id}"));
            }
        }
    }
    for entry in output["changed"].as_array().into_iter().flatten() {
        if let Some(id) = entry["id"].as_str() {
            lines.push(format!("- changed: {id}"));
        }
    }
    for key in ["added_locales", "removed_locales", "changed_locales"] {
        for locale in output[key].as_array().into_iter().flatten() {
            if let Some(locale) = locale.as_str() {
                lines.push(format!("- {key}: {locale}"));
            }
        }
    }
    push_diagnostics(&mut lines, &output["report"]["diagnostics"]);
    lines.join("\n")
}

fn render_publish_check(output: &Value) -> String {
    let ok = output["ok"].as_bool().unwrap_or(false);
    let author = output["author"].as_str().unwrap_or("unknown");
    let state = output["state"].as_str().unwrap_or("unknown");
    let revision = output["revision"].as_u64().unwrap_or(0);
    let mut lines = vec![format!(
        "Publish check for {author} revision {revision} ({state}): {}",
        if ok { "allowed" } else { "blocked" }
    )];
    push_diagnostics(&mut lines, &output["diagnostics"]);
    lines.join("\n")
}

fn push_diagnostics(lines: &mut Vec<String>, diagnostics: &Value) {
    for diagnostic in diagnostics.as_array().into_iter().flatten() {
        lines.push(format!(
            "- {} {}: {}",
            diagnostic["severity"].as_str().unwrap_or("?"),
            diagnostic["code"].as_str().unwrap_or("?"),
            diagnostic["message"].as_str().unwrap_or("")
        ));
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn sanitize_name(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("project name must not be empty");
    }
    let cleaned: String = trimmed
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .collect();
    if cleaned.is_empty() {
        bail!("project name must contain at least one letter or digit");
    }
    Ok(cleaned)
}

fn create_directory(path: &Path) -> Result<String> {
    if !path.exists() {
        fs::create_dir_all(path)
            .with_context(|| format!("failed to create directory {}", path.display()))?;
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

fn create_file(path: &Path, contents: &[u8]) -> Result<String> {
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path.to_string_lossy().replace('\\', "/"))
}

fn project_main(project_name: &str) -> String {
    format!("fn main() {{\n    println!(\"Welcome to {project_name}!\");\n}}\n")
}

fn cargo_toml_for(project_name: &str, repo_root: &Path, _template: Template) -> String {
    let repo_root = repo_root.to_string_lossy().replace('\\', "/");

    format!(
        "[package]\nname = \"{project_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nengine_core = {{ path = \"{repo_root}/crates/engine_core\" }}\nengine_content = {{ path = \"{repo_root}/crates/engine_content\" }}\nengine_sim = {{ path = \"{repo_root}/crates/engine_sim\" }}\nbackend_nakama = {{ path = \"{repo_root}/crates/backend_nakama\" }}\nserde = {{ version = \"1\", features = [\"derive\"] }}\nserde_json = \"1\"\nron = \"0.12\"\n\n[profile.dev]\ndebug = 1\n\n[profile.release]\nlto = \"thin\"\ncodegen-units = 1\n",
    )
}
