use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WuwaOffset {
    pub name: String,
    pub global_gworld: u64,
    pub uworld_persistentlevel: u64,
    pub uworld_owninggameinstance: u64,
    pub ulevel_lastworldorigin: u64,
    pub ugameinstance_localplayers: u64,
    pub uplayer_playercontroller: u64,
    pub aplayercontroller_acknowlegedpawn: u64,
    pub aactor_rootcomponent: u64,
    pub uscenecomponent_componenttoworld: u64, // relativelocation에서 +0xB4
    // pub uscenecomponent_relativerotation: u64,
}
