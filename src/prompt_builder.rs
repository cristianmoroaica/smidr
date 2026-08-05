//! Prompt builder — constructs phase-specific user messages for Claude.
//!
//! Each phase in the pipeline (spec, decompose, component, assembly, refinement)
//! sends Claude a different system prompt and a structured user message.
//! This module handles both: locating the right system prompt file and
//! building the user message for each phase.

/// Locate `prompts/<phase_name>.md`.
///
/// Walks up from cwd and from the binary directory, looking for a
/// `prompts/` directory that contains `<phase_name>.md`.  This mirrors
/// the logic used by `find_system_prompt` in `claude.rs` but accepts any
/// phase filename.
pub fn load_phase_system_prompt(phase_name: &str) -> Result<String, String> {
    let filename = format!("{phase_name}.md");

    let starts: Vec<std::path::PathBuf> = [
        std::env::current_dir().ok(),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf())),
    ]
    .into_iter()
    .flatten()
    .collect();

    for start in &starts {
        let mut dir = start.as_path();
        loop {
            let candidate = dir.join("prompts").join(&filename);
            if candidate.exists() {
                return std::fs::read_to_string(&candidate)
                    .map_err(|e| format!("Failed to read {}: {e}", candidate.display()));
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
    }

    Err(format!(
        "prompts/{filename} not found. Run from within the MiModel project."
    ))
}

/// Load all engineering knowledge files from `prompts/knowledge/`.
///
/// Returns a combined string of all `.md` files in the knowledge directory,
/// suitable for appending to build-phase system prompts.
pub fn load_engineering_knowledge() -> String {
    let starts: Vec<std::path::PathBuf> = [
        std::env::current_dir().ok(),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf())),
    ]
    .into_iter()
    .flatten()
    .collect();

    for start in &starts {
        let mut dir = start.as_path();
        loop {
            let knowledge_dir = dir.join("prompts").join("knowledge");
            if knowledge_dir.is_dir() {
                let mut sections = Vec::new();
                if let Ok(entries) = std::fs::read_dir(&knowledge_dir) {
                    let mut files: Vec<_> = entries
                        .flatten()
                        .filter(|e| {
                            e.path().extension().map_or(false, |ext| ext == "md")
                        })
                        .collect();
                    files.sort_by_key(|e| e.file_name());
                    for entry in files {
                        if let Ok(content) = std::fs::read_to_string(entry.path()) {
                            sections.push(content);
                        }
                    }
                }
                if !sections.is_empty() {
                    return format!(
                        "\n\n---\n\n# Engineering Knowledge Base\n\n{}\n",
                        sections.join("\n\n---\n\n")
                    );
                }
                return String::new();
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_system_prompt() {
        let prompt = load_phase_system_prompt("spec");
        assert!(prompt.is_ok());
        assert!(prompt.unwrap().contains("ask_question"));
    }
}
