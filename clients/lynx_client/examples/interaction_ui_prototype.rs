//! THROWAWAY: Can `bevy_immediate` make entity panels readable and easy to use?
//! Three native layouts over the same in-memory fixture: sidebar, dialog, dock.
//! No network, credentials, or persistence. Promote only after user review.
use bevy::prelude::*;
use bevy_immediate::{
    BevyImmediatePlugin, Imm, ImmCtx,
    ui::{CapsUi, activated::ImmUiActivated, text::ImmUiText},
};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Lynx - interaction UI PROTOTYPE".into(),
                resolution: (1280, 800).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins((
            lynx_client::fonts::ClientFontPlugin,
            BevyImmediatePlugin::<CapsUi>::new(),
        ))
        .init_resource::<Demo>()
        .add_systems(Startup, scene)
        .add_systems(Update, (keyboard, draw).chain())
        .run();
}

#[derive(Resource, Debug)]
struct Demo {
    variant: usize,
    open: bool,
    near: bool,
    guess: String,
    attempts: u8,
    notice: String,
}

impl Default for Demo {
    fn default() -> Self {
        Self {
            variant: 0,
            open: true,
            near: true,
            guess: String::new(),
            attempts: 8,
            notice: "Fixture local: experimente o código 427.".into(),
        }
    }
}

impl Demo {
    fn submit(&mut self) {
        if !self.open || self.guess.len() != 3 || self.attempts == 0 {
            return;
        }
        self.attempts -= 1;
        if self.guess == "427" {
            self.open = false;
            self.notice = "Fixture: cofre desbloqueado; painel fechado.".into();
        } else {
            self.notice = self
                .guess
                .bytes()
                .zip(b"427")
                .map(|(digit, answer)| match digit.cmp(answer) {
                    std::cmp::Ordering::Equal => "correto",
                    std::cmp::Ordering::Less => "maior",
                    std::cmp::Ordering::Greater => "menor",
                })
                .collect::<Vec<_>>()
                .join(" | ");
        }
        self.guess.clear();
    }
}

fn scene(mut commands: Commands) {
    commands.spawn(Camera2d);
    for (name, x, y, color) in [
        ("Jogador", 0., 0., Color::srgb(0.2, 0.8, 1.)),
        ("Cofre", 0., -90., Color::srgb(1., 0.7, 0.2)),
        ("Barril", -130., 30., Color::srgb(0.7, 0.5, 0.2)),
    ] {
        commands.spawn((
            Sprite::from_color(color, Vec2::splat(28.)),
            Transform::from_xyz(x, y, 0.),
        ));
        commands.spawn((
            Text2d::new(name),
            TextFont {
                font_size: bevy::text::FontSize::Px(18.0),
                ..default()
            },
            Transform::from_xyz(x, y + 30., 1.),
        ));
    }
}

#[allow(clippy::needless_pass_by_value)]
fn keyboard(keys: Res<ButtonInput<KeyCode>>, mut demo: ResMut<Demo>) {
    if keys.just_pressed(KeyCode::ArrowLeft) {
        demo.variant = (demo.variant + 2) % 3;
    }
    if keys.just_pressed(KeyCode::ArrowRight) {
        demo.variant = (demo.variant + 1) % 3;
    }
    if keys.just_pressed(KeyCode::Escape) {
        demo.open = false;
        demo.guess.clear();
    }
    if !demo.open {
        return;
    }
    if keys.just_pressed(KeyCode::Backspace) {
        demo.guess.pop();
    }
    for (digit, key) in "0123456789".chars().zip([
        KeyCode::Digit0,
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ]) {
        if keys.just_pressed(key) && demo.guess.len() < 3 {
            demo.guess.push(digit);
        }
    }
    if keys.just_pressed(KeyCode::Enter) {
        demo.submit();
    }
}

fn label(ui: &mut Imm<CapsUi>, id: &str, text: impl Into<String>) {
    ui.ch_id(id)
        .on_spawn_insert(|| TextFont {
            font_size: bevy::text::FontSize::Px(18.0),
            ..default()
        })
        .text(text.into());
}

fn button(ui: &mut Imm<CapsUi>, id: &str, text: &str) -> bool {
    ui.ch_id(id)
        .on_spawn_insert(|| {
            (
                bevy::ui_widgets::Button,
                Node {
                    padding: UiRect::axes(px(14), px(10)),
                    min_height: px(42),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border_radius: BorderRadius::all(px(5)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.13, 0.24, 0.32)),
            )
        })
        .add(|ui| {
            label(ui, "caption", text);
        })
        .activated()
}

