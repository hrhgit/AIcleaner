pub(crate) struct EffectiveThinking<'a> {
    pub enabled: bool,
    pub level: &'a str,
}

fn normalize_level<'a>(level: &'a str) -> &'a str {
    let trimmed = level.trim();
    if trimmed.is_empty() { "medium" } else { trimmed }
}

pub(crate) fn advisor_turn(level: &str) -> EffectiveThinking<'_> {
    EffectiveThinking {
        enabled: true,
        level: normalize_level(level),
    }
}

pub(crate) fn advisor_summary_tool(level: &str) -> EffectiveThinking<'_> {
    let _ = level;
    EffectiveThinking {
        enabled: false,
        level: "medium",
    }
}

pub(crate) fn organizer_stage<'a>(stage: &str, level: &'a str) -> EffectiveThinking<'a> {
    let enabled = matches!(stage, "reconcile_tree" | "adjust_tree");
    EffectiveThinking {
        enabled,
        level: if enabled { normalize_level(level) } else { "medium" },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisor_turn_always_enables_reasoning() {
        let thinking = advisor_turn("high");
        assert!(thinking.enabled);
        assert_eq!(thinking.level, "high");
    }

    #[test]
    fn advisor_summary_tool_always_disables_reasoning() {
        let thinking = advisor_summary_tool("high");
        assert!(!thinking.enabled);
        assert_eq!(thinking.level, "medium");
    }

    #[test]
    fn organizer_stage_only_enables_reasoning_for_final_adjustment_stages() {
        assert!(!organizer_stage("initial_tree", "high").enabled);
        assert!(!organizer_stage("summary_batch", "high").enabled);
        assert!(!organizer_stage("classification_batch_1", "high").enabled);
        assert!(!organizer_stage("local_refine_subtree", "high").enabled);
        assert!(organizer_stage("reconcile_tree", "high").enabled);
        assert!(organizer_stage("adjust_tree", "low").enabled);
    }
}
