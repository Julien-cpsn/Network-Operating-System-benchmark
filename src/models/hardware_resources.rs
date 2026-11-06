use clap::ValueEnum;
use strum::{Display, VariantArray};

#[derive(Debug, Clone, ValueEnum, VariantArray, Display)]
pub enum HardwareResources {
    /// Low resources (2 vCPU, 4096 MB RAM)
    #[clap(name = "lr")]
    #[strum(to_string = "Low Resources")]
    LowResources,
    /// Medium resources (4 vCPU, 8192 MB RAM)
    #[clap(name = "mr")]
    #[strum(to_string = "Medium Resources")]
    MediumResources,
    /// High resources (8 vCPU, 16384 MB RAM)
    #[clap(name = "hr")]
    #[strum(to_string = "High Resources")]
    HighResources,
}

impl HardwareResources {
    pub fn to_vcpu_and_ram(&self) -> (u32, u32) {
        match self {
            HardwareResources::LowResources => (2, 4096),
            HardwareResources::MediumResources => (4, 8192),
            HardwareResources::HighResources => (8, 16384)
        }
    }

    pub fn to_command(&self) -> &str {
        match self {
            HardwareResources::LowResources => "lr",
            HardwareResources::MediumResources => "mr",
            HardwareResources::HighResources => "hr"
        }
    }
}