fn card(node: Node) -> impl Bundle {
    (
        node,
        BackgroundColor(Color::srgba(0.035, 0.055, 0.08, 0.97)),
    )
}

fn panel(ui: &mut Imm<CapsUi>, demo: &mut Demo, keypad: bool) {
    label(ui, "title", "Cofre / fechadura");
    label(ui, "hint", "Código de 3 dígitos · Enter para tentar");
    label(
        ui,
        "code",
        format!("[ {:_<3} ]   Tentativas: {}", demo.guess, demo.attempts),
    );
    if keypad {
        ui.ch_id("keypad")
            .on_spawn_insert(|| Node {
                display: Display::Grid,
                grid_template_columns: RepeatedGridTrack::flex(3, 1.),
                row_gap: px(6),
                column_gap: px(6),
                ..default()
            })
            .add(|ui| {
                for digit in "1234567890".chars() {
                    let text = digit.to_string();
                    if button(ui, &text, &text) && demo.guess.len() < 3 {
                        demo.guess.push(digit);
                    }
                }
            });
    }
    ui.ch_id("controls")
        .on_spawn_insert(|| Node {
            flex_direction: FlexDirection::Row,
            column_gap: px(8),
            ..default()
        })
        .add(|ui| {
            if button(ui, "try", "Tentar") {
                demo.submit();
            }
            if button(ui, "erase", "Apagar") {
                demo.guess.pop();
            }
            if button(ui, "close", "Fechar") {
                demo.open = false;
                demo.guess.clear();
            }
        });
    label(ui, "notice", &demo.notice);
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
fn draw(ctx: ImmCtx<CapsUi>, mut demo: ResMut<Demo>) {
    ctx.build_immediate_root("interaction-prototype").ch_id("canvas").on_spawn_insert(|| Node {
        width: percent(100), height: percent(100), ..default()
    }).add(|ui| {
        ui.ch_id("fixture").on_spawn_insert(|| card(Node {
            position_type: PositionType::Absolute, top: px(16), left: px(16), width: px(400),
            padding: UiRect::all(px(16)), row_gap: px(10), flex_direction: FlexDirection::Column, ..default()
        })).add(|ui| {
            label(ui, "title", "PROTÓTIPO · bevy_immediate 0.9");
            label(ui, "state", format!("ui.lockpick | alvo: vault-1\nAberto: {} | perto: {}\nTentativas: {} | entrada: {}", demo.open, demo.near, demo.attempts, demo.guess));
            if button(ui, "open", "Perto / abrir cofre") { demo.near = true; demo.open = true; demo.guess.clear(); }
            if button(ui, "far", "Longe / fechar por distância") { demo.near = false; demo.open = false; demo.guess.clear(); demo.notice = "Fixture: fora do alcance.".into(); }
            if button(ui, "reset", "Reiniciar fixture") { let variant = demo.variant; *demo = Demo { variant, ..default() }; }
        });
        if demo.open {
            let variant = demo.variant;
            ui.ch_id(("variant", variant)).on_spawn_insert(move || {
                let mut node = Node { position_type: PositionType::Absolute, padding: UiRect::all(px(20)),
                    row_gap: px(14), flex_direction: FlexDirection::Column, ..default() };
                match variant {
                    0 => { node.right = px(16); node.top = px(16); node.width = px(400); }
                    1 => { node.left = percent(35); node.top = px(80); node.width = px(460); }
                    _ => { node.left = percent(25); node.right = px(16); node.bottom = px(100); }
                }
                card(node)
            }).add(|ui| { panel(ui, &mut demo, variant == 1); });
        }
        ui.ch_id("switcher").on_spawn_insert(|| card(Node {
            position_type: PositionType::Absolute, bottom: px(16), left: percent(25),
            padding: UiRect::all(px(10)), column_gap: px(12), align_items: AlignItems::Center, ..default()
        })).add(|ui| {
            if button(ui, "previous", "<") { demo.variant = (demo.variant + 2) % 3; }
            label(ui, "variant", ["A · painel lateral", "B · diálogo com teclado", "C · barra inferior"][demo.variant]);
            if button(ui, "next", ">") { demo.variant = (demo.variant + 1) % 3; }
        });
    });
}
