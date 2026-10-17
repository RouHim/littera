//! FR-005 / FR-014: name the command that installs the GPU's missing Vulkan driver.
//!
//! Without an ICD for the GPU in the machine the loader reports no device and
//! `engine::probe_devices` can only say "no Vulkan device found" — which names
//! no fix, so the system check had nothing to show. This module reads the two
//! inventories the loader itself works from: the display controllers in
//! `/sys/bus/pci/devices` and the manifests in `/usr/share/vulkan/icd.d`. A GPU
//! whose vendor has no manifest names the driver that is missing.
//!
//! Nothing here assumes a distribution: the package manager is looked up on
//! `PATH`, and a family whose package name for the vendor is not known keeps the
//! vendor wording instead of a guessed command. The manifests and the PCI
//! inventory are the loader's and the kernel's own interfaces, so the check
//! reads the same facts the fix has to change.
//!
//! Only the *presence* of a manifest is checked. A manifest that names a library
//! the kernel driver does not provide, a GPU the driver rejects and a machine
//! without any GPU all look alike from here: they keep the generic remedy for
//! the check's non-driver failures.

use std::path::{Path, PathBuf};

/// Where `vulkan-icd-loader` looks for ICD manifests.
const ICD_DIR: &str = "/usr/share/vulkan/icd.d";
/// PCI inventory of the running kernel.
const PCI_DEVICES: &str = "/sys/bus/pci/devices";

/// Package managers whose package name for a vendor's ICD is stable across the
/// distributions that use them: pacman (Arch), apt (Debian/Ubuntu), dnf
/// (Fedora). A distribution whose manager is not listed keeps the vendor
/// wording — a wrong package name is worse than none.
const MANAGERS: &[(&str, PackageManager)] = &[
    ("pacman", PackageManager::Pacman),
    ("apt", PackageManager::Apt),
    ("dnf", PackageManager::Dnf),
];

/// ICD manifest families per vendor: the file names the loader picks up, so a
/// renamed manifest cannot silently turn into a false "driver missing" advice.
const MANIFEST_PREFIXES_INTEL: &[&str] = &["intel_icd"];
const MANIFEST_PREFIXES_AMD: &[&str] = &["radeon_icd", "amdvlk"];
const MANIFEST_PREFIXES_NVIDIA: &[&str] = &["nvidia_icd", "nouveau_icd"];

/// PCI vendor of a display controller, as far as the remedy needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vendor {
    Intel,
    Amd,
    Nvidia,
}

impl Vendor {
    fn from_pci(vendor: u16) -> Option<Self> {
        match vendor {
            0x8086 => Some(Vendor::Intel),
            0x1002 | 0x1022 => Some(Vendor::Amd),
            0x10de => Some(Vendor::Nvidia),
            _ => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Vendor::Intel => "Intel",
            Vendor::Amd => "AMD",
            Vendor::Nvidia => "NVIDIA",
        }
    }

    const fn manifest_prefixes(self) -> &'static [&'static str] {
        match self {
            Vendor::Intel => MANIFEST_PREFIXES_INTEL,
            Vendor::Amd => MANIFEST_PREFIXES_AMD,
            Vendor::Nvidia => MANIFEST_PREFIXES_NVIDIA,
        }
    }
}

/// The package manager this machine installs with, as far as naming a command
/// needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Pacman,
    Apt,
    Dnf,
    /// No manager [`MANAGERS`] knows about on `PATH`.
    Unknown,
}

impl PackageManager {
    /// The first known manager found on `PATH`.
    fn detect() -> Self {
        match std::env::var_os("PATH") {
            Some(path) => {
                Self::detect_from(&std::env::split_paths(&path).collect::<Vec<PathBuf>>())
            }
            None => Self::Unknown,
        }
    }

    fn detect_from(dirs: &[PathBuf]) -> Self {
        MANAGERS
            .iter()
            .find_map(|(binary, manager)| {
                dirs.iter()
                    .any(|dir| dir.join(binary).is_file())
                    .then_some(*manager)
            })
            .unwrap_or(Self::Unknown)
    }

