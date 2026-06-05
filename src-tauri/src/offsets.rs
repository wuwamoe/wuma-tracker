use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GWorldScanConfig {
    pub enabled: bool,
    /// MOV 명령어 앞 바이트열 (e.g. "48 8B 1D")
    pub prefix: String,
    /// disp32 직후 바이트열, ?? = wildcard (e.g. "48 85 DB 74 ?? 41 B0 01")
    pub suffix: String,
}

impl Default for GWorldScanConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prefix: "48 8B 1D".to_string(),
            suffix: "48 85 DB 74 ?? 41 B0 01".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackerConfig {
    pub last_updated: String,
    pub gworld_scan: GWorldScanConfig,
    pub offsets: Vec<WuwaOffset>,
}

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
    pub uscenecomponent_componenttoworld: u64,
}
