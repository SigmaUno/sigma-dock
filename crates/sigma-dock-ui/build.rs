use std::{env, fs, process::Command};
fn main() {
    for name in ["GITHUB_SHA", "GITHUB_REF", "SIGMA_DOCK_BUILD_CHANNEL"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads/main");
    println!("cargo:rerun-if-changed=.cargo_vcs_info.json");
    let commit = env::var("GITHUB_SHA")
        .ok()
        .or_else(|| {
            fs::read(".cargo_vcs_info.json")
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|value| value["git"]["sha1"].as_str().map(str::to_owned))
        })
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        })
        .filter(|value| value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into());
    let channel = env::var("SIGMA_DOCK_BUILD_CHANNEL")
        .ok()
        .filter(|value| matches!(value.as_str(), "stable" | "preview" | "snapshot"))
        .unwrap_or_else(|| {
            if env::var("GITHUB_REF")
                .unwrap_or_default()
                .starts_with("refs/tags/v")
            {
                if env::var("CARGO_PKG_VERSION")
                    .unwrap_or_default()
                    .contains('-')
                {
                    "preview"
                } else {
                    "stable"
                }
            } else {
                "snapshot"
            }
            .into()
        });
    println!("cargo:rustc-env=SIGMA_DOCK_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=SIGMA_DOCK_BUILD_CHANNEL={channel}");
}