    /// The install command for a vendor, or `None` when this manager's package
    /// name for it is unknown.
    fn install(self, vendor: Vendor) -> Option<String> {
        match (self, vendor) {
            (Self::Pacman, Vendor::Intel) => Some("sudo pacman -S vulkan-intel".to_owned()),
            (Self::Pacman, Vendor::Amd) => Some("sudo pacman -S vulkan-radeon".to_owned()),
            (Self::Pacman, Vendor::Nvidia) => Some("sudo pacman -S nvidia-utils".to_owned()),
            // Mesa ships both the Intel and the AMD ICDs in one package here.
            (Self::Apt, Vendor::Intel | Vendor::Amd) => {
                Some("sudo apt install mesa-vulkan-drivers".to_owned())
            }
            (Self::Dnf, Vendor::Intel | Vendor::Amd) => {
                Some("sudo dnf install mesa-vulkan-drivers".to_owned())
            }
            // Debian and Fedora split the NVIDIA ICD over driver packages whose
            // names move between releases, and an unknown manager names nothing.
            _ => None,
        }
    }
}

/// The fix for the first GPU whose vendor has no ICD manifest, or `None` when
/// every GPU in the machine is covered — then the missing device has another
/// cause and the generic remedy stands.
fn missing_driver(
    vendors: &[Vendor],
    manifests: &[String],
    manager: PackageManager,
) -> Option<String> {
    let vendor = vendors
        .iter()
        .find(|vendor| !covered(**vendor, manifests))?;
    Some(match manager.install(*vendor) {
        Some(command) => format!("{command} — no Vulkan ICD for the {} GPU", vendor.label()),
        None => format!(
            "install a Vulkan ICD for the {} GPU — no manifest in {ICD_DIR} claims it",
            vendor.label()
        ),
    })
}

/// Whether a manifest in the loader's search path claims this vendor.
fn covered(vendor: Vendor, manifests: &[String]) -> bool {
    manifests.iter().any(|manifest| {
        vendor
            .manifest_prefixes()
            .iter()
            .any(|prefix| manifest.starts_with(prefix))
    })
}

/// Live probe for the system check: the command that installs the missing
/// driver, or `None` when the machine's GPUs are covered.
pub fn advice() -> Option<String> {
    missing_driver(&vendor_inventory(), &manifests(), PackageManager::detect())
}

/// Display controllers in this machine (`0x03xxxx`: VGA, XGA, 3D, other), in
/// `/sys/bus/pci/devices` order.
fn vendor_inventory() -> Vec<Vendor> {
    let Ok(entries) = std::fs::read_dir(PCI_DEVICES) else {
        return Vec::new();
    };
    let mut vendors = Vec::new();
    for entry in entries.flatten() {
        let device = entry.path();
        let class = read_hex(&device.join("class"));
        let vendor = read_hex(&device.join("vendor"));
        let (Some(class), Some(vendor)) = (class, vendor) else {
            continue;
        };
        if class >> 16 != 0x03 {
            continue;
        }
        let Ok(vendor) = u16::try_from(vendor) else {
            continue;
        };
        if let Some(vendor) = Vendor::from_pci(vendor)
            && !vendors.contains(&vendor)
        {
            vendors.push(vendor);
        }
    }
    vendors
}

/// File names of the manifests the loader would read; empty when the loader is
/// not installed at all.
fn manifests() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(Path::new(ICD_DIR)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".json"))
        .collect()
}

