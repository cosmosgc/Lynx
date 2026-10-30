use super::absolute_path;
use crate::{NewArgs, Template};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

impl Template {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Mmorpg => "mmorpg",
        }
    }

    pub(crate) fn content_files(self) -> Vec<(&'static str, &'static str)> {
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

pub(crate) fn new_project(args: &NewArgs) -> Result<Value> {
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

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
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
