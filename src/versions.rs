#[cfg(target_os = "macos")]
const OS: &str = "macos";

#[cfg(target_os = "linux")]
const OS: &str = "linux";

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const OS: &str = "undefined";

#[cfg(feature = "electrs_0_8_10")]
const VERSION: &str = "v0.8.10";

#[cfg(feature = "esplora_a33e97e1")]
const VERSION: &str = "esplora_a33e97e1a1fc63fa9c20a116bb92579bbf43b254";

#[cfg(feature = "esplora_ee3646fc")]
const VERSION: &str = "esplora_ee3646fc4a6a3f407fae3cf726c31c3230cddc52";

#[cfg(feature = "electrs_0_9_1")]
const VERSION: &str = "v0.9.1";

#[cfg(feature = "electrs_0_9_11")]
const VERSION: &str = "v0.9.11";

#[cfg(feature = "electrs_0_10_6")]
const VERSION: &str = "v0.10.6";

#[cfg(not(any(
    feature = "electrs_0_8_10",
    feature = "electrs_0_9_1",
    feature = "electrs_0_9_11",
    feature = "electrs_0_10_6",
    feature = "esplora_a33e97e1",
    feature = "esplora_ee3646fc",
)))]
const VERSION: &str = "NA";

pub const HAS_FEATURE: bool = cfg!(any(
    feature = "electrs_0_8_10",
    feature = "electrs_0_9_1",
    feature = "electrs_0_9_11",
    feature = "electrs_0_10_6",
    feature = "esplora_a33e97e1",
    feature = "esplora_ee3646fc",
));

pub fn electrs_name() -> String {
    format!("electrs_{}_{}", OS, VERSION)
}
