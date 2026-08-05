//! Prompt builder — constructs phase-specific user messages for Claude.
//!
//! Each phase in the pipeline (spec, decompose, component, assembly, refinement)
//! sends Claude a different system prompt and a structured user message.
//! The prompt files are embedded into the binary via rust-embed (the
//! `debug-embed` feature makes debug builds embed too), so the installed
//! `smidr` works from any working directory.

/// The `prompts/` directory, embedded at compile time: `<phase>.md` system
/// prompts at the top level plus `knowledge/*.md` reference material.
#[derive(rust_embed::RustEmbed)]
#[folder = "prompts"]
struct Prompts;

/// Load the embedded `prompts/<phase_name>.md` system prompt.
pub fn load_phase_system_prompt(phase_name: &str) -> Result<String, String> {
    let filename = format!("{phase_name}.md");
    let file = Prompts::get(&filename)
        .ok_or_else(|| format!("embedded prompt {filename} missing — rebuild smidr"))?;
    String::from_utf8(file.data.into_owned())
        .map_err(|e| format!("embedded prompt {filename} is not valid UTF-8: {e}"))
}

/// Combined engineering knowledge from the embedded `prompts/knowledge/*.md`
/// files (sorted by filename), suitable for appending to build-phase system
/// prompts. Empty string if there are none.
pub fn load_engineering_knowledge() -> String {
    let mut names: Vec<_> = Prompts::iter()
        .filter(|p| p.starts_with("knowledge/") && p.ends_with(".md"))
        .collect();
    names.sort();

    let sections: Vec<String> = names
        .iter()
        .filter_map(|n| Prompts::get(n))
        .filter_map(|f| String::from_utf8(f.data.into_owned()).ok())
        .collect();

    if sections.is_empty() {
        return String::new();
    }
    format!(
        "\n\n---\n\n# Engineering Knowledge Base\n\n{}\n",
        sections.join("\n\n---\n\n")
    )
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

    #[test]
    fn test_knowledge_is_embedded() {
        let knowledge = load_engineering_knowledge();
        assert!(knowledge.contains("Engineering Knowledge Base"));
    }
}
