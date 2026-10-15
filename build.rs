fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Deployment fingerprint: embed the git SHA so the running binary can be
    // matched against a commit without guessing from behaviour.
    // Priority: explicit build arg (Docker builds have no .git) -> local git -> unknown.
    println!("cargo:rerun-if-env-changed=GITLAB_RS_GIT_SHA");
    let git_sha = std::env::var("GITLAB_RS_GIT_SHA")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(local_git_sha)
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GITLAB_RS_GIT_SHA={git_sha}");

    tonic_build::configure()
        .build_server(false)
        .compile_protos(
            &[
                "proto/gitaly/smarthttp.proto",
                "proto/gitaly/repository.proto",
                "proto/gitaly/blob.proto",
                "proto/gitaly/diff.proto",
                "proto/gitaly/commit.proto",
                "proto/gitaly/ref.proto",
            ],
            &["proto"],
        )?;
    Ok(())
}

fn local_git_sha() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!sha.is_empty()).then_some(sha)
}
