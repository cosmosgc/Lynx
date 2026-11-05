//! Full Latin font coverage, embedded so packaged clients need no font path.
use bevy::prelude::*;

#[derive(Debug)]
pub struct ClientFontPlugin;

impl Plugin for ClientFontPlugin {
    fn build(&self, app: &mut App) {
        let font =
            Font::from_bytes(include_bytes!("../assets/fonts/FiraMono-Regular.ttf").to_vec());
        app.world_mut()
            .resource_mut::<Assets<Font>>()
            .insert(bevy::asset::AssetId::default(), font)
            .expect("default font asset is replaceable");
    }
}
