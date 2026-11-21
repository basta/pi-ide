//! `FileImage`: an image from the panel's folder or its project.
//!
//! gpui-shell's own `Image` reads the host's assets, which in pide are pide's
//! bundled icons, so a panel couldn't show its own picture or a figure from
//! the project. `FileImage("chart.png")` looks in the panel's folder first,
//! then the project; it never leaves either.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use gpui_shell::gpui::{self, IntoElement as _, Refineable as _, Styled as _};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, RegistryError,
};

/// Panel folder → project root, for every panel that has loaded.
static ROOTS: LazyLock<Mutex<HashMap<PathBuf, PathBuf>>> = LazyLock::new(Default::default);

/// Called as a panel loads, so its images can find the project.
pub fn remember(dir: &Path, project: &Path) {
    ROOTS
        .lock()
        .unwrap()
        .insert(dir.to_path_buf(), project.to_path_buf());
}

/// The panel whose script is running: the folder in its read grant.
fn running_panel() -> Option<(PathBuf, PathBuf)> {
    let caps = format!("{:?}", gpui_shell::current_capabilities());
    let roots = ROOTS.lock().unwrap();
    roots
        .iter()
        .filter(|(dir, _)| caps.contains(&format!("{dir:?}")))
        .max_by_key(|(dir, _)| dir.as_os_str().len())
        .map(|(d, p)| (d.clone(), p.clone()))
}

/// The file `path` names, in the panel folder or else the project.
pub fn resolve(path: &str, dir: &Path, project: &Path) -> Result<PathBuf, String> {
    let rel = Path::new(path.trim());
    if path.trim().is_empty() || path.contains("://") || path.starts_with("data:") {
        return Err("FileImage takes a file in the panel's folder or the project".into());
    }
    if rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("FileImage: {path:?} leaves the project"));
    }
    let candidates: Vec<PathBuf> = if rel.is_absolute() {
        vec![rel.to_path_buf()]
    } else {
        vec![dir.join(rel), project.join(rel)]
    };
    for file in candidates {
        let Ok(real) = file.canonicalize() else {
            continue;
        };
        let inside = [dir, project]
            .iter()
            .filter_map(|r| r.canonicalize().ok())
            .any(|r| real.starts_with(r));
        if inside && real.is_file() {
            return Ok(real);
        }
    }
    Err(format!(
        "FileImage: no file {path:?} in the panel's folder or the project"
    ))
}

#[derive(Clone)]
struct Source(PathBuf);

struct Materializer;
impl ComponentMaterializer for Materializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let source = request
            .payload()
            .downcast_ref::<Source>()
            .ok_or_else(|| anyhow::anyhow!("FileImage received an incompatible payload"))?
            .clone();
        let mut image = gpui::img(source.0);
        image.style().refine(&request.take_style());
        Ok(image.into_any_element())
    }
}

pub fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new("FileImage", Arc::new(Materializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                "FileImage",
                vec![ArgumentDescriptor::new("path", ArgumentSchema::String)],
                |args| match args {
                    [ComponentArgument::String(path)] => {
                        let (dir, project) = running_panel()
                            .ok_or("FileImage works only inside a pide script panel")?;
                        resolve(path, &dir, &project)
                            .map(Source)
                            .map(ComponentPayload::new)
                    }
                    _ => Err("FileImage expects one path".into()),
                },
            )])
            .with_methods(vec![])
            .with_documentation(
                "pide: an image file (PNG, JPEG, GIF, WebP, SVG…) from the panel's folder, else the project. Relative paths, or absolute ones inside either. Size it with w/h; style like a div.",
            ),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn finds_images_in_the_panel_then_the_project_and_nowhere_else() {
        let base = std::env::temp_dir().join(format!("pide-img-{}", std::process::id()));
        let project = base.join("proj");
        let dir = project.join(".pide/panels/p");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(project.join("figs")).unwrap();
        std::fs::write(dir.join("logo.png"), b"x").unwrap();
        std::fs::write(project.join("logo.png"), b"y").unwrap();
        std::fs::write(project.join("figs/plot.png"), b"z").unwrap();
        std::fs::write(base.join("secret.png"), b"s").unwrap();

        let got = |p: &str| resolve(p, &dir, &project);
        assert!(
            got("logo.png")
                .unwrap()
                .ends_with(".pide/panels/p/logo.png")
        );
        assert!(
            got("figs/plot.png")
                .unwrap()
                .ends_with("proj/figs/plot.png")
        );
        assert!(got("../../../../secret.png").is_err());
        assert!(got(base.join("secret.png").to_str().unwrap()).is_err());
        assert!(got("https://example.com/a.png").is_err());
        assert!(got("missing.png").is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