/// A sysfs attribute like `0x030000` or `0x8086`, without the radix prefix.
fn read_hex(path: &Path) -> Option<u32> {
    let value = std::fs::read_to_string(path).ok()?;
    u32::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifests(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lingua-vulkan-icd-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the temp dir");
        dir
    }

    #[test]
    fn contract_literals_are_the_ones_the_check_shows() {
        assert_eq!(ICD_DIR, "/usr/share/vulkan/icd.d");
        assert_eq!(PCI_DEVICES, "/sys/bus/pci/devices");
        assert_eq!(Vendor::Intel.label(), "Intel");
        assert_eq!(Vendor::Amd.label(), "AMD");
        assert_eq!(Vendor::Nvidia.label(), "NVIDIA");
    }

    #[test]
    fn pci_ids_map_to_their_vendor() {
        assert_eq!(Vendor::from_pci(0x8086), Some(Vendor::Intel));
        assert_eq!(Vendor::from_pci(0x1002), Some(Vendor::Amd));
        assert_eq!(Vendor::from_pci(0x1022), Some(Vendor::Amd));
        assert_eq!(Vendor::from_pci(0x10de), Some(Vendor::Nvidia));
        assert_eq!(Vendor::from_pci(0x1af4), None);
    }

    #[test]
    fn the_package_manager_is_read_from_path() {
        let dir = temp_dir("manager");
        let empty = temp_dir("manager-empty");
        std::fs::write(dir.join("dnf"), b"").expect("stage a manager");

        assert_eq!(
            PackageManager::detect_from(std::slice::from_ref(&dir)),
            PackageManager::Dnf
        );
        assert_eq!(
            PackageManager::detect_from(std::slice::from_ref(&empty)),
            PackageManager::Unknown
        );
        assert_eq!(PackageManager::detect_from(&[]), PackageManager::Unknown);

        std::fs::remove_dir_all(&dir).expect("clean the temp dir");
        std::fs::remove_dir_all(&empty).expect("clean the temp dir");
    }

    #[test]
    fn install_commands_name_the_vendor_package_per_manager() {
        assert_eq!(
            PackageManager::Pacman.install(Vendor::Intel).as_deref(),
            Some("sudo pacman -S vulkan-intel")
        );
        assert_eq!(
            PackageManager::Pacman.install(Vendor::Amd).as_deref(),
            Some("sudo pacman -S vulkan-radeon")
        );
        assert_eq!(
            PackageManager::Pacman.install(Vendor::Nvidia).as_deref(),
            Some("sudo pacman -S nvidia-utils")
        );
        assert_eq!(
            PackageManager::Apt.install(Vendor::Amd).as_deref(),
            Some("sudo apt install mesa-vulkan-drivers")
        );
        assert_eq!(
            PackageManager::Dnf.install(Vendor::Intel).as_deref(),
            Some("sudo dnf install mesa-vulkan-drivers")
        );
        // Unknown managers and the NVIDIA splits keep the vendor wording.
        assert_eq!(PackageManager::Unknown.install(Vendor::Intel), None);
        assert_eq!(PackageManager::Apt.install(Vendor::Nvidia), None);
        assert_eq!(PackageManager::Dnf.install(Vendor::Nvidia), None);
    }

    #[test]
    fn a_gpu_without_a_manifest_names_its_driver() {
        // The machine this was written on: an Intel iGPU with only the AMD
        // manifest installed by an earlier setup.
        let advice = missing_driver(
            &[Vendor::Intel],
            &manifests(&["radeon_icd.json"]),
            PackageManager::Pacman,
        );
        assert_eq!(
            advice.as_deref(),
            Some("sudo pacman -S vulkan-intel — no Vulkan ICD for the Intel GPU")
        );

        assert_eq!(
            missing_driver(
                &[Vendor::Amd],
                &manifests(&["intel_icd.x86_64.json"]),
                PackageManager::Apt
            )
            .as_deref(),
            Some("sudo apt install mesa-vulkan-drivers — no Vulkan ICD for the AMD GPU")
        );
    }

    #[test]
    fn a_manager_without_a_package_name_keeps_the_vendor_wording() {
        assert_eq!(
            missing_driver(
                &[Vendor::Nvidia],
                &manifests(&["intel_icd.x86_64.json"]),
                PackageManager::Dnf
            )
            .as_deref(),
            Some(
                "install a Vulkan ICD for the NVIDIA GPU — no manifest in \
                 /usr/share/vulkan/icd.d claims it"
            )
        );
    }

    #[test]
    fn covered_gpus_need_no_advice() {
        assert_eq!(
            missing_driver(
                &[Vendor::Intel, Vendor::Amd],
                &manifests(&["intel_icd.x86_64.json", "radeon_icd.x86_64.json"]),
                PackageManager::Pacman
            ),
            None
        );
        // Alternative drivers count as coverage: amdvlk, nouveau and the 32-bit
        // manifest next to the 64-bit one.
        assert_eq!(
            missing_driver(
                &[Vendor::Amd],
                &manifests(&["amdvlk64.json"]),
                PackageManager::Pacman
            ),
            None
        );
        assert_eq!(
            missing_driver(
                &[Vendor::Nvidia],
                &manifests(&["nouveau_icd.x86_64.json"]),
                PackageManager::Unknown
            ),
            None
        );
    }

    #[test]
    fn a_machine_without_a_known_gpu_gets_no_vendor_advice() {
        assert_eq!(
            missing_driver(&[], &manifests(&[]), PackageManager::Pacman),
            None
        );
        // An unknown vendor carries no package name — the generic remedy covers it.
        assert_eq!(
            missing_driver(
                &[],
                &manifests(&["radeon_icd.json"]),
                PackageManager::Pacman
            ),
            None
        );
    }

    #[test]
    fn only_display_controller_classes_count_as_gpus() {
        assert_eq!(0x030000u32 >> 16, 0x03); // VGA controller
        assert_eq!(0x030200u32 >> 16, 0x03); // 3D controller
        assert_eq!(0x040300u32 >> 16, 0x04); // HD audio — not a display controller
    }
}
