//! Raccourci du Bureau qui ouvre la fenêtre de DJ-Bridge d'un double-clic.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Nom de l'exécutable de la fenêtre, à la racine du dossier du projet.
const ROOT_EXE: &str = "DJ-Bridge.exe";
const GUI_EXE: &str = "dj-bridge-gui.exe";

pub fn run(target: Option<PathBuf>) -> Result<()> {
    let target = match target {
        Some(target) => target,
        None => default_target()?,
    };
    let target = target.canonicalize().with_context(|| format!("{} introuvable", target.display()))?;
    let desktop = dirs::desktop_dir().context("Bureau introuvable")?;
    let link = desktop.join("DJ-Bridge.lnk");
    create(&link, &target)?;
    println!("Raccourci créé : {} → {}", link.display(), display(&target));
    Ok(())
}

/// DJ-Bridge.exe du dossier courant, sinon l'exécutable de la fenêtre à côté de celui-ci.
fn default_target() -> Result<PathBuf> {
    let root = std::env::current_dir()?.join(ROOT_EXE);
    if root.exists() {
        return Ok(root);
    }
    let sibling = std::env::current_exe()?.with_file_name(GUI_EXE);
    if sibling.exists() {
        return Ok(sibling);
    }
    bail!("ni {ROOT_EXE} ici, ni {GUI_EXE} à côté de cette commande : compile d'abord avec `cargo build --release`")
}

/// Raccourci Windows (.lnk) vers `target`, avec son icône, lancé depuis son dossier.
fn create(link: &Path, target: &Path) -> Result<()> {
    // Chemins passés par l'environnement : rien à échapper dans le script.
    let script = "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:DJB_LINK); \
                  $s.TargetPath = $env:DJB_TARGET; $s.WorkingDirectory = $env:DJB_DIR; \
                  $s.IconLocation = \"$env:DJB_TARGET,0\"; $s.Description = 'DJ-Bridge : lumières Hue en live'; $s.Save()";
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("DJB_LINK", link)
        .env("DJB_TARGET", display(target))
        .env("DJB_DIR", target.parent().map(display).unwrap_or_default())
        .status()
        .context("PowerShell introuvable")?;
    if !status.success() {
        bail!("le raccourci n'a pas pu être créé ({status})");
    }
    Ok(())
}

/// Chemin sans le préfixe `\\?\` que laisse `canonicalize` sous Windows.
fn display(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(text)
}
