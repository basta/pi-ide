//! The Visual Studio developer environment for clangd on Windows.
//!
//! A project built with MSVC has `cl.exe` command lines in its
//! `compile_commands.json`. clangd follows cl's rules: without `INCLUDE` it
//! finds the VC headers and the Windows SDK by itself, but not ATL/MFC
//! (`atlmfc\include`) and whatever else `vcvars64.bat` adds. Build tools run
//! inside that environment (a Developer Prompt, CLion, `vsenv.ps1`), pide
//! usually doesn't, so we load it once, the same way: vswhere finds Visual
//! Studio, `vcvars64.bat` runs in cmd, and `set` prints the result.

use std::sync::OnceLock;

/// Variables `vcvars64.bat` sets or changes, `PATH` (spelled `Path`) included:
/// builds need its cl, link and ninja. Empty off Windows, when `INCLUDE` is already set (pide started
/// from a Developer Prompt), or when there's no Visual Studio with C++.
/// Blocking the first time (about a second): call off the UI thread.
pub fn dev_env() -> &'static [(String, String)] {
    static ENV: OnceLock<Vec<(String, String)>> = OnceLock::new();
    ENV.get_or_init(|| {
        if !cfg!(windows) || std::env::var_os("INCLUDE").is_some() {
            return vec![];
        }
        match load() {
            Ok(env) => {
                log::info!("msvc: loaded the developer environment ({} variables)", env.len());
                env
            }
            Err(e) => {
                log::info!("msvc: no developer environment: {e:#}");
                vec![]
            }
        }
    })
}

#[cfg(windows)]
fn load() -> anyhow::Result<Vec<(String, String)>> {
    use anyhow::Context as _;
    use std::os::windows::process::CommandExt as _;
    use std::path::PathBuf;
    use std::process::Command;

    const NO_WINDOW: u32 = 0x0800_0000; // CREATE_NO_WINDOW: no console flashing up

    let installer = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"))
        .join(r"Microsoft Visual Studio\Installer\vswhere.exe");
    anyhow::ensure!(installer.is_file(), "{} not found", installer.display());
    let out = Command::new(&installer)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .creation_flags(NO_WINDOW)
        .output()
        .context("running vswhere")?;
    let vs = String::from_utf8_lossy(&out.stdout).trim().to_string();
    anyhow::ensure!(!vs.is_empty(), "no Visual Studio with the C++ x64 tools");
    let vcvars = PathBuf::from(&vs).join(r"VC\Auxiliary\Build\vcvars64.bat");
    anyhow::ensure!(vcvars.is_file(), "{} not found", vcvars.display());

    // `/s /c "<line>"`: cmd strips the outer quotes and runs the line as is.
    let out = Command::new("cmd")
        .raw_arg(format!(
            "/d /s /c \"\"{}\" >nul 2>&1 && set\"",
            vcvars.display()
        ))
        .creation_flags(NO_WINDOW)
        .output()
        .context("running vcvars64.bat")?;
    anyhow::ensure!(out.status.success(), "vcvars64.bat failed");
    let env = changed(&String::from_utf8_lossy(&out.stdout), |k| std::env::var(k).ok());
    anyhow::ensure!(
        env.iter().any(|(k, _)| k.eq_ignore_ascii_case("INCLUDE")),
        "vcvars64.bat set no INCLUDE"
    );
    Ok(env)
}

#[cfg(not(windows))]
fn load() -> anyhow::Result<Vec<(String, String)>> {
    anyhow::bail!("not Windows")
}

/// `set`'s output, keeping what differs from the current environment.
fn changed(set_output: &str, current: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    set_output
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| !k.is_empty())
        .filter(|(k, v)| current(k).as_deref() != Some(*v))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_what_vcvars_changed() {
        let out = "Path=C:\\vc\\bin;C:\\x\nINCLUDE=C:\\vc\\include;C:\\vc\\atlmfc\\include\nUSERNAME=me\nVCToolsVersion=14.44\n";
        let env = changed(out, |k| (k == "USERNAME").then(|| "me".to_string()));
        assert_eq!(
            env,
            [
                ("Path".into(), "C:\\vc\\bin;C:\\x".into()),
                ("INCLUDE".into(), "C:\\vc\\include;C:\\vc\\atlmfc\\include".into()),
                ("VCToolsVersion".into(), "14.44".into()),
            ]
        );
    }

    /// On a machine with Visual Studio, the environment has the toolset's
    /// headers (ATL's only when that toolset has the ATL component).
    #[test]
    fn loads_on_this_machine() {
        let env = dev_env();
        if env.is_empty() {
            return; // not Windows, or no Visual Studio here
        }
        let include = env.iter().find(|(k, _)| k.eq_ignore_ascii_case("INCLUDE")).unwrap();
        let include = include.1.to_ascii_lowercase();
        assert!(include.contains(r"\vc\tools\msvc\"), "{include}");
    }
}
