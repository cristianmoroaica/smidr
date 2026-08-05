//! Spec preview panel — displays the evolving spec.toml content during the Spec phase.

pub struct SpecPanel {
    content: String,
}

impl SpecPanel {
    pub fn new() -> Self {
        Self { content: String::new() }
    }

    pub fn set_content(&mut self, content: &str) {
        self.content = content.to_string();
    }

    pub fn content(&self) -> &str {
        &self.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_empty() {
        let panel = SpecPanel::new();
        assert!(panel.content().is_empty());
    }

    #[test]
    fn test_set_and_get_content() {
        let mut panel = SpecPanel::new();
        panel.set_content("[model]\nname = \"Test\"");
        assert_eq!(panel.content(), "[model]\nname = \"Test\"");
    }
}
