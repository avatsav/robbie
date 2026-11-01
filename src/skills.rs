use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use walkdir::WalkDir;

#[derive(Default)]
pub struct SkillRegistry {
    // Sorted names keep the model's prompt stable across runs.
    skills: BTreeMap<String, SkillDocument>,
}

struct SkillDocument {
    description: String,
    directory: PathBuf,
    body: String,
}

#[derive(Default, Deserialize)]
struct Metadata {
    name: Option<String>,
    #[serde(default)]
    description: String,
}

impl SkillRegistry {
    // This is synchronous startup I/O; call it before starting a frontend,
    // on a blocking worker rather than the UI executor.
    pub fn scan(directory: &Path) -> Result<Self> {
        match std::fs::metadata(directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error).context("cannot inspect skills directory"),
            Ok(metadata) if !metadata.is_dir() => bail!("skills path is not a directory: {}", directory.display()),
            Ok(_) => {}
        }
        let directory = directory.canonicalize().context("cannot resolve skills directory")?;
        let mut registry = Self::default();
        // Do not follow symlinks. References/scripts can live beside SKILL.md;
        // only regular SKILL.md files are loaded here.
        for entry in WalkDir::new(directory).sort_by_file_name() {
            let entry = entry.context("cannot scan skills directory")?;
            if !entry.file_type().is_file() || entry.file_name() != "SKILL.md" {
                continue;
            }
            let path = entry.path();
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            let (metadata, body) = parse_document(&text)
                .with_context(|| format!("invalid skill {}", path.display()))?;
            let parent = path.parent().context("skill has no parent directory")?;
            let name = metadata.name.unwrap_or_else(|| {
                parent.file_name().unwrap_or_default().to_string_lossy().into_owned()
            });
            let name = name.trim().to_string();
            if name.is_empty() || name.chars().any(char::is_control) {
                bail!("invalid skill name in {}", path.display());
            }
            if let Some(previous) = registry.skills.get(&name) {
                bail!("duplicate skill {name:?}: {} and {}", previous.directory.display(), parent.display());
            }
            registry.skills.insert(name, SkillDocument {
                description: metadata.description.split_whitespace().collect::<Vec<_>>().join(" "),
                directory: parent.to_path_buf(),
                body,
            });
        }
        Ok(registry)
    }

    pub fn describe_available(&self) -> String {
        self.skills.iter().map(|(name, skill)| {
            let description = if skill.description.is_empty() { "(no description)" } else { &skill.description };
            format!("- {name}: {description}")
        }).collect::<Vec<_>>().join("\n")
    }

    pub fn load(&self, name: &str) -> Result<String> {
        // Exact registry lookup, not a path supplied by the model.
        let skill = self.skills.get(name).with_context(|| format!("unknown skill: {name}"))?;
        Ok(format!(
            "Skill: {name}\nBase directory: {}\nResolve relative references against this directory.\n\n{}",
            skill.directory.display(), skill.body,
        ))
    }
}

fn parse_document(text: &str) -> Result<(Metadata, String)> {
    let text = text.replace("\r\n", "\n");
    if text == "---" {
        bail!("unclosed YAML frontmatter");
    }
    let Some(rest) = text.strip_prefix("---\n") else {
        return Ok((Metadata::default(), text));
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches('\n') == "---" {
            let yaml = &rest[..offset];
            let metadata = if yaml.trim().is_empty() {
                Metadata::default()
            } else {
                serde_yaml_ng::from_str(yaml).context("invalid YAML frontmatter")?
            };
            return Ok((metadata, rest[offset + line.len()..].to_string()));
        }
        offset += line.len();
    }
    bail!("unclosed YAML frontmatter")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, folder: &str, text: &str) {
        let directory = root.join(folder);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("SKILL.md"), text).unwrap();
    }

    #[test]
    fn parses_yaml_lf_crlf_and_optional_frontmatter() {
        for newline in ["\n", "\r\n"] {
            let text = "---\nname: review\ndescription: >-\n  Review changes\n  for correctness.\n---\n# Checklist\nKeep --- in the body.\n".replace('\n', newline);
            let (metadata, body) = parse_document(&text).unwrap();
            assert_eq!(metadata.name.as_deref(), Some("review"));
            assert_eq!(metadata.description, "Review changes for correctness.");
            assert_eq!(body, "# Checklist\nKeep --- in the body.\n");
        }
        assert_eq!(parse_document("# Plain skill").unwrap().1, "# Plain skill");
        assert_eq!(parse_document("---\n---\nbody").unwrap().1, "body");
        assert_eq!(parse_document("---\nname: empty\n---").unwrap().1, "");
    }

    #[test]
    fn rejects_unclosed_or_invalid_metadata() {
        for text in ["---", "---\nname: missing", "---\nname: [bad\n---\nbody", "---\nname: [not, a, string]\n---\nbody"] {
            assert!(parse_document(text).is_err(), "{text}");
        }
    }

    #[test]
    fn discovers_nested_skills_and_separates_summaries_from_bodies() {
        let root = tempfile::tempdir().unwrap();
        write_skill(root.path(), "z/fallback", "Secret fallback instructions.");
        write_skill(root.path(), "a", "---\nname: named\ndescription: Explain ownership.\n---\nSecret named instructions.");
        std::fs::write(root.path().join("README.md"), "not a skill").unwrap();
        let registry = SkillRegistry::scan(root.path()).unwrap();
        assert_eq!(registry.describe_available(), "- fallback: (no description)\n- named: Explain ownership.");
        let loaded = registry.load("named").unwrap();
        assert!(loaded.contains("Secret named instructions."));
        assert!(loaded.contains(&root.path().canonicalize().unwrap().join("a").display().to_string()));
        assert!(!loaded.contains("description:"));
        assert!(registry.load("../a/SKILL.md").is_err());
        // The registry is a startup snapshot, not a live file read.
        std::fs::remove_file(root.path().join("a/SKILL.md")).unwrap();
        assert_eq!(registry.load("named").unwrap(), loaded);
    }

    #[test]
    fn missing_directory_is_empty_but_bad_paths_and_duplicates_fail() {
        let root = tempfile::tempdir().unwrap();
        assert!(SkillRegistry::scan(&root.path().join("absent")).unwrap().describe_available().is_empty());
        write_skill(root.path(), "first", "---\nname: duplicate\n---\nfirst");
        assert!(SkillRegistry::scan(&root.path().join("first/SKILL.md")).is_err());
        write_skill(root.path(), "second", "---\nname: duplicate\n---\nsecond");
        let error = SkillRegistry::scan(root.path()).err().unwrap().to_string();
        assert!(error.contains("duplicate skill"));
        assert!(error.contains("first") && error.contains("second"));
    }

    #[test]
    fn invalid_skill_reports_its_path() {
        let root = tempfile::tempdir().unwrap();
        for text in ["---\nname: [bad\n---\n", "---\nname: ' '\n---\n"] {
            write_skill(root.path(), "broken", text);
            let error = SkillRegistry::scan(root.path()).err().unwrap().to_string();
            assert!(error.contains("broken/SKILL.md"), "{error}");
        }
    }
}
