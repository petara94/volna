use std::process::Command;

fn output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .replace('\n', " | ")
    })
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.lock");
    let rustc = std::env::var("RUSTC")
        .ok()
        .and_then(|p| output(&p, &["-Vv"]))
        .unwrap_or_else(|| "unknown".into());
    let revision = output("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unversioned".into());
    let dirty = if revision == "unversioned" {
        "unversioned".into()
    } else {
        output("git", &["status", "--porcelain"])
            .map(|s| (!s.is_empty()).to_string())
            .unwrap_or_else(|| "unknown".into())
    };
    for (key, value) in [
        ("RUSTC", rustc),
        ("REVISION", revision),
        ("DIRTY", dirty),
        ("TARGET", std::env::var("TARGET").unwrap_or_default()),
        ("PROFILE", std::env::var("PROFILE").unwrap_or_default()),
    ] {
        println!("cargo:rustc-env=VOLNA_BUILD_{key}={value}");
    }
}
