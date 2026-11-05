# Bevy crates

The workspace directly declares `bevy` version `0.20.0` in the root
`Cargo.toml`. The `lynx_client` crate uses it, with windowing and rendering
features enabled through its optional `window` feature.

The following 57 Bevy crates are present in `Cargo.lock`. Most are transitive
dependencies of `bevy` and its selected features; their presence in the lockfile
does not mean they are direct workspace dependencies.

- `bevy`
- `bevy_a11y`
- `bevy_android`
- `bevy_animation`
- `bevy_animation_macros`
- `bevy_app`
- `bevy_asset`
- `bevy_asset_macros`
- `bevy_camera`
- `bevy_clipboard`
- `bevy_color`
- `bevy_core_pipeline`
- `bevy_curve`
- `bevy_derive`
- `bevy_diagnostic`
- `bevy_ecs`
- `bevy_ecs_macro_logic`
- `bevy_ecs_macros`
- `bevy_encase_derive`
- `bevy_extract`
- `bevy_extract_macros`
- `bevy_gizmos`
- `bevy_gizmos_macros`
- `bevy_gizmos_render`
- `bevy_image`
- `bevy_input`
- `bevy_input_focus`
- `bevy_internal`
- `bevy_light`
- `bevy_log`
- `bevy_macro_utils`
- `bevy_material`
- `bevy_material_macros`
- `bevy_math`
- `bevy_mesh`
- `bevy_platform`
- `bevy_post_process`
- `bevy_ptr`
- `bevy_reflect`
- `bevy_reflect_derive`
- `bevy_render`
- `bevy_render_macros`
- `bevy_shader`
- `bevy_shape`
- `bevy_sprite`
- `bevy_sprite_render`
- `bevy_state`
- `bevy_state_macros`
- `bevy_tasks`
- `bevy_text`
- `bevy_time`
- `bevy_transform`
- `bevy_ui`
- `bevy_ui_render`
- `bevy_utils`
- `bevy_window`
- `bevy_winit`
