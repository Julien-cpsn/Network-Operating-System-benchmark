use std::env;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::{exit, Command};
use dotenv::dotenv;
use tracing::{error, info, warn};
use crate::{GUEST_IMAGE_PATH, QEMU_BINARY, QEMU_ENABLE_KVM, QEMU_IMG_BINARY, QEMU_WORK_DIR};
use crate::utils::qemu::network::{select_link_backend, VhostMode};

const TARGET: &str = "env";

pub fn harvest_env_variables() {
    dotenv().ok();

    let current_dir = env::current_dir().expect("Could not read current directory");

    for (key, value) in env::vars() {
        match key.as_str() {
            "QEMU_BINARY" => { QEMU_BINARY.get_or_init(|| value); },
            "QEMU_IMG_BINARY" => { QEMU_IMG_BINARY.get_or_init(|| value); },
            // Absolute: QEMU changes its working directory to "/" when it daemonizes
            "QEMU_WORK_DIR" => { QEMU_WORK_DIR.get_or_init(|| current_dir.join(value)); },
            "QEMU_ENABLE_KVM" => {
                let enable = match value.to_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => true,
                    "0" | "false" | "no" | "off" => false,
                    _ => {
                        error!(target: TARGET, "Environment variable QEMU_ENABLE_KVM must be true or false, got \"{value}\"");
                        exit(1);
                    }
                };
                QEMU_ENABLE_KVM.get_or_init(|| enable);
            },
            "GUEST_IMAGE_PATH" => { GUEST_IMAGE_PATH.get_or_init(|| current_dir.join(value)); },
            _ => {}
        }
    }

    // Defaults
    QEMU_BINARY.get_or_init(|| String::from("qemu-system-x86_64"));
    QEMU_IMG_BINARY.get_or_init(|| String::from("qemu-img"));
    QEMU_WORK_DIR.get_or_init(|| current_dir.join("experimentation").join("vms"));
    QEMU_ENABLE_KVM.get_or_init(|| true);

    if let Some(guest_image_path) = GUEST_IMAGE_PATH.get() {
        if !guest_image_path.is_file() {
            error!(target: TARGET, "Environment variable GUEST_IMAGE_PATH (\"{}\") is not a file", guest_image_path.display());
            exit(1);
        }
    }
    else {
        error!(target: TARGET, "Environment variable GUEST_IMAGE_PATH not set");
        exit(1);
    }

    for binary in [QEMU_BINARY.get().unwrap(), QEMU_IMG_BINARY.get().unwrap()] {
        match Command::new(binary).arg("--version").output() {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout);
                info!(target: TARGET, "{}", version.lines().next().unwrap_or_default());
            },
            _ => {
                error!(target: TARGET, "Could not run \"{binary} --version\", is QEMU installed?");
                exit(1);
            }
        }
    }

    if *QEMU_ENABLE_KVM.get().unwrap() {
        if let Err(err) = OpenOptions::new().read(true).write(true).open("/dev/kvm") {
            error!(target: TARGET, "KVM is not usable (/dev/kvm: {err}). Fix its permissions, or set QEMU_ENABLE_KVM=false to use (much slower) emulation");
            exit(1);
        }
    }
    else {
        info!(target: TARGET, "KVM disabled, VMs are emulated (TCG) and routers use \"-cpu max\" instead of \"-cpu host\"");
    }

    let vhost_mode = match env::var("QEMU_VHOST") {
        Ok(value) => VhostMode::parse(&value).unwrap_or_else(|err| {
            warn!(target: TARGET, "{err}");
            exit(1);
        }),
        Err(_) => VhostMode::Auto,
    };

    if let Err(err) = select_link_backend(vhost_mode) {
        error!(target: TARGET, "{err}");
        exit(1);
    }
}

pub fn work_dir() -> PathBuf {
    QEMU_WORK_DIR.get().unwrap().clone()
}
